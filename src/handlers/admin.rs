use axum::Json;
use axum::extract::{Form, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, EXPIRES, LOCATION, PRAGMA, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use tokio::fs;
use tracing::{info, trace, warn};

use crate::state::{AppState, BanSubject, IpBan};
use crate::util::{ADMIN_SESSION_TTL, IpVersion, get_cookie, json_error, new_id, now_secs};

fn is_https(headers: &HeaderMap) -> bool {
    if let Some(v) = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
    {
        if v.split(',')
            .next()
            .map(|s| s.trim().eq_ignore_ascii_case("https"))
            .unwrap_or(false)
        {
            return true;
        }
    }
    if let Some(v) = headers
        .get(axum::http::header::FORWARDED)
        .and_then(|v| v.to_str().ok())
    {
        let lower = v.to_ascii_lowercase();
        if lower.contains("proto=https") {
            return true;
        }
    }
    if let Some(v) = headers.get("cf-visitor").and_then(|v| v.to_str().ok()) {
        let lower = v.to_ascii_lowercase();
        if lower.contains("\"scheme\":\"https\"") || lower.contains("https") {
            return true;
        }
    }
    false
}

#[derive(Deserialize)]
pub struct BanForm {
    pub ip: String,
    pub reason: Option<String>,
}

#[derive(Deserialize)]
pub struct UnbanForm {
    pub key: String,
}

#[derive(Deserialize)]
pub struct AdminAuthForm {
    pub key: String,
}

#[derive(Deserialize)]
pub struct AdminFileDeleteForm {
    pub file: String,
}

#[derive(Deserialize)]
pub struct AdminReportDeleteForm {
    pub idx: usize,
}

pub async fn ban_page_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    trace!("rendering ban page");
    if let Some(tok) = get_cookie(&headers, "adm") {
        if !state.is_admin(&tok).await {
            warn!("ban page access denied: invalid admin session");
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
        warn!("ban page access denied: missing admin session");
        return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
    }
    let bans = state.bans.read().await.clone();
    let rows: String = bans
        .iter()
        .map(|b| {
            let subject_label = describe_ban_subject(b);
            let subject_key = b.subject.key();
            let reason_enc = htmlescape::encode_minimal(&b.reason);
            let subject_enc = htmlescape::encode_minimal(&subject_label);
            let key_enc = htmlescape::encode_minimal(subject_key);
            format!("<tr><td>{}</td><td>{}</td><td>{}</td><td><form method=post action=/unban style=margin:0><input type=hidden name=key value=\"{}\"><button type=submit class=del aria-label=\"Unban {}\">Unban</button></form></td></tr>", subject_enc, reason_enc, b.time, key_enc, subject_enc)
        })
        .collect();
    let path = state.static_dir.join("admin_ban.html");
    match fs::read(&path).await {
        Ok(bytes) => {
            let mut body = String::from_utf8_lossy(&bytes).into_owned();
            body = body.replace("{{ROWS}}", &rows);
            (
                StatusCode::OK,
                [(CONTENT_TYPE, HeaderValue::from_static("text/html"))],
                body,
            )
                .into_response()
        }
        Err(_) => json_error(
            StatusCode::NOT_FOUND,
            "missing_template",
            "ban template missing",
        ),
    }
}

#[axum::debug_handler]
pub async fn ban_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(frm): Form<BanForm>,
) -> Response {
    trace!(target = %frm.ip, "processing ban submission");
    if let Some(tok) = get_cookie(&headers, "adm") {
        if !state.is_admin(&tok).await {
            warn!("ban submission rejected: invalid admin session");
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
        warn!("ban submission rejected: missing admin session");
        return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
    }
    let input = frm.ip.trim();
    if input.is_empty() {
        warn!("ban submission rejected: no target provided");
        return json_error(StatusCode::BAD_REQUEST, "missing", "missing ban target");
    }
    let Some(subject) = state.ban_subject_from_input(input) else {
        warn!(target = input, "ban submission rejected: invalid target");
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid",
            "unable to interpret target",
        );
    };
    let reason_trimmed = frm.reason.as_deref().map(str::trim).unwrap_or_default();
    let reason = reason_trimmed.to_string();
    let ban = IpBan {
        subject,
        label: Some(input.to_string()),
        reason,
        time: 0,
    };
    state.add_ban(ban).await;
    state.persist_bans().await;
    info!(target = input, reason = reason_trimmed, "ban added");
    (
        StatusCode::SEE_OTHER,
        [(LOCATION, HeaderValue::from_static("/admin/ban"))],
    )
        .into_response()
}

#[axum::debug_handler]
pub async fn unban_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(frm): Form<UnbanForm>,
) -> Response {
    trace!(key = %frm.key, "processing unban submission");
    if let Some(tok) = get_cookie(&headers, "adm") {
        if !state.is_admin(&tok).await {
            warn!("unban rejected: invalid admin session");
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
        warn!("unban rejected: missing admin session");
        return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
    }
    let key = frm.key.trim();
    if key.is_empty() {
        warn!("unban rejected: empty key");
        return json_error(StatusCode::BAD_REQUEST, "missing", "missing ban key");
    }
    state.remove_ban(key).await;
    state.persist_bans().await;
    info!(ban_key = key, "ban removed");
    (
        StatusCode::SEE_OTHER,
        [(LOCATION, HeaderValue::from_static("/admin/ban"))],
    )
        .into_response()
}

pub async fn auth_get_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    trace!("serving admin auth page");
    if let Some(tok) = get_cookie(&headers, "adm") {
        if state.is_admin(&tok).await {
            let already_path = state.static_dir.join("admin_already.html");
            if let Ok(bytes) = fs::read(&already_path).await {
                return (
                    StatusCode::OK,
                    [(CONTENT_TYPE, HeaderValue::from_static("text/html"))],
                    bytes,
                )
                    .into_response();
            }
            return (
                StatusCode::OK,
                [(CONTENT_TYPE, HeaderValue::from_static("text/html"))],
                "<html><body><p>Already admin.</p><a href=/isadmin>Check</a></body></html>",
            )
                .into_response();
        }
    }
    let tpl_path = state.static_dir.join("admin_auth.html");
    match fs::read(&tpl_path).await {
        Ok(bytes) => (
            StatusCode::OK,
            [(CONTENT_TYPE, HeaderValue::from_static("text/html"))],
            bytes,
        )
            .into_response(),
        Err(_) => (
            StatusCode::OK,
            [(CONTENT_TYPE, HeaderValue::from_static("text/html"))],
            "<html><body><form method=post action=/auth><input type=password name=key autofocus placeholder=Admin+Key><button type=submit>Auth</button></form></body></html>",
        )
            .into_response(),
    }
}

pub async fn auth_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(frm): Form<AdminAuthForm>,
) -> Response {
    // Legacy redirect-based flow removed. Delegate to JSON-based handler to unify logic.
    auth_post_json_handler(State(state), headers, Form(frm)).await
}

pub async fn auth_post_json_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(frm): Form<AdminAuthForm>,
) -> Response {
    let submitted = frm.key.trim();
    if submitted.is_empty() {
        warn!("admin auth (json) rejected: empty key");
        return json_error(StatusCode::BAD_REQUEST, "missing", "missing key");
    }
    let current_key = { state.admin_key.read().await.clone() };
    if current_key.is_empty() {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "no_key",
            "admin key unavailable",
        );
    }
    if subtle_equals(submitted.as_bytes(), current_key.as_bytes()) {
        let token = new_id();
        state.create_admin_session(token.clone()).await;
        state.persist_admin_sessions().await;
        let mut cookie = format!(
            "adm={}; Path=/; HttpOnly; Max-Age={}; SameSite=Strict",
            token, ADMIN_SESSION_TTL
        );
        if is_https(&headers) {
            cookie.push_str("; Secure");
        } else if state.production {
            warn!("admin auth json over non-HTTPS in production; not setting Secure flag");
        }
        // Build 200 response with Set-Cookie (avoid redirect caching issues)
        let mut resp =
            (StatusCode::OK, Json(json!({"admin": true, "token": token}))).into_response();
        {
            let h = resp.headers_mut();
            h.insert(
                CACHE_CONTROL,
                HeaderValue::from_static("no-store, no-cache, must-revalidate, private"),
            );
            h.insert(PRAGMA, HeaderValue::from_static("no-cache"));
            h.insert(EXPIRES, HeaderValue::from_static("0"));
            h.append(SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
        }
        info!("admin auth success (json)");
        return resp;
    }
    warn!("admin auth (json) failed: invalid key");
    json_error(StatusCode::UNAUTHORIZED, "invalid_key", "invalid key")
}

pub async fn is_admin_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    trace!("checking admin session status");
    if let Some(tok) = get_cookie(&headers, "adm") {
        if state.is_admin(&tok).await {
            return (StatusCode::OK, Json(json!({"admin": true}))).into_response();
        }
    }
    (StatusCode::OK, Json(json!({"admin": false}))).into_response()
}

pub async fn admin_files_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    trace!("rendering admin files view");
    if let Some(tok) = get_cookie(&headers, "adm") {
        if !state.is_admin(&tok).await {
            warn!("admin files access denied: invalid session");
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
        warn!("admin files access denied: missing session");
        return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
    }
    let mut rows = String::new();
    let now = now_secs();
    for entry in state.owners.iter() {
        let file = entry.key();
        let meta = entry.value();
        let path = state.upload_dir.join(file);
        let size = match fs::metadata(&path).await {
            Ok(md) => md.len(),
            Err(_) => 0,
        };
        let remain = if meta.expires > now {
            meta.expires - now
        } else {
            0
        };
        let human = if remain >= 86400 {
            format!("{}d", remain / 86400)
        } else if remain >= 3600 {
            format!("{}h", remain / 3600)
        } else if remain >= 60 {
            format!("{}m", remain / 60)
        } else {
            format!("{}s", remain)
        };
        let file_href = format!("/f/{}", urlencoding::encode(file));
        let file_label = htmlescape::encode_minimal(file);
        let owner_label = htmlescape::encode_minimal(&short_hash(&meta.owner_hash));
        let file_attr = htmlescape::encode_minimal(file);
        rows.push_str(&format!("<tr><td><a href=\"{href}\" target=_blank rel=noopener>{label}</a></td><td>{owner}</td><td data-exp=\"{exp}\">{human}</td><td>{size}</td><td><form method=post action=/admin/files style=margin:0><input type=hidden name=file value=\"{file_attr}\"><button type=submit class=del data-file=\"{file_attr}\">Delete</button></form></td></tr>",
            href = file_href,
            label = file_label,
            owner = owner_label,
            exp = meta.expires,
            human = human,
            size = size,
            file_attr = file_attr,
        ));
    }
    let tpl_path = state.static_dir.join("admin_files.html");
    match fs::read(&tpl_path).await {
        Ok(bytes) => {
            let mut body = String::from_utf8_lossy(&bytes).into_owned();
            body = body.replace("{{FILE_ROWS}}", &rows);
            (
                StatusCode::OK,
                [(CONTENT_TYPE, HeaderValue::from_static("text/html"))],
                body,
            )
                .into_response()
        }
        Err(_) => json_error(
            StatusCode::NOT_FOUND,
            "missing_template",
            "admin files template missing",
        ),
    }
}

#[axum::debug_handler]
pub async fn admin_file_delete_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(frm): Form<AdminFileDeleteForm>,
) -> Response {
    trace!(file = %frm.file, "admin file delete requested");
    if let Some(tok) = get_cookie(&headers, "adm") {
        if !state.is_admin(&tok).await {
            warn!(file = %frm.file, "admin file delete rejected: invalid session");
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
        warn!(file = %frm.file, "admin file delete rejected: missing session");
        return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
    }
    let file = frm.file.trim();
    if file.is_empty() || file.contains('/') || file.contains('\\') {
        warn!(file, "admin file delete rejected: invalid name");
        return json_error(StatusCode::BAD_REQUEST, "bad_file", "invalid file");
    }
    state.owners.remove(file);
    let _ = fs::remove_file(state.upload_dir.join(file)).await;
    state.persist_owners().await;
    info!(file, "admin deleted file");
    (
        StatusCode::SEE_OTHER,
        [(LOCATION, HeaderValue::from_static("/admin/files"))],
    )
        .into_response()
}

pub async fn admin_reports_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    trace!("rendering admin reports view");
    if let Some(tok) = get_cookie(&headers, "adm") {
        if !state.is_admin(&tok).await {
            warn!("admin reports access denied: invalid session");
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
        warn!("admin reports access denied: missing session");
        return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
    }
    let reports = state.reports.read().await.clone();
    let mut rows = String::new();
    for (idx, r) in reports.iter().enumerate() {
        rows.push_str(&format!("<tr><td><a href=\"/{file}\" target=_blank rel=noopener>{file}</a></td><td>{reason}</td><td>{details}</td><td>{reporter}</td><td>{time}</td><td><form method=post action=/admin/reports style=margin:0><input type=hidden name=idx value=\"{idx}\"><button type=submit class=del data-idx=\"{idx}\">Remove</button></form></td></tr>",
            file=htmlescape::encode_minimal(&r.file),
            reason=htmlescape::encode_minimal(&r.reason),
            details=htmlescape::encode_minimal(&r.details),
            reporter=htmlescape::encode_minimal(&short_hash(&r.reporter_hash)),
            time=r.time,
            idx=idx));
    }
    let tpl_path = state.static_dir.join("admin_reports.html");
    match fs::read(&tpl_path).await {
        Ok(bytes) => {
            let mut body = String::from_utf8_lossy(&bytes).into_owned();
            body = body.replace("{{REPORT_ROWS}}", &rows);
            (
                StatusCode::OK,
                [(CONTENT_TYPE, HeaderValue::from_static("text/html"))],
                body,
            )
                .into_response()
        }
        Err(_) => json_error(
            StatusCode::NOT_FOUND,
            "missing_template",
            "admin reports template missing",
        ),
    }
}

#[axum::debug_handler]
pub async fn admin_report_delete_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(frm): Form<AdminReportDeleteForm>,
) -> Response {
    trace!(index = frm.idx, "admin report delete requested");
    if let Some(tok) = get_cookie(&headers, "adm") {
        if !state.is_admin(&tok).await {
            warn!(
                idx = frm.idx,
                "admin report delete rejected: invalid session"
            );
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
        warn!(
            idx = frm.idx,
            "admin report delete rejected: missing session"
        );
        return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
    }
    let idx = frm.idx;
    {
        let mut reports = state.reports.write().await;
        if idx < reports.len() {
            reports.remove(idx);
            info!(idx, "admin removed report");
        }
    }
    state.persist_reports().await;
    (
        StatusCode::SEE_OTHER,
        [(LOCATION, HeaderValue::from_static("/admin/reports"))],
    )
        .into_response()
}

fn subtle_equals(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn short_hash(value: &str) -> String {
    if value.len() <= 12 {
        return value.to_string();
    }
    format!("{}…", &value[..12])
}

fn describe_ban_subject(ban: &IpBan) -> String {
    if let Some(label) = ban.label.as_ref().filter(|l| !l.trim().is_empty()) {
        return label.trim().to_string();
    }
    describe_subject(&ban.subject)
}

fn describe_subject(subject: &BanSubject) -> String {
    match subject {
        BanSubject::Exact { hash } => format!("Hash {}", short_hash(hash)),
        BanSubject::Network {
            hash,
            prefix,
            version,
        } => format!(
            "Net/{}/{} {}",
            prefix,
            version_label(*version),
            short_hash(hash)
        ),
    }
}

fn version_label(version: IpVersion) -> &'static str {
    match version {
        IpVersion::V4 => "v4",
        IpVersion::V6 => "v6",
    }
}

#[cfg(test)]
mod tests {
    use super::is_https;
    use axum::http::HeaderMap;
    use axum::http::header::{FORWARDED, HeaderName};

    #[test]
    fn https_detects_x_forwarded_proto() {
        let mut h = HeaderMap::new();
        h.insert(
            HeaderName::from_static("x-forwarded-proto"),
            "https".parse().unwrap(),
        );
        assert!(is_https(&h));
    }

    #[test]
    fn https_detects_forwarded_proto() {
        let mut h = HeaderMap::new();
        h.insert(
            FORWARDED,
            "for=1.2.3.4;proto=https;host=example.com".parse().unwrap(),
        );
        assert!(is_https(&h));
    }

    #[test]
    fn https_detects_cf_visitor() {
        let mut h = HeaderMap::new();
        h.insert(
            HeaderName::from_static("cf-visitor"),
            r#"{"scheme":"https"}"#.parse().unwrap(),
        );
        assert!(is_https(&h));
    }

    #[test]
    fn https_negative_cases() {
        let mut h = HeaderMap::new();
        h.insert(
            HeaderName::from_static("x-forwarded-proto"),
            "http".parse().unwrap(),
        );
        assert!(!is_https(&h));

        let mut h2 = HeaderMap::new();
        h2.insert(FORWARDED, "for=1.2.3.4;proto=http".parse().unwrap());
        assert!(!is_https(&h2));

        let h3 = HeaderMap::new();
        assert!(!is_https(&h3));
    }
}
