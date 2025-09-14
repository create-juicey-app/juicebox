use axum::{extract::{State, Multipart, Path, ConnectInfo, Query, Form}, http::{StatusCode, HeaderMap, header::{CACHE_CONTROL}, HeaderValue}, response::{IntoResponse, Response}, routing::{get, post, delete}, Json, Router, middleware::{Next}};
use axum::http::Request;
use axum::body::Body;
use std::{net::SocketAddr as ClientAddr};
use tokio::fs;
use serde::{Serialize, Deserialize};
use crate::util::{json_error, real_client_ip, is_forbidden_extension, make_storage_name, now_secs, ttl_to_duration, qualify_path, MAX_FILE_BYTES, PROD_HOST, get_cookie, ADMIN_SESSION_TTL};
use crate::util::extract_client_ip;
use crate::state::{AppState, FileMeta, ReportRecord, cleanup_expired, verify_user_entries_with_report, spawn_integrity_check, ReconcileReport};
use crate::state::IpBan;
use tower_http::services::ServeDir;

// Response structs
#[derive(Serialize, Deserialize)] pub struct UploadResponse { pub files: Vec<String>, pub truncated: bool, pub remaining: usize }
#[derive(Serialize)] pub struct ListResponse { pub files: Vec<String>, pub metas: Vec<FileMetaEntry>, pub reconcile: Option<ReconcileReport> }
#[derive(Serialize)] pub struct FileMetaEntry { pub file: String, pub expires: u64 }

#[derive(Deserialize)] pub struct ReportForm { pub file: String, pub reason: String, pub details: Option<String> }
#[derive(Deserialize)] pub struct SimpleQuery { pub m: Option<String> }
#[derive(Deserialize)] pub struct SimpleDeleteForm { pub f: String }
#[derive(Deserialize)] pub struct AdminAuthForm { pub key: String }
#[derive(Deserialize)] pub struct BanForm { pub ip: String, pub reason: Option<String> }
#[derive(Deserialize)] pub struct UnbanForm { pub ip: String }

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
            let meta = FileMeta { owner: ip.clone(), expires };
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
    cleanup_expired(&state).await; let client_ip=real_client_ip(&headers, &addr); let reconcile_report=verify_user_entries_with_report(&state, &client_ip).await; cleanup_expired(&state).await; crate::state::check_storage_integrity(&state).await; let owners=state.owners.read().await; let mut files: Vec<(String,u64)>=owners.iter().filter_map(|(f,m)| if m.owner==client_ip { Some((f.clone(), m.expires)) } else { None }).collect(); files.sort_by(|a,b| a.0.cmp(&b.0)); let only_names: Vec<String>=files.iter().map(|(n,_)| qualify_path(&state, &format!("f/{}", n))).collect(); let metas: Vec<FileMetaEntry>=files.into_iter().map(|(n,e)| FileMetaEntry{ file: qualify_path(&state, &format!("f/{}", n)), expires: e }).collect(); let body=Json(ListResponse{ files: only_names, metas, reconcile: reconcile_report }); let mut resp=body.into_response(); resp.headers_mut().insert(CACHE_CONTROL, "no-store".parse().unwrap()); resp }

#[axum::debug_handler]
pub async fn fetch_file_handler(State(state): State<AppState>, Path(file): Path<String>) -> Response {
    if file.contains('/') {
        return (StatusCode::BAD_REQUEST, "bad file").into_response();
    }
    cleanup_expired(&state).await;
    let expired = {
        let owners = state.owners.read().await;
        owners.get(&file).map(|m| m.expires <= now_secs()).unwrap_or(true)
    };
    if expired {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let file_path = state.upload_dir.join(&file);
    if !file_path.exists() {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match fs::read(&file_path).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&file_path).first_or_octet_stream();
            let mut headers = HeaderMap::new();
            headers.insert(axum::http::header::CONTENT_TYPE, mime.as_ref().parse().unwrap());
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
    let record = ReportRecord { file: form.file, reason: form.reason, details: form.details.unwrap_or_default(), ip, time: now_secs() };
    {
        // acquire write lock, push, then drop before persisting
        let mut reports = state.reports.write().await;
        reports.push(record);
    }
    state.persist_reports().await;
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
        } else {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
    }
    match fs::read(&candidate).await {
        Ok(bytes) => { let mime = mime_guess::from_path(&candidate).first_or_octet_stream(); ([(axum::http::header::CONTENT_TYPE, mime.as_ref())], bytes).into_response() },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "cant read file").into_response()
    }
}

pub async fn root_handler(State(state): State<AppState>) -> Response { let index_path=state.static_dir.join("index.html"); if !index_path.exists() { return (StatusCode::NOT_FOUND, "index missing").into_response(); } match fs::read(&index_path).await { Ok(bytes)=>{ let mime=mime_guess::from_path(&index_path).first_or_octet_stream(); ([(axum::http::header::CONTENT_TYPE, mime.as_ref())], bytes).into_response() }, Err(_)=>(StatusCode::INTERNAL_SERVER_ERROR, "cant read index").into_response() } }

pub async fn simple_list_handler(State(state): State<AppState>, ConnectInfo(addr): ConnectInfo<ClientAddr>, headers: HeaderMap, Query(query): Query<SimpleQuery>) -> Response {
    let ip = real_client_ip(&headers, &addr);
    let owners = state.owners.read().await;
    let my_files: Vec<String> = owners.iter().filter(|(_, meta)| meta.owner == ip).map(|(file, _)| file.clone()).collect();
    let message = query.m.unwrap_or_default();
    let body = format!(
        "<html><body><h1>Your Files</h1><p>{}</p><ul>{}</ul><form method=post action=/simple_delete><input name=f><button type=submit>Delete</button></form></body></html>",
        message,
        my_files.iter().map(|f| format!("<li>{}</li>", f)).collect::<String>()
    );
    ([(axum::http::header::CONTENT_TYPE, "text/html")], body).into_response()
}

#[axum::debug_handler]
pub async fn simple_delete_handler(State(state): State<AppState>, ConnectInfo(addr): ConnectInfo<ClientAddr>, headers: HeaderMap, Form(frm): Form<SimpleDeleteForm>) -> Response {
    let ip=real_client_ip(&headers, &addr);
    let target=frm.f;
    let owned = {
        let owners=state.owners.read().await;
        owners.get(&target).map(|m| m.owner.clone())
    };
    if owned.is_some() && owned.unwrap() == ip {
        {
            let mut owners=state.owners.write().await;
            owners.remove(&target);
        }
        let _ = fs::remove_file(state.upload_dir.join(&target)).await;
        state.persist_owners().await;
        let hv = HeaderValue::from_static("/simple?m=Deleted");
        return (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, hv)]).into_response();
    }
    let hv = HeaderValue::from_static("/simple?m=Not+found");
    (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, hv)]).into_response()
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
    // Build HTML similar styling to report
    let bans = state.bans.read().await.clone();
    let rows: String = bans.iter().map(|b| format!("<tr><td>{}</td><td>{}</td><td>{}</td></tr>", b.ip, htmlescape::encode_minimal(&b.reason), b.time)).collect();
    // Note: pattern attribute uses double braces to avoid format! interpreting them
    let body = format!(r#"<!DOCTYPE html><html lang=en><head><meta charset=utf-8><title>IP Bans – JuiceBox</title><meta name=viewport content="width=device-width,initial-scale=1" /><link rel=stylesheet href=/css/app.css /><style>main{{width:min(760px,88%);}} table{{width:100%;border-collapse:collapse;font-size:.65rem;}} th,td{{border:1px solid #33464f;padding:.45rem .55rem;text-align:left;}} th{{background:#1e2a33;text-transform:uppercase;letter-spacing:.5px;font-weight:600;color:var(--text-subtle);}} tbody tr:nth-child(even){{background:#1b252e;}} form.ban-form{{display:flex;flex-wrap:wrap;gap:.6rem;margin:0 0 1.2rem;}} form.ban-form input, form.ban-form textarea{{background:#121b24;color:var(--text);border:1px solid #2b394a;border-radius:9px;padding:.55rem .65rem;font:inherit;font-size:.7rem;}} form.ban-form button{{background:var(--accent);color:#111;border:1px solid rgba(var(--accent-rgb)/.6);padding:.55rem 1rem;font-size:.7rem;font-weight:600;border-radius:9px;cursor:pointer;}} .msg{{font-size:.65rem;}}</style></head><body><header style="padding:1.7rem 1.2rem 1rem;text-align:center;"><h1 style="margin:0 0 .6rem;font-size:clamp(1.8rem,4.2vw,3rem);">IP Bans</h1><p class=lead style=margin:0;font-size:.9rem;color:var(--text-subtle);>Manage blocked IP addresses.</p></header><main id=banMain><div class=panel><section><form class=ban-form method=post action=/ban><input name=ip placeholder="IP or CIDR" required pattern="[A-Fa-f0-9:.\\/]{{3,64}}" /><input name=reason placeholder=Reason /><button type=submit>Ban</button></form><form class=ban-form method=post action=/unban><input name=ip placeholder="IP" required pattern="[A-Fa-f0-9:.]{{3,64}}" /><button type=submit>Unban</button></form><h2 style="margin:1rem 0 .6rem;font-size:.9rem;color:var(--text-subtle);letter-spacing:.5px;text-transform:uppercase;">Current Bans</h2><div style="overflow:auto;max-height:340px;"><table><thead><tr><th>IP</th><th>Reason</th><th>Time</th></tr></thead><tbody>{rows}</tbody></table></div></section></div></main><footer style="margin:1.8rem 0 1rem;text-align:center;font-size:.55rem;opacity:.45;">juicebox // admin // bans</footer></body></html>"#);
    (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/html")], body).into_response()
}

#[axum::debug_handler]
pub async fn ban_post_handler(State(state): State<AppState>, headers: HeaderMap, Form(frm): Form<BanForm>) -> Response {
    if let Some(tok)=get_cookie(&headers, "adm") { if !state.is_admin(&tok).await { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); } } else { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); }
    let ip = frm.ip.trim(); if ip.is_empty() { return json_error(StatusCode::BAD_REQUEST, "missing", "missing ip"); }
    state.add_ban(ip.to_string(), frm.reason.unwrap_or_default()).await; state.persist_bans().await; (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, HeaderValue::from_static("/ban"))]).into_response()
}

#[axum::debug_handler]
pub async fn unban_post_handler(State(state): State<AppState>, headers: HeaderMap, Form(frm): Form<UnbanForm>) -> Response {
    if let Some(tok)=get_cookie(&headers, "adm") { if !state.is_admin(&tok).await { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); } } else { return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required"); }
    let ip = frm.ip.trim(); if ip.is_empty() { return json_error(StatusCode::BAD_REQUEST, "missing", "missing ip"); }
    state.remove_ban(ip).await; state.persist_bans().await; (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, HeaderValue::from_static("/ban"))]).into_response()
}

pub fn build_router(state: AppState) -> Router {
    let static_root = state.static_dir.clone();
    let css_service = ServeDir::new(static_root.join("css"));
    let js_service = ServeDir::new(static_root.join("js"));
    Router::new()
        .route("/upload", post(upload_handler))
        .route("/list", get(list_handler))
        .route("/mine", get(list_handler)) // alias for legacy frontend expecting /mine
        .route("/f/{file}", get(fetch_file_handler).delete(delete_handler))
        .route("/d/{file}", delete(delete_handler)) // legacy delete alias
        .route("/report", get(report_page_handler).post(report_handler))
        .route("/ban", get(ban_page_handler).post(ban_post_handler))
        .route("/unban", post(unban_post_handler))
        .route("/healthz", get(|| async { "ok" }))
        .route("/simple", get(simple_list_handler))
        .route("/simple_delete", post(simple_delete_handler))
        .route("/auth", get(auth_get_handler).post(auth_post_handler))
        .route("/isadmin", get(is_admin_handler))
        .route("/debug-ip", get(debug_ip_handler))
        .nest_service("/css", css_service.clone())
        .nest_service("/js", js_service.clone())
        .route("/", get(root_handler))
        .route("/{*path}", get(file_handler))
        .with_state(state)
}

// --- Admin auth handlers ---

// Reads admin key JSON from state; rotates if expired.
fn load_admin_key() -> Option<String> { None }

pub async fn auth_get_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(tok) = get_cookie(&headers, "adm") { if state.is_admin(&tok).await { return (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/html")], "<html><body><p>Already admin.</p><a href=/isadmin>Check</a></body></html>").into_response(); } }
    let body = "<html><body><form method=post action=/auth><input type=password name=key autofocus placeholder=Admin+Key><button type=submit>Auth</button></form></body></html>";
    (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/html")], body).into_response()
}

pub async fn auth_post_handler(State(state): State<AppState>, _headers: HeaderMap, Form(frm): Form<AdminAuthForm>) -> Response {
    let submitted = frm.key.trim();
    if submitted.is_empty() { return json_error(StatusCode::BAD_REQUEST, "missing", "missing key"); }
    // read current key from state
    let current_key = { state.admin_key.read().await.clone() };
    if current_key.is_empty() { return json_error(StatusCode::INTERNAL_SERVER_ERROR, "no_key", "admin key unavailable"); }
    if subtle_equals(submitted.as_bytes(), current_key.as_bytes()) {
        let token = crate::util::random_name(32);
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

pub async fn add_security_headers(req: axum::http::Request<Body>, next: Next) -> Response { let mut resp=next.run(req).await; let h=resp.headers_mut(); if !h.contains_key("X-Content-Type-Options") { h.insert("X-Content-Type-Options", "nosniff".parse().unwrap()); } if !h.contains_key("X-Frame-Options") { h.insert("X-Frame-Options", "DENY".parse().unwrap()); } if !h.contains_key("Referrer-Policy") { h.insert("Referrer-Policy", "no-referrer".parse().unwrap()); } if !h.contains_key("Content-Security-Policy") { h.insert("Content-Security-Policy", "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:".parse().unwrap()); } if !h.contains_key("Cache-Control") { h.insert("Cache-Control", "private, max-age=0, no-store".parse().unwrap()); } if !h.contains_key("Permissions-Policy") { h.insert("Permissions-Policy", "camera=(), microphone=(), geolocation=(), fullscreen=(), payment=()".parse().unwrap()); } resp }

pub async fn enforce_host(req: axum::http::Request<Body>, next: Next) -> Response { let host = req.headers().get("host").and_then(|h| h.to_str().ok()).unwrap_or_default(); if host == PROD_HOST { next.run(req).await } else { let uri = format!("https://{}{}", PROD_HOST, req.uri().path_and_query().map(|pq| pq.as_str()).unwrap_or("/")); let hv = HeaderValue::from_str(&uri).unwrap(); (StatusCode::MOVED_PERMANENTLY, [(axum::http::header::LOCATION, hv)]).into_response() } }

// Global middleware: if IP banned, immediately return a themed banned page.
pub async fn ban_gate(State(state): State<AppState>, ConnectInfo(addr): ConnectInfo<ClientAddr>, req: Request<Body>, _next: Next) -> Response {
    let path = req.uri().path();
    // Allow core static assets through (styling / basic JS) even if banned
    if path.starts_with("/css/") || path.starts_with("/js/") { return _next.run(req).await; }
    let ip = extract_client_ip(req.headers(), Some(addr.ip()));
    if !state.is_banned(&ip).await { return _next.run(req).await; }
    let (reason,time) = { let bans=state.bans.read().await; if let Some(b)=bans.iter().find(|b| b.ip==ip) { (b.reason.clone(), b.time) } else { (String::new(), 0) } };
    let safe_reason = htmlescape::encode_minimal(&reason);
    let body = format!(r#"<!DOCTYPE html><html lang=en><head><meta charset=utf-8><title>Access Restricted</title><meta name=viewport content="width=device-width,initial-scale=1" /><link rel=stylesheet href=/css/app.css /><style>body{{display:flex;flex-direction:column;min-height:100vh;}} main{{width:min(760px,90%);margin:2rem auto;}} .panel{{animation:fadeIn .6s var(--e-out);}} h1{{font-size:clamp(2rem,4vw,3.1rem);margin:0 0 .8rem;}} .reason{{background:#223038;border:1px solid #33464f;padding:.85rem 1rem;border-radius:12px;font-size:.75rem;line-height:1.4;letter-spacing:.4px;margin:1.1rem 0 0;}} @keyframes fadeIn{{from{{opacity:0;transform:translateY(6px);}}to{{opacity:1;transform:translateY(0);}}}} .mini{{opacity:.6;font-size:.6rem;margin-top:1.2rem;}} .cta{{margin-top:1.4rem;font-size:.7rem;}} .code{{font-family:monospace;font-size:.7rem;}} footer{{text-align:center;margin:2rem 0 1rem;font-size:.55rem;opacity:.45;}}</style></head><body><header style="text-align:center;padding:1.9rem 1rem 1rem;"><h1>You've Been Banned</h1><p class=lead style="margin:0;font-size:.95rem;color:var(--text-subtle);">Access to this service is currently revoked for your IP.</p></header><main><div class=panel><section style="padding:1.3rem 1.4rem 1.6rem;"><p style="font-size:.8rem;line-height:1.5;margin:0 0 .9rem;">If you believe this is an error you may wait or contact the operator. Routine evasion attempts are discouraged and usually logged.</p><div class=reason><strong>Reason:</strong> {safe_reason}<br><span class=code>IP: {ip}</span>{time_line}</div><div class=cta><a href="/" style="color:var(--accent);">Return home</a> (will still be blocked).</div><p class=mini>Why this style? Because a plain 403 is boring. Improve your behavior and this message might go away.</p></section></div></main><footer>juicebox // banned</footer></body></html>"#, safe_reason=safe_reason, ip=ip, time_line=if time>0 { format!("<br><span class=code>Time: {time}</span>") } else { String::new() });
    (StatusCode::FORBIDDEN, [(axum::http::header::CONTENT_TYPE, "text/html")], body).into_response()
}
