use axum::Json;
use axum::extract::{Form, State};
use axum::http::header::{CONTENT_TYPE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use tokio::fs;

use crate::state::AppState;
use crate::util::{ADMIN_SESSION_TTL, get_cookie, json_error, new_id, now_secs};

#[derive(Deserialize)]
pub struct BanForm {
    pub ip: String,
    pub reason: Option<String>,
}

#[derive(Deserialize)]
pub struct UnbanForm {
    pub ip: String,
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
    if let Some(tok) = get_cookie(&headers, "adm") {
        if !state.is_admin(&tok).await {
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
        return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
    }
    let bans = state.bans.read().await.clone();
    let rows: String = bans
        .iter()
        .map(|b| {
            let ip_enc = htmlescape::encode_minimal(&b.ip);
            let reason_enc = htmlescape::encode_minimal(&b.reason);
            format!("<tr><td>{}</td><td>{}</td><td>{}</td><td><form method=post action=/unban style=margin:0><input type=hidden name=ip value=\"{}\"><button type=submit class=del aria-label=\"Unban {}\">Unban</button></form></td></tr>", ip_enc, reason_enc, b.time, ip_enc, ip_enc)
        })
        .collect();
    let path = state.static_dir.join("ban.html");
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
    if let Some(tok) = get_cookie(&headers, "adm") {
        if !state.is_admin(&tok).await {
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
        return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
    }
    let ip = frm.ip.trim();
    if ip.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "missing", "missing ip");
    }
    state
        .add_ban(ip.to_string(), frm.reason.unwrap_or_default())
        .await;
    state.persist_bans().await;
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
    if let Some(tok) = get_cookie(&headers, "adm") {
        if !state.is_admin(&tok).await {
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
        return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
    }
    let ip = frm.ip.trim();
    if ip.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "missing", "missing ip");
    }
    state.remove_ban(ip).await;
    state.persist_bans().await;
    (
        StatusCode::SEE_OTHER,
        [(LOCATION, HeaderValue::from_static("/admin/ban"))],
    )
        .into_response()
}

pub async fn auth_get_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
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
    _headers: HeaderMap,
    Form(frm): Form<AdminAuthForm>,
) -> Response {
    let submitted = frm.key.trim();
    if submitted.is_empty() {
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
        let cookie = format!(
            "adm={}; Path=/; HttpOnly; Max-Age={}; SameSite=Strict",
            token, ADMIN_SESSION_TTL
        );
        let mut resp = (
            StatusCode::SEE_OTHER,
            [(LOCATION, HeaderValue::from_static("/"))],
        )
            .into_response();
        resp.headers_mut()
            .append(SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
        return resp;
    }
    json_error(StatusCode::UNAUTHORIZED, "invalid_key", "invalid key")
}

pub async fn is_admin_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(tok) = get_cookie(&headers, "adm") {
        if state.is_admin(&tok).await {
            return (StatusCode::OK, Json(json!({"admin": true}))).into_response();
        }
    }
    (StatusCode::OK, Json(json!({"admin": false}))).into_response()
}

pub async fn admin_files_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(tok) = get_cookie(&headers, "adm") {
        if !state.is_admin(&tok).await {
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
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
        rows.push_str(&format!("<tr><td><a href=\"/f/{f}\" target=_blank rel=noopener>{f}</a></td><td>{o}</td><td data-exp=\"{exp}\">{human}</td><td>{size}</td><td><form method=post action=/admin/files style=margin:0><input type=hidden name=file value=\"{f}\"><button type=submit class=del data-file=\"{f}\">Delete</button></form></td></tr>", f=file, o=&meta.owner, exp=meta.expires, human=human, size=size));
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
    if let Some(tok) = get_cookie(&headers, "adm") {
        if !state.is_admin(&tok).await {
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
        return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
    }
    let file = frm.file.trim();
    if file.is_empty() || file.contains('/') || file.contains('\\') {
        return json_error(StatusCode::BAD_REQUEST, "bad_file", "invalid file");
    }
    state.owners.remove(file);
    let _ = fs::remove_file(state.upload_dir.join(file)).await;
    state.persist_owners().await;
    (
        StatusCode::SEE_OTHER,
        [(LOCATION, HeaderValue::from_static("/admin/files"))],
    )
        .into_response()
}

pub async fn admin_reports_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(tok) = get_cookie(&headers, "adm") {
        if !state.is_admin(&tok).await {
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
        return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
    }
    let reports = state.reports.read().await.clone();
    let mut rows = String::new();
    for (idx, r) in reports.iter().enumerate() {
        rows.push_str(&format!("<tr><td><a href=\"/{file}\" target=_blank rel=noopener>{file}</a></td><td>{reason}</td><td>{details}</td><td>{ip}</td><td>{time}</td><td><form method=post action=/admin/reports style=margin:0><input type=hidden name=idx value=\"{idx}\"><button type=submit class=del data-idx=\"{idx}\">Remove</button></form></td></tr>", file=htmlescape::encode_minimal(&r.file), reason=htmlescape::encode_minimal(&r.reason), details=htmlescape::encode_minimal(&r.details), ip=htmlescape::encode_minimal(&r.ip), time=r.time, idx=idx));
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
    if let Some(tok) = get_cookie(&headers, "adm") {
        if !state.is_admin(&tok).await {
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
        return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
    }
    let idx = frm.idx;
    {
        let mut reports = state.reports.write().await;
        if idx < reports.len() {
            reports.remove(idx);
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
