use axum::{extract::{State, Multipart, Path, ConnectInfo, Query, Form}, http::{StatusCode, HeaderMap, header::{CACHE_CONTROL, EXPIRES}, HeaderValue}, response::{IntoResponse, Response}, routing::{get, post, delete}, Json, Router, middleware::{Next}};
use axum::http::Request;
use axum::body::Body;
use std::{net::SocketAddr as ClientAddr, time::{SystemTime, Duration}};
use tokio::fs;
use serde::{Serialize, Deserialize};
use crate::util::{json_error, real_client_ip, is_forbidden_extension, make_storage_name, now_secs, ttl_to_duration, qualify_path, MAX_FILE_BYTES, PROD_HOST, get_cookie, ADMIN_SESSION_TTL};
use crate::util::extract_client_ip;
use crate::state::{AppState, FileMeta, ReportRecord, cleanup_expired, verify_user_entries_with_report, spawn_integrity_check, ReconcileReport};
use tower_http::services::ServeDir;
use time::{OffsetDateTime};
use tera::{Tera, Context};

// Email event for reports
#[derive(Clone, Debug)]
pub struct ReportRecordEmail {
    pub file: String,
    pub reason: String,
    pub details: String,
    pub ip: String,          // reporter ip
    pub time: u64,
    pub iso_time: String,
    pub owner_ip: String,
    pub original_name: String,
    pub expires: u64,
    pub size: u64,
    pub report_index: usize,
    pub total_reports_for_file: usize,
    pub total_reports: usize,
}

// Response structs
#[derive(Serialize, Deserialize)] pub struct UploadResponse { pub files: Vec<String>, pub truncated: bool, pub remaining: usize }
#[derive(Serialize)] pub struct ListResponse { pub files: Vec<String>, pub metas: Vec<FileMetaEntry>, pub reconcile: Option<ReconcileReport> }
#[derive(Serialize)] pub struct FileMetaEntry { pub file: String, pub expires: u64, #[serde(skip_serializing_if="String::is_empty")] pub original: String }

#[derive(Deserialize)] pub struct ReportForm { pub file: String, pub reason: String, pub details: Option<String> }
#[derive(Deserialize)] pub struct SimpleQuery { pub m: Option<String> }
#[derive(Deserialize)] pub struct SimpleDeleteForm { pub f: String }
#[derive(Deserialize)] pub struct AdminAuthForm { pub key: String }
#[derive(Deserialize)] pub struct BanForm { pub ip: String, pub reason: Option<String> }
#[derive(Deserialize)] pub struct UnbanForm { pub ip: String }
#[derive(Deserialize)] pub struct AdminFileDeleteForm { pub file: String }
#[derive(Deserialize)] pub struct AdminReportDeleteForm { pub idx: usize }

// Upload handler
#[axum::debug_handler]
pub async fn upload_handler(State(state): State<AppState>, ConnectInfo(addr): ConnectInfo<ClientAddr>, headers: HeaderMap, mut multipart: Multipart) -> Response {
    if state.is_banned(&real_client_ip(&headers,&addr)).await { return json_error(StatusCode::FORBIDDEN, "banned", "ip banned"); }
    let ip = real_client_ip(&headers, &addr);
    let sem = state.upload_sem.clone();
    let _permit = match sem.try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return json_error(StatusCode::SERVICE_UNAVAILABLE, "busy", "server is busy, try again later"),
    };

    let mut ttl_code = "24h".to_string();
    let mut files_to_process = Vec::new();

    while let Some(field) = multipart.next_field().await.unwrap() {
        let name = if let Some(name) = field.name() {
            name.to_string()
        } else {
            continue;
        };

        if name == "ttl" {
            if let Ok(data) = field.bytes().await {
                if let Ok(s) = std::str::from_utf8(&data) {
                    ttl_code = s.to_string();
                }
            }
            continue;
        }

        if name.starts_with("file") {
            let original_name = field.file_name().map(|s| s.to_string());
            if let Ok(data) = field.bytes().await {
                if !data.is_empty() {
                    files_to_process.push((original_name, data));
                }
            }
        }
    }

    if files_to_process.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "no_files", "no files were uploaded");
    }

    let expires = now_secs() + ttl_to_duration(&ttl_code).as_secs();
    let mut saved_files = Vec::new();

    for (original_name, data) in &files_to_process {
        if data.len() as u64 > MAX_FILE_BYTES {
            continue;
        }
        let storage_name = make_storage_name(original_name.as_deref());
        if is_forbidden_extension(&storage_name) {
            continue;
        }
        let path = state.upload_dir.join(&storage_name);
        if fs::write(&path, data).await.is_ok() {
            let meta = FileMeta { owner: ip.clone(), expires, original: original_name.clone().unwrap_or_default() };
            state.owners.write().await.insert(storage_name.clone(), meta);
            saved_files.push(storage_name);
        }
    }

    state.persist_owners().await;
    spawn_integrity_check(state.clone());

    let truncated = saved_files.len() < files_to_process.len();
    let remaining = files_to_process.len() - saved_files.len();

    (StatusCode::OK, Json(UploadResponse { files: saved_files, truncated, remaining })).into_response()
}

#[axum::debug_handler]
pub async fn list_handler(State(state): State<AppState>, ConnectInfo(addr): ConnectInfo<ClientAddr>, headers: HeaderMap) -> Response {
    if state.is_banned(&real_client_ip(&headers,&addr)).await { return json_error(StatusCode::FORBIDDEN, "banned", "ip banned"); }
    cleanup_expired(&state).await; let client_ip=real_client_ip(&headers, &addr); let reconcile_report=verify_user_entries_with_report(&state, &client_ip).await; cleanup_expired(&state).await; crate::state::check_storage_integrity(&state).await;
    let owners=state.owners.read().await;
    let mut files: Vec<(String,u64,String)>=owners.iter().filter_map(|(f,m)| if m.owner==client_ip { Some((f.clone(), m.expires, m.original.clone())) } else { None }).collect();
    files.sort_by(|a,b| a.0.cmp(&b.0));
    let only_names: Vec<String>=files.iter().map(|(n,_,_)| qualify_path(&state, &format!("f/{}", n))).collect();
    let metas: Vec<FileMetaEntry>=files.into_iter().map(|(n,e,o)| FileMetaEntry{ file: qualify_path(&state, &format!("f/{}", n)), expires: e, original: o }).collect();
    let body=Json(ListResponse{ files: only_names, metas, reconcile: reconcile_report });
    let mut resp=body.into_response(); resp.headers_mut().insert(CACHE_CONTROL, "no-store".parse().unwrap()); resp }

#[axum::debug_handler]
pub async fn fetch_file_handler(State(state): State<AppState>, Path(file): Path<String>) -> Response {
    if file.contains('/') { return (StatusCode::BAD_REQUEST, "bad file").into_response(); }
    cleanup_expired(&state).await;
    let now = now_secs();
    let (exists, expired, meta_expires) = {
        let owners = state.owners.read().await;
        if let Some(m) = owners.get(&file) { (true, m.expires <= now, m.expires) } else { (false, true, 0) }
    };
    if !exists || expired { return (StatusCode::NOT_FOUND, "not found").into_response(); }
    let file_path = state.upload_dir.join(&file);
    if !file_path.exists() { return (StatusCode::NOT_FOUND, "not found").into_response(); }
    match fs::read(&file_path).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&file_path).first_or_octet_stream();
            let mut headers = HeaderMap::new();
            headers.insert(axum::http::header::CONTENT_TYPE, mime.as_ref().parse().unwrap());
            // derive remaining TTL for caching
            if meta_expires > now {
                let remaining = meta_expires - now;
                headers.insert(CACHE_CONTROL, HeaderValue::from_str(&format!("public, max-age={}", remaining)).unwrap());
                let exp_time = SystemTime::UNIX_EPOCH + Duration::from_secs(meta_expires);
                headers.insert(EXPIRES, HeaderValue::from_str(&httpdate::fmt_http_date(exp_time)).unwrap());
            }
            (headers, bytes).into_response()
        },
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "fs_error", "cant read file")
    }
}

#[axum::debug_handler]
pub async fn delete_handler(State(state): State<AppState>, ConnectInfo(addr): ConnectInfo<ClientAddr>, headers: HeaderMap, Path(file): Path<String>) -> Response { let ip=real_client_ip(&headers, &addr); if state.is_banned(&ip).await { return json_error(StatusCode::FORBIDDEN, "banned", "ip banned"); } if file.contains('/') || file.contains("..") || file.contains('\\') { return json_error(StatusCode::BAD_REQUEST, "bad_file", "invalid file name"); } cleanup_expired(&state).await; { let owners=state.owners.read().await; match owners.get(&file) { Some(meta) if meta.owner==ip => {}, _=> return (StatusCode::NOT_FOUND, "not found").into_response(), } } { let mut owners=state.owners.write().await; owners.remove(&file); } let path=state.upload_dir.join(&file); let _=fs::remove_file(&path).await; state.persist_owners().await; (StatusCode::NO_CONTENT, ()).into_response() }

#[axum::debug_handler]
pub async fn report_handler(State(state): State<AppState>, ConnectInfo(addr): ConnectInfo<ClientAddr>, headers: HeaderMap, Form(form): Form<ReportForm>) -> Response {
    if state.is_banned(&real_client_ip(&headers,&addr)).await { return json_error(StatusCode::FORBIDDEN, "banned", "ip banned"); }
    let ip = real_client_ip(&headers, &addr);
    let now = now_secs();
    // Canonicalize file id: if exact not found and no extension supplied, try auto-matching stored file with extension.
    let mut file_name = form.file.trim().to_string();
    {
        let owners = state.owners.read().await; // read lock scope
        if !owners.contains_key(&file_name) && !file_name.contains('.') {
            // collect candidates that start with "{id}." (single extension segment)
            let prefix = format!("{file_name}.");
            let mut candidates: Vec<&String> = owners.keys().filter(|k| k.starts_with(&prefix)).collect();
            // deterministic selection: pick shortest name (i.e., shortest extension); then lexicographically
            candidates.sort();
            candidates.sort_by_key(|k| k.len());
            if let Some(best) = candidates.first() { file_name = (*best).clone(); }
        }
    }
    let record = ReportRecord { file: file_name.clone(), reason: form.reason.clone(), details: form.details.clone().unwrap_or_default(), ip: ip.clone(), time: now };
    let (owner_ip, original_name, expires, size) = {
        let owners = state.owners.read().await;
        if let Some(meta) = owners.get(&record.file) {
            let path = state.upload_dir.join(&record.file);
            let sz = tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0);
            (meta.owner.clone(), meta.original.clone(), meta.expires, sz)
        } else { (String::new(), String::new(), 0u64, 0u64) }
    };
    let (report_index, total_reports_for_file, total_reports) = {
        let mut reports = state.reports.write().await;
        reports.push(record.clone());
        let idx = reports.len() - 1;
        let count_file = reports.iter().filter(|r| r.file == record.file).count();
        let total = reports.len();
        (idx, count_file, total)
    };
    state.persist_reports().await;
    if let Some(tx) = &state.email_tx {
        let iso = OffsetDateTime::from_unix_timestamp(now as i64).map(|t| t.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()).unwrap_or_default();
        let _ = tx.send(ReportRecordEmail {
            file: record.file.clone(),
            reason: record.reason.clone(),
            details: record.details.clone(),
            ip: record.ip.clone(),
            time: record.time,
            iso_time: iso,
            owner_ip,
            original_name,
            expires,
            size,
            report_index,
            total_reports_for_file,
            total_reports,
        }).await;
    }
    (StatusCode::NO_CONTENT, ()).into_response()
}

pub async fn file_handler(State(state): State<AppState>, Path(path): Path<String>) -> Response {
    // normalize and security checks + extensionless .html support
    let rel = path.trim_start_matches('/');
    if rel.contains("..") || rel.contains('\\') { return (StatusCode::BAD_REQUEST, "bad path").into_response(); }
    let mut candidate = state.static_dir.join(rel);
    if !candidate.exists() {
        // try mapping extensionless request to .html file
        if !rel.is_empty() && !rel.contains('.') {
            let alt = state.static_dir.join(format!("{}.html", rel));
            if alt.exists() { candidate = alt; } else { return (StatusCode::NOT_FOUND, "not found").into_response(); }
        } else { return (StatusCode::NOT_FOUND, "not found").into_response(); }
    }
    match fs::read(&candidate).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&candidate).first_or_octet_stream();
            let mut headers = HeaderMap::new();
            headers.insert(axum::http::header::CONTENT_TYPE, mime.as_ref().parse().unwrap());
            // apply cache policy based on extension
            if let Some(ext) = candidate.extension().and_then(|e| e.to_str()) {
                let cacheable = matches!(ext.to_ascii_lowercase().as_str(), "css"|"js"|"webp"|"png"|"jpg"|"jpeg"|"gif"|"svg"|"ico"|"woff"|"woff2");
                if cacheable {
                    let max_age = 86400; // 1 day
                    headers.insert(CACHE_CONTROL, HeaderValue::from_str(&format!("public, max-age={max_age}")).unwrap());
                    let exp_time = SystemTime::now() + Duration::from_secs(max_age);
                    headers.insert(EXPIRES, HeaderValue::from_str(&httpdate::fmt_http_date(exp_time)).unwrap());
                }
            }
            (headers, bytes).into_response()
        },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "cant read file").into_response()
    }
}

pub async fn root_handler(State(state): State<AppState>) -> Response {
    // Prepare Tera context (for now, just English, can expand for i18n)
    let mut ctx = Context::new();
    ctx.insert("lang", "en");
    // Example: pass translation map (t) for i18n
    let mut t = std::collections::HashMap::new();
    // (You can load translations from a file or hardcode for now)
    t.insert("title", "JuiceBox - Fast Temporary File Host");
    t.insert("meta_description", "JuiceBox is an open-source and simple high-speed temporary file host with hotlinking. Click, upload, share lightweight expiring links with selectable file retention up to 500mb.");
    t.insert("og_title", "JuiceBox – Fast Temporary File Host");
    t.insert("og_description", "Upload files up to 500MB and share instant expiring links (1h–14d retention).");
    t.insert("twitter_title", "JuiceBox – Fast Temporary File Host");
    t.insert("twitter_description", "Upload files (≤500MB) and share expiring links with selectable retention.");
    t.insert("skip_main", "Skip to main content");
    t.insert("skip_files", "Skip to your files");
    t.insert("js_disabled", "JavaScript is disabled or unsupported. Use the <a href='/simple' style='color:var(--accent);text-decoration:underline;'>basic no‑script uploader</a>.");
    t.insert("brand_subtitle", "Fast Temporary File Host");
    t.insert("lead", "Click, Upload, Share!");
    t.insert("upload", "Upload");
    t.insert("ttl_title", "Choose how long the uploaded files will be kept before automatic deletion");
    t.insert("retention", "Retention:");
    t.insert("ttl_adjust", "Adjust retention from 1 hour up to 14 days");
    t.insert("auto_delete", "auto delete");
    t.insert("drop_title", "Click or drag files here to upload (max 500MB each)");
    t.insert("file_picker", "Select files to upload (maximum 500 megabytes each)");
    t.insert("drop_hint", "Click or drop files");
    t.insert("upload_note", "Upload your files. Links show after each finishes. Maximum file size is 500MB, Files expire after selected retention. Limit: maximum 5 active files per IP (delete one to free a slot). See <a href='/faq' target='_blank' rel='noopener' title='Open FAQ in new tab'>Read the FAQ</a>.");
    t.insert("privacy_note", "Notice: Your IP address is processed only to associate uploads with your session, apply limits, and enable abuse/moderation actions. It is not shared and is removed once all related files expire.");
    t.insert("your_files", "Your Files");
    t.insert("owned_note", "These are files linked to your IP, They persist across refresh until deleted.");
    t.insert("about_heading", "What is JuiceBox?");
    t.insert("about_1", "JuiceBox Temporary File Host is a fast, simple way to share files without creating an account. Pick a retention period from 1 hour up to 14 days, upload files up to 500 MB, and instantly share lightweight expiring links. When the timer ends, files are automatically deleted. It’s perfect for quick hand‑offs, code snippets, screenshots, small archives, and any content you don’t want to keep online forever.");
    t.insert("about_2", "How it works: drag and drop or click to select files, choose a retention window, then copy the link once each upload completes. We limit each IP to a small number of active files to prevent abuse; delete an older item to free a slot. For a minimal experience, try the <a href='/simple'>no‑script uploader</a>. For details about limits, retention, thumbnails and moderation, read the <a href='/faq'>Frequently Asked Questions</a>. Our focus is speed, privacy and ease: no tracking pixels, no social logins, and no permanent storage, just quick, disposable sharing.");
    t.insert("about_3", "Privacy and safety: your IP is used only to associate uploads, enforce rate limits and handle abuse reports, and it is removed after related files expire. To report prohibited content or request removal, visit the <a href='/report'>Report form</a> page. Use of the service implies acceptance of our <a href='/terms'>Terms of Service</a>. If you prefer to browse a simple landing link to share with others, you can point them to the home page or the <a href='/faq'>FAQ guide</a> for guidance.");
    t.insert("inspired", "Inspired by");
    t.insert("share", "Share:");
    t.insert("home", "Home");
    t.insert("simple", "Simple");
    t.insert("faq", "FAQ");
    t.insert("report", "Report");
    t.insert("terms", "Terms");
    t.insert("donate", "Donate");
    ctx.insert("t", &t);
    let tera = &state.tera;
    match tera.render("index.html.tera", &ctx) {
        Ok(rendered) => (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/html")], rendered).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("template error: {}", e)).into_response(),
    }
}

pub async fn debug_ip_handler(ConnectInfo(addr): ConnectInfo<ClientAddr>, headers: HeaderMap) -> Response {
    let edge = addr.ip().to_string();
    let cf = headers.get("CF-Connecting-IP").and_then(|v| v.to_str().ok()).unwrap_or("-");
    let xff = headers.get("X-Forwarded-For").and_then(|v| v.to_str().ok()).unwrap_or("-");
    Json(serde_json::json!({"edge": edge, "cf": cf, "xff": xff})).into_response()
}

pub async fn report_page_handler(State(state): State<AppState>) -> Response {
    let path = state.static_dir.join("report.html");
    if !path.exists() { return (StatusCode::NOT_FOUND, "report page missing").into_response(); }
    match fs::read(&path).await {
        Ok(bytes) => { let mime = mime_guess::from_path(&path).first_or_octet_stream(); ([(axum::http::header::CONTENT_TYPE, mime.as_ref())], bytes).into_response() },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "cant read report").into_response()
    }
}

pub async fn ban_page_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    // admin gate
    if let Some(tok)=get_cookie(&headers, "adm") { if state.is_admin(&tok).await { } else { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); } } else { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); }
    let bans = state.bans.read().await.clone();
    // Updated: include action (unban) column
    let rows: String = bans.iter().map(|b| {
        let ip_enc = htmlescape::encode_minimal(&b.ip);
        let reason_enc = htmlescape::encode_minimal(&b.reason);
        format!("<tr><td>{}</td><td>{}</td><td>{}</td><td><form method=post action=/unban style=margin:0><input type=hidden name=ip value=\"{}\"><button type=submit class=del aria-label=\"Unban {}\">Unban</button></form></td></tr>", ip_enc, reason_enc, b.time, ip_enc, ip_enc)
    }).collect();
    let path = state.static_dir.join("ban.html");
    match fs::read(&path).await {
        Ok(bytes) => {
            let mut body = String::from_utf8_lossy(&bytes).into_owned();
            body = body.replace("{{ROWS}}", &rows);
            (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/html")], body).into_response()
        },
        Err(_) => json_error(StatusCode::NOT_FOUND, "missing_template", "ban template missing")
    }
}

#[axum::debug_handler]
pub async fn ban_post_handler(State(state): State<AppState>, headers: HeaderMap, Form(frm): Form<BanForm>) -> Response { if let Some(tok)=get_cookie(&headers, "adm") { if !state.is_admin(&tok).await { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); } } else { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); } let ip = frm.ip.trim(); if ip.is_empty() { return json_error(StatusCode::BAD_REQUEST, "missing", "missing ip"); } state.add_ban(ip.to_string(), frm.reason.unwrap_or_default()).await; state.persist_bans().await; (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, HeaderValue::from_static("/admin/ban"))]).into_response() }

#[axum::debug_handler]
pub async fn unban_post_handler(State(state): State<AppState>, headers: HeaderMap, Form(frm): Form<UnbanForm>) -> Response {
    if let Some(tok)=get_cookie(&headers, "adm") { if !state.is_admin(&tok).await { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); } } else { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); }
    let ip = frm.ip.trim();
    if ip.is_empty() { return json_error(StatusCode::BAD_REQUEST, "missing", "missing ip"); }
    state.remove_ban(ip).await; state.persist_bans().await;
    (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, HeaderValue::from_static("/admin/ban"))]).into_response()
}

pub async fn auth_get_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(tok) = get_cookie(&headers, "adm") { if state.is_admin(&tok).await {
        let already_path = state.static_dir.join("admin_already.html");
        if let Ok(bytes)=fs::read(&already_path).await { return (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/html")], bytes).into_response(); }
        return (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/html")], "<html><body><p>Already admin.</p><a href=/isadmin>Check</a></body></html>").into_response();
    } }
    let tpl_path = state.static_dir.join("admin_auth.html");
    match fs::read(&tpl_path).await {
        Ok(bytes) => (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/html")], bytes).into_response(),
        Err(_) => (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/html")], "<html><body><form method=post action=/auth><input type=password name=key autofocus placeholder=Admin+Key><button type=submit>Auth</button></form></body></html>").into_response(),
    }
}

pub async fn auth_post_handler(State(state): State<AppState>, _headers: HeaderMap, Form(frm): Form<AdminAuthForm>) -> Response {
    let submitted = frm.key.trim();
    if submitted.is_empty() { return json_error(StatusCode::BAD_REQUEST, "missing", "missing key"); }
    // read current key from state
    let current_key = { state.admin_key.read().await.clone() };
    if current_key.is_empty() { return json_error(StatusCode::INTERNAL_SERVER_ERROR, "no_key", "admin key unavailable"); }
    if subtle_equals(submitted.as_bytes(), current_key.as_bytes()) {
        let token = crate::util::new_id();
        state.create_admin_session(token.clone()).await;
        state.persist_admin_sessions().await;
        let cookie = format!("adm={}; Path=/; HttpOnly; Max-Age={}; SameSite=Strict", token, ADMIN_SESSION_TTL);
        let mut resp = (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, HeaderValue::from_static("/"))]).into_response();
        resp.headers_mut().append(axum::http::header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
        return resp;
    }
    json_error(StatusCode::UNAUTHORIZED, "invalid_key", "invalid key")
}

pub async fn is_admin_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(tok) = get_cookie(&headers, "adm") { if state.is_admin(&tok).await { return (StatusCode::OK, Json(serde_json::json!({"admin": true}))).into_response(); } }
    (StatusCode::OK, Json(serde_json::json!({"admin": false}))).into_response()
}

fn subtle_equals(a: &[u8], b: &[u8]) -> bool { if a.len()!=b.len() { return false; } let mut diff: u8 = 0; for i in 0..a.len() { diff |= a[i] ^ b[i]; } diff == 0 }

pub async fn add_security_headers(req: axum::http::Request<Body>, next: Next) -> Response { let mut resp=next.run(req).await; let h=resp.headers_mut(); if !h.contains_key("X-Content-Type-Options") { h.insert("Content-Security-Policy", "default-src 'self'; script-src 'self' 'unsafe-inline' https://static.cloudflareinsights.com; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:".parse().unwrap()); } if !h.contains_key("Permissions-Policy") { h.insert("Permissions-Policy", "camera=(), microphone=(), geolocation=(), fullscreen=(), payment=()".parse().unwrap()); }
  // Ensure charset is specified for HTML responses
  if let Some(ct_val) = h.get(axum::http::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()) {
    let ct_lower = ct_val.to_ascii_lowercase();
    if ct_lower.starts_with("text/html") && !ct_lower.contains("charset=") {
      h.insert(axum::http::header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    }
  }
  resp }

pub async fn enforce_host(req: axum::http::Request<Body>, next: Next) -> Response { let host = req.headers().get("host").and_then(|h| h.to_str().ok()).unwrap_or_default(); if host == PROD_HOST { next.run(req).await } else { let uri = format!("https://{}{}", PROD_HOST, req.uri().path_and_query().map(|pq| pq.as_str()).unwrap_or("/")); let hv = HeaderValue::from_str(&uri).unwrap(); (StatusCode::MOVED_PERMANENTLY, [(axum::http::header::LOCATION, hv)]).into_response() } }

// Global middleware: if IP banned, immediately return a themed banned page.
pub async fn ban_gate(State(state): State<AppState>, ConnectInfo(addr): ConnectInfo<ClientAddr>, req: Request<Body>, _next: Next) -> Response {
    let path = req.uri().path();
    if path.starts_with("/css/") || path.starts_with("/js/") { return _next.run(req).await; }
    let ip = extract_client_ip(req.headers(), Some(addr.ip()));
    if !state.is_banned(&ip).await { return _next.run(req).await; }
    let (reason,time) = { let bans=state.bans.read().await; if let Some(b)=bans.iter().find(|b| b.ip==ip) { (b.reason.clone(), b.time) } else { (String::new(), 0) } };
    let safe_reason = htmlescape::encode_minimal(&reason);
    let time_line = if time>0 { format!("<br><span class=code>Time: {time}</span>") } else { String::new() };
    let tpl_path = state.static_dir.join("banned.html");
    if let Ok(bytes)=fs::read(&tpl_path).await {
        let mut body = String::from_utf8_lossy(&bytes).into_owned();
        body = body.replace("{{REASON}}", &safe_reason).replace("{{IP}}", &ip).replace("{{TIME_LINE}}", &time_line);
        return (StatusCode::FORBIDDEN, [(axum::http::header::CONTENT_TYPE, "text/html")], body).into_response();
    }
    let fallback = format!("<html><body><h1>Banned</h1><p>{}</p><p>{}</p></body></html>", safe_reason, ip);
    (StatusCode::FORBIDDEN, [(axum::http::header::CONTENT_TYPE, "text/html")], fallback).into_response()
}

pub async fn admin_files_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(tok)=get_cookie(&headers, "adm") { if !state.is_admin(&tok).await { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); } } else { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); }
    // Build table rows (file, owner, expires human, size)
    let owners = state.owners.read().await.clone();
    let mut rows = String::new();
    let now = now_secs();
    for (file, meta) in owners.iter() {
        let path = state.upload_dir.join(file);
        let size = match fs::metadata(&path).await { Ok(md)=> md.len(), Err(_)=>0 };
        let remain = if meta.expires>now { meta.expires - now } else { 0 };
        let human = if remain >= 86400 { format!("{}d", remain/86400) } else if remain >= 3600 { format!("{}h", remain/3600) } else if remain >= 60 { format!("{}m", remain/60) } else { format!("{}s", remain) };
        rows.push_str(&format!("<tr><td><a href=\"/f/{f}\" target=_blank rel=noopener>{f}</a></td><td>{o}</td><td data-exp=\"{exp}\">{human}</td><td>{size}</td><td><form method=post action=/admin/files style=margin:0><input type=hidden name=file value=\"{f}\"><button type=submit class=del data-file=\"{f}\">Delete</button></form></td></tr>", f=file, o=&meta.owner, exp=meta.expires, human=human, size=size));
    }
    let tpl_path = state.static_dir.join("admin_files.html");
    match fs::read(&tpl_path).await { Ok(bytes)=> { let mut body=String::from_utf8_lossy(&bytes).into_owned(); body = body.replace("{{FILE_ROWS}}", &rows); (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/html")], body).into_response() }, Err(_)=> json_error(StatusCode::NOT_FOUND, "missing_template", "admin files template missing") }
}

#[axum::debug_handler]
pub async fn admin_file_delete_handler(State(state): State<AppState>, headers: HeaderMap, Form(frm): Form<AdminFileDeleteForm>) -> Response {
    if let Some(tok)=get_cookie(&headers, "adm") { if !state.is_admin(&tok).await { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); } } else { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); }
    let file = frm.file.trim();
    if file.is_empty() || file.contains('/') || file.contains('\\') { return json_error(StatusCode::BAD_REQUEST, "bad_file", "invalid file"); }
    {
        let mut owners = state.owners.write().await;
        owners.remove(file);
    }
    let _ = fs::remove_file(state.upload_dir.join(file)).await;
    state.persist_owners().await;
    let hv = HeaderValue::from_static("/admin/files");
    (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, hv)]).into_response()
}

pub async fn admin_reports_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(tok)=get_cookie(&headers, "adm") { if !state.is_admin(&tok).await { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); } } else { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); }
    let reports = state.reports.read().await.clone();
    let mut rows = String::new();
    for (idx, r) in reports.iter().enumerate() {
        rows.push_str(&format!("<tr><td><a href=\"/{file}\" target=_blank rel=noopener>{file}</a></td><td>{reason}</td><td>{details}</td><td>{ip}</td><td>{time}</td><td><form method=post action=/admin/reports style=margin:0><input type=hidden name=idx value=\"{idx}\"><button type=submit class=del data-idx=\"{idx}\">Remove</button></form></td></tr>", file=htmlescape::encode_minimal(&r.file), reason=htmlescape::encode_minimal(&r.reason), details=htmlescape::encode_minimal(&r.details), ip=htmlescape::encode_minimal(&r.ip), time=r.time, idx=idx));
    }
    let tpl_path = state.static_dir.join("admin_reports.html");
    match fs::read(&tpl_path).await { Ok(bytes)=> { let mut body=String::from_utf8_lossy(&bytes).into_owned(); body=body.replace("{{REPORT_ROWS}}", &rows); (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/html")], body).into_response() }, Err(_)=> json_error(StatusCode::NOT_FOUND, "missing_template", "admin reports template missing") }
}

#[axum::debug_handler]
pub async fn admin_report_delete_handler(State(state): State<AppState>, headers: HeaderMap, Form(frm): Form<AdminReportDeleteForm>) -> Response {
    if let Some(tok)=get_cookie(&headers, "adm") { if !state.is_admin(&tok).await { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); } } else { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); }
    let idx = frm.idx;
    {
        let mut reports = state.reports.write().await;
        if idx < reports.len() { reports.remove(idx); }
    }
    state.persist_reports().await;
    let hv = HeaderValue::from_static("/admin/reports");
    (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, hv)]).into_response()
}

// Placeholder handlers for /simple endpoints
pub async fn simple_list_handler() -> axum::response::Response {
    (axum::http::StatusCode::NOT_IMPLEMENTED, "Not implemented").into_response()
}

pub async fn simple_upload_handler() -> axum::response::Response {
    (axum::http::StatusCode::NOT_IMPLEMENTED, "Not implemented").into_response()
}

pub async fn simple_delete_handler() -> axum::response::Response {
    (axum::http::StatusCode::NOT_IMPLEMENTED, "Not implemented").into_response()
}

// Add cache middleware for static asset dirs (css/js)
pub async fn add_cache_headers(req: axum::http::Request<Body>, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let mut resp = next.run(req).await;
    if (path.starts_with("/css/") || path.starts_with("/js/")) && !path.contains("../") {
        let headers = resp.headers_mut();
        let max_age = 86400; // 1 day (filenames not fingerprinted)
        headers.insert(CACHE_CONTROL, HeaderValue::from_str(&format!("public, max-age={max_age}")).unwrap());
        let exp_time = SystemTime::now() + Duration::from_secs(max_age as u64);
        headers.insert(EXPIRES, HeaderValue::from_str(&httpdate::fmt_http_date(exp_time)).unwrap());
    }
    resp
}

pub fn build_router(state: AppState) -> Router {
    let static_root = state.static_dir.clone();
    let css_service = ServeDir::new(static_root.join("css"));
    let js_service = ServeDir::new(static_root.join("js"));
    Router::new()
        .route("/upload", post(upload_handler))
        .route("/list", get(list_handler))
        .route("/mine", get(list_handler))
        .route("/f/{file}", get(fetch_file_handler).delete(delete_handler))
        .route("/d/{file}", delete(delete_handler))
        .route("/report", get(report_page_handler).post(report_handler))
        .route("/unban", post(unban_post_handler))
        .route("/healthz", get(|| async { "ok" }))
        .route("/simple", get(simple_list_handler))
        .route("/simple/upload", post(simple_upload_handler))
        .route("/simple_delete", post(simple_delete_handler))
        .route("/auth", get(auth_get_handler).post(auth_post_handler))
        .route("/isadmin", get(is_admin_handler))
        .route("/debug-ip", get(debug_ip_handler))
        .route("/admin/ban", get(ban_page_handler).post(ban_post_handler))
        .route("/admin/files", get(admin_files_handler).post(admin_file_delete_handler))
        .route("/admin/reports", get(admin_reports_handler).post(admin_report_delete_handler))
        .nest_service("/css", css_service.clone())
        .nest_service("/js", js_service.clone())
        .route("/", get(root_handler))
        .route("/{*path}", get(file_handler))
        .with_state(state)
}
