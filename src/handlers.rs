// --- CheckHash API for client-side deduplication ---
use axum::extract::Query as AxumQuery;
use mime_guess::mime;

#[derive(Deserialize)]
pub struct CheckHashQuery {
    pub hash: String,
}

pub async fn checkhash_handler(
    State(state): State<AppState>,
    AxumQuery(query): AxumQuery<CheckHashQuery>,
) -> Response {
    let exists = state
        .owners
        .iter()
        .any(|entry| entry.value().hash == query.hash);
    Json(json!({ "exists": exists })).into_response()
}
use crate::state::{
    cleanup_expired, spawn_integrity_check, verify_user_entries_with_report, AppState, FileMeta,
    ReconcileReport, ReportRecord,
};
use crate::util::extract_client_ip;
use crate::util::{
    format_bytes, get_cookie, is_forbidden_extension, json_error, make_storage_name,
    max_file_bytes, now_secs, qualify_path, real_client_ip, ttl_to_duration, ADMIN_SESSION_TTL,
    PROD_HOST,
};
use axum::body::Body;
use axum::http::Request;
use axum::{
    extract::{ConnectInfo, Form, Multipart, Path, Query, State},
    http::{
        header::{CACHE_CONTROL, EXPIRES},
        HeaderMap, HeaderValue, StatusCode,
    },
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::{
    net::SocketAddr as ClientAddr,
    time::{Duration, SystemTime},
};
use tera::Context;
use time::OffsetDateTime;
use tokio::fs;
use tower_http::services::ServeDir;

// Email event for reports
#[derive(Clone, Debug)]
pub struct ReportRecordEmail {
    pub file: String,
    pub reason: String,
    pub details: String,
    pub ip: String, // reporter ip
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
#[derive(Serialize, Deserialize)]
pub struct UploadResponse {
    pub files: Vec<String>,
    pub truncated: bool,
    pub remaining: usize,
}
#[derive(Serialize)]
pub struct ListResponse {
    pub files: Vec<String>,
    pub metas: Vec<FileMetaEntry>,
    pub reconcile: Option<ReconcileReport>,
}
#[derive(Serialize)]
pub struct FileMetaEntry {
    pub file: String,
    pub expires: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub original: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<u64>,
}

#[derive(Deserialize)]
pub struct ReportForm {
    pub file: String,
    pub reason: String,
    pub details: Option<String>,
}
#[derive(Deserialize)]
pub struct SimpleQuery {
    pub m: Option<String>,
}
#[derive(Deserialize)]
pub struct SimpleDeleteForm {
    pub f: String,
}
#[derive(Deserialize)]
pub struct AdminAuthForm {
    pub key: String,
}
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
pub struct AdminFileDeleteForm {
    pub file: String,
}
#[derive(Deserialize)]
pub struct AdminReportDeleteForm {
    pub idx: usize,
}
#[derive(Debug, Deserialize)]
pub struct LangQuery {
    pub lang: Option<String>,
    pub m: Option<String>,
    pub deleted: Option<String>,
}

// Config response
#[derive(Serialize)]
pub struct ConfigResponse {
    pub max_file_bytes: u64,
    pub max_file_size_str: String,
}

// Upload handler
#[axum::debug_handler]
pub async fn upload_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    if state.is_banned(&real_client_ip(&headers, &addr)).await {
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let ip = real_client_ip(&headers, &addr);
    let sem = state.upload_sem.clone();
    let _permit = match sem.try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "busy",
                "server is busy, try again later",
            )
        }
    };

    let mut ttl_code = "24h".to_string();
    // --- forbidden file check and file collection ---
    let mut files_to_process = Vec::new();
    let mut pending_files = Vec::new();
    let mut forbidden_error: Option<String> = None;

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
            let content_type = field.content_type().map(|m| m.to_string());
            if let Ok(data) = field.bytes().await {
                if !data.is_empty() {
                    pending_files.push((original_name, content_type, data));
                }
            }
        }
    }

    let mut has_forbidden = false;
    for (original_name, _content_type, data) in &pending_files {
        // Check forbidden by extension (filename)
        if let Some(orig) = original_name {
            if is_forbidden_extension(orig) {
                tracing::warn!(?original_name, "Upload rejected: forbidden file extension");
                forbidden_error = Some("File type not allowed (forbidden extension)".to_string());
                has_forbidden = true;
                break;
            }
        }
        // Check forbidden by content (infer)
        let is_forbidden_content = if let Some(kind) = infer::get(&data) {
            let ext = kind.extension();
            crate::util::FORBIDDEN_EXTENSIONS.contains(&ext)
        } else {
            false
        };
        if is_forbidden_content {
            tracing::warn!(
                ?original_name,
                "Upload rejected: forbidden file content detected by infer"
            );
            forbidden_error = Some("File type not allowed (forbidden content)".to_string());
            has_forbidden = true;
            break;
        }
    }

    for (original_name, _content_type, data) in pending_files {
        files_to_process.push((original_name, data));
    }

    // Priority: forbidden filetype error > no files error
    if has_forbidden {
        let msg = forbidden_error.unwrap_or_else(|| "File type not allowed".to_string());
        return json_error(
            StatusCode::BAD_REQUEST,
            "bad_filetype",
            match msg.as_str() {
                "File type not allowed (forbidden content)" => {
                    "File type not allowed (forbidden content)"
                }
                "File type not allowed (forbidden MIME type)" => {
                    "File type not allowed (forbidden MIME type)"
                }
                _ => "File type not allowed",
            },
        );
    }

    if files_to_process.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "no_files",
            "no files were uploaded",
        );
    }

    // --- file saving and duplicate detection ---
    let now = now_secs();
    let ttl = ttl_to_duration(&ttl_code).as_secs();
    let expires = now + ttl;
    let mut saved_files = Vec::new();
    let mut duplicate_info = None;

    for (original_name, data) in &files_to_process {
        if data.len() as u64 > max_file_bytes() {
            tracing::warn!(ip = %ip, ?original_name, size = data.len(), "Upload rejected: file too large");
            continue;
        }
        // Compute SHA-256 hash
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let hash = format!("{:x}", hasher.finalize());
        // Check for duplicate in file_owners.json
        if let Some(entry) = state.owners.iter().find(|entry| entry.value().hash == hash) {
            tracing::info!(ip = %ip, ?original_name, file = %entry.key(), "Duplicate upload detected");
            duplicate_info = Some(serde_json::json!({
                "duplicate": true,
                "file": entry.key(),
                "meta": entry.value()
            }));
            continue;
        }
        let storage_name = make_storage_name(original_name.as_deref());
        if is_forbidden_extension(&storage_name) {
            tracing::warn!(ip = %ip, ?original_name, file = %storage_name, "Upload rejected: forbidden extension");
            continue;
        }
        let path = state.upload_dir.join(&storage_name);
        if fs::write(&path, data).await.is_ok() {
            let meta = FileMeta {
                hash: hash.clone(),
                created: now,
                expires,
                owner: ip.clone(),
                original: original_name.clone().unwrap_or_default(),
            };
            state.owners.insert(storage_name.clone(), meta);
            tracing::info!(ip = %ip, file = %storage_name, size = data.len(), "File uploaded successfully");
            saved_files.push(storage_name);
        } else {
            tracing::error!(ip = %ip, file = %storage_name, "Failed to write uploaded file");
        }
    }

    state.persist_owners().await;
    spawn_integrity_check(state.clone());

    if let Some(dup) = duplicate_info {
        return (StatusCode::CONFLICT, Json(dup)).into_response();
    }

    let truncated = saved_files.len() < files_to_process.len();
    let remaining = files_to_process.len() - saved_files.len();

    (
        StatusCode::OK,
        Json(UploadResponse {
            files: saved_files,
            truncated,
            remaining,
        }),
    )
        .into_response()
}

#[axum::debug_handler]
pub async fn list_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
) -> Response {
    if state.is_banned(&real_client_ip(&headers, &addr)).await {
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    cleanup_expired(&state).await;
    let client_ip = real_client_ip(&headers, &addr);
    let reconcile_report = verify_user_entries_with_report(&state, &client_ip).await;
    cleanup_expired(&state).await;
    crate::state::check_storage_integrity(&state).await;
    let mut files: Vec<(String, u64, String, u64, u64)> = state
        .owners
        .iter()
        .filter_map(|entry| {
            let m = entry.value();
            if m.owner == client_ip {
                let set = m.created;
                let total = if m.expires > set { m.expires - set } else { 0 };
                Some((
                    entry.key().clone(),
                    m.expires,
                    m.original.clone(),
                    total,
                    set,
                ))
            } else {
                None
            }
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let only_names: Vec<String> = files
        .iter()
        .map(|(n, _, _, _, _)| qualify_path(&state, &format!("f/{}", n)))
        .collect();
    let metas: Vec<FileMetaEntry> = files
        .into_iter()
        .map(|(n, e, o, t, s)| FileMetaEntry {
            file: qualify_path(&state, &format!("f/{}", n)),
            expires: e,
            original: o,
            total: Some(t),
            set: Some(s),
        })
        .collect();
    let body = Json(ListResponse {
        files: only_names,
        metas,
        reconcile: reconcile_report,
    });
    let mut resp = body.into_response();
    resp.headers_mut()
        .insert(CACHE_CONTROL, "no-store".parse().unwrap());
    resp
}

#[axum::debug_handler]
pub async fn fetch_file_handler(
    State(state): State<AppState>,
    Path(file): Path<String>,
) -> Response {
    if file.contains('/') {
        return (StatusCode::BAD_REQUEST, "bad file").into_response();
    }
    cleanup_expired(&state).await;
    let now = now_secs();
    let (exists, expired, meta_expires) = {
        if let Some(m) = state.owners.get(&file) {
            let m = m.value();
            (true, m.expires <= now, m.expires)
        } else {
            (false, true, 0)
        }
    };
    if !exists || expired {
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
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                mime.as_ref().parse().unwrap(),
            );
            // derive remaining TTL for caching
            if meta_expires > now {
                let remaining = meta_expires - now;
                headers.insert(
                    CACHE_CONTROL,
                    HeaderValue::from_str(&format!("public, max-age={}", remaining)).unwrap(),
                );
                let exp_time = SystemTime::UNIX_EPOCH + Duration::from_secs(meta_expires);
                headers.insert(
                    EXPIRES,
                    HeaderValue::from_str(&httpdate::fmt_http_date(exp_time)).unwrap(),
                );
            }
            (headers, bytes).into_response()
        }
        Err(_) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "fs_error",
            "cant read file",
        ),
    }
}

#[axum::debug_handler]
pub async fn delete_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    Path(file): Path<String>,
) -> Response {
    let ip = real_client_ip(&headers, &addr);
    if state.is_banned(&ip).await {
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    if file.contains('/') || file.contains("..") || file.contains('\\') {
        return json_error(StatusCode::BAD_REQUEST, "bad_file", "invalid file name");
    }
    cleanup_expired(&state).await;
    match state.owners.get(&file) {
        Some(meta) if meta.value().owner == ip => {}
        _ => return (StatusCode::NOT_FOUND, "not found").into_response(),
    }
    state.owners.remove(&file);
    // Remove hash entry
    // No more file_hashes or persist_hashes; hashes are part of FileMeta now.
    let path = state.upload_dir.join(&file);
    let _ = fs::remove_file(&path).await;
    state.persist_owners().await;
    (StatusCode::NO_CONTENT, ()).into_response()
}

#[axum::debug_handler]
pub async fn report_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    Form(form): Form<ReportForm>,
) -> Response {
    if state.is_banned(&real_client_ip(&headers, &addr)).await {
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let ip = real_client_ip(&headers, &addr);
    let now = now_secs();
    // Canonicalize file id: if exact not found and no extension supplied, try auto-matching stored file with extension.
    let mut file_name = form.file.trim().to_string();
    if state.owners.get(&file_name).is_none() && !file_name.contains('.') {
        // collect candidates that start with "{id}." (single extension segment)
        let prefix = format!("{file_name}.");
        let mut candidates: Vec<String> = state
            .owners
            .iter()
            .filter_map(|entry| {
                let k = entry.key();
                if k.starts_with(&prefix) {
                    Some(k.clone())
                } else {
                    None
                }
            })
            .collect();
        // deterministic selection: pick shortest name (i.e., shortest extension); then lexicographically
        candidates.sort();
        candidates.sort_by_key(|k| k.len());
        if let Some(best) = candidates.first() {
            file_name = best.clone();
        }
    }
    let record = ReportRecord {
        file: file_name.clone(),
        reason: form.reason.clone(),
        details: form.details.clone().unwrap_or_default(),
        ip: ip.clone(),
        time: now,
    };
    let (owner_ip, original_name, expires, size) = {
        if let Some(meta) = state.owners.get(&record.file) {
            let meta = meta.value();
            let path = state.upload_dir.join(&record.file);
            let sz = tokio::fs::metadata(&path)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            (meta.owner.clone(), meta.original.clone(), meta.expires, sz)
        } else {
            (String::new(), String::new(), 0u64, 0u64)
        }
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
        let iso = OffsetDateTime::from_unix_timestamp(now as i64)
            .map(|t| {
                t.format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let _ = tx
            .send(ReportRecordEmail {
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
            })
            .await;
    }
    (StatusCode::NO_CONTENT, ()).into_response()
}

pub async fn file_handler(State(state): State<AppState>, Path(path): Path<String>) -> Response {
    // normalize and security checks + extensionless .html support
    let rel = path.trim_start_matches('/');
    if rel.contains("..") || rel.contains('\\') {
        return (StatusCode::BAD_REQUEST, "bad path").into_response();
    }
    let mut candidate = state.static_dir.join(rel);
    if !candidate.exists() {
        // try mapping extensionless request to .html file
        if !rel.is_empty() && !rel.contains('.') {
            let alt = state.static_dir.join(format!("{}.html", rel));
            if alt.exists() {
                candidate = alt;
            } else {
                return (StatusCode::NOT_FOUND, "not found").into_response();
            }
        } else {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
    }
    match fs::read(&candidate).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&candidate).first_or_octet_stream();
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                mime.as_ref().parse().unwrap(),
            );
            // apply cache policy based on extension
            if let Some(ext) = candidate.extension().and_then(|e| e.to_str()) {
                let cacheable = matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "css"
                        | "js"
                        | "webp"
                        | "png"
                        | "jpg"
                        | "jpeg"
                        | "gif"
                        | "svg"
                        | "ico"
                        | "woff"
                        | "woff2"
                );
                if cacheable {
                    let max_age = 86400; // 1 day
                    headers.insert(
                        CACHE_CONTROL,
                        HeaderValue::from_str(&format!("public, max-age={max_age}")).unwrap(),
                    );
                    let exp_time = SystemTime::now() + Duration::from_secs(max_age);
                    headers.insert(
                        EXPIRES,
                        HeaderValue::from_str(&httpdate::fmt_http_date(exp_time)).unwrap(),
                    );
                }
            }
            (headers, bytes).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "cant read file").into_response(),
    }
}

pub async fn root_handler(
    State(state): State<AppState>,
    Query(query): Query<LangQuery>,
) -> Response {
    // Determine language (default to en)
    let lang = query.lang.as_deref().unwrap_or("en");
    let lang_file = format!("translations/lang_{}.toml", lang);
    // Try to load translation file, fallback to English if missing
    let t_map: HashMap<String, String> = {
        let content = match fs::read_to_string(&lang_file).await {
            Ok(s) => s,
            Err(_) => match fs::read_to_string("translations/lang_en.toml").await {
                Ok(s) => s,
                Err(_) => String::new(),
            },
        };
        toml::from_str(&content).unwrap_or_else(|_| HashMap::new())
    };
    let mut ctx = Context::new();
    ctx.insert("lang", lang);
    ctx.insert("t", &t_map);
    ctx.insert("max_file_bytes", &max_file_bytes());
    ctx.insert("max_file_size_str", &format_bytes(max_file_bytes()));
    let tera = &state.tera;
    match tera.render("index.html.tera", &ctx) {
        Ok(rendered) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "text/html")],
            rendered,
        )
            .into_response(),
        Err(e) => {
            eprintln!(
                "[Tera] Error rendering template 'index.html.tera': {e}\n{:#?}",
                e
            );
            // Tera::Error no longer exposes line information directly.
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("template error: {}", e),
            )
                .into_response()
        }
    }
}

pub async fn debug_ip_handler(
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
) -> Response {
    let edge = addr.ip().to_string();
    let cf = headers
        .get("CF-Connecting-IP")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    let xff = headers
        .get("X-Forwarded-For")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    Json(serde_json::json!({"edge": edge, "cf": cf, "xff": xff})).into_response()
}

// Helper to load translation map for a given lang code, with debug logging
async fn load_translation_map(lang: &str) -> HashMap<String, String> {
    let lang_file = format!("translations/lang_{}.toml", lang);
    println!("[i18n] Attempting to load translation file: {}", lang_file);
    let content = match fs::read_to_string(&lang_file).await {
        Ok(s) => {
            println!("[i18n] Loaded file: {}", lang_file);
            s
        }
        Err(e) => {
            println!(
                "[i18n] Failed to load {}: {}. Falling back to lang_en.toml",
                lang_file, e
            );
            match fs::read_to_string("translations/lang_en.toml").await {
                Ok(s) => s,
                Err(e2) => {
                    println!("[i18n] Failed to load fallback lang_en.toml: {}", e2);
                    String::new()
                }
            }
        }
    };
    match toml::from_str::<HashMap<String, String>>(&content) {
        Ok(map) => {
            println!("[i18n] Loaded {} keys for lang {}", map.len(), lang);
            map
        }
        Err(e) => {
            println!("[i18n] Failed to parse TOML for lang {}: {}", lang, e);
            HashMap::new()
        }
    }
}

async fn render_tera_page(
    state: &AppState,
    template: &str,
    lang: &str,
    extra: Option<(&str, &tera::Value)>,
) -> Response {
    let t_map = load_translation_map(lang).await;
    let mut ctx = Context::new();
    ctx.insert("lang", lang);
    ctx.insert("t", &t_map);
    if let Some((k, v)) = extra {
        ctx.insert(k, v);
    }
    let tera = &state.tera;
    match tera.render(template, &ctx) {
        Ok(rendered) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "text/html")],
            rendered,
        )
            .into_response(),
        Err(e) => {
            eprintln!(
                "[Tera] Error rendering template '{}': {e}\n{:#?}",
                template, e
            );
            // Tera::Error no longer exposes line information directly.
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("template error: {}", e),
            )
                .into_response()
        }
    }
}

pub async fn faq_handler(
    State(state): State<AppState>,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");
    render_tera_page(&state, "faq.html.tera", lang, None).await
}

pub async fn terms_handler(
    State(state): State<AppState>,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");
    render_tera_page(&state, "terms.html.tera", lang, None).await
}

pub async fn report_page_handler_i18n(
    State(state): State<AppState>,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");
    render_tera_page(&state, "report.html.tera", lang, None).await
}

use std::net::SocketAddr;
pub async fn simple_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");

    // Try to get a message from query param
    let message = if let Some(_) = query.deleted {
        Some("File DEleted Successfully.".to_string())
    } else {
        query.m.clone()
    };

    // For simple UI, show files for the real client IP (from request context)
    let client_ip = crate::util::real_client_ip(&headers, &addr);
    let mut files: Vec<(String, u64, String)> = state
        .owners
        .iter()
        .filter_map(|entry| {
            let m = entry.value();
            if m.owner == client_ip {
                Some((entry.key().clone(), m.expires, m.original.clone()))
            } else {
                None
            }
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let now = now_secs();
    let mut rows = String::new();
    for (fname, expires, original) in &files {
        let url = qualify_path(&state, &format!("f/{}", fname));
        let expired = now >= *expires;
        let expires_in = if *expires > now { *expires - now } else { 0 };
        let human = if expired {
            "expired".to_string()
        } else if expires_in >= 86400 {
            format!("{}d", expires_in / 86400)
        } else if expires_in >= 3600 {
            format!("{}h", expires_in / 3600)
        } else if expires_in >= 60 {
            format!("{}m", expires_in / 60)
        } else {
            format!("{}s", expires_in)
        };
        rows.push_str(&format!(
            "<tr><td><a href=\"{}\">{}</a></td><td>{}</td><td><a href=\"/simple/delete?f={}\" class=delete-link>Delete</a></td></tr>",
            url,
            htmlescape::encode_minimal(original),
            human,
            htmlescape::encode_minimal(fname)
        ));
    }
    let mut ctx = tera::Context::new();
    ctx.insert("lang", lang);
    ctx.insert("ROWS", &rows);
    if let Some(msg) = message {
        ctx.insert("MESSAGE", &msg);
    }
    // Insert translation map
    let t_map = load_translation_map(lang).await;
    ctx.insert("t", &t_map);
    let tera = &state.tera;
    match tera.render("simple.html.tera", &ctx) {
        Ok(rendered) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "text/html")],
            rendered,
        )
            .into_response(),
        Err(e) => {
            eprintln!(
                "[Tera] Error rendering template 'simple.html.tera': {e}\n{:#?}",
                e
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("template error: {}", e),
            )
                .into_response()
        }
    }
}

pub async fn banned_handler(
    State(state): State<AppState>,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");
    render_tera_page(&state, "banned.html.tera", lang, None).await
}

pub async fn ban_page_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    // admin gate
    if let Some(tok) = get_cookie(&headers, "adm") {
        if state.is_admin(&tok).await {
        } else {
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
        return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
    }
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
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "text/html")],
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
        [(
            axum::http::header::LOCATION,
            HeaderValue::from_static("/admin/ban"),
        )],
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
        [(
            axum::http::header::LOCATION,
            HeaderValue::from_static("/admin/ban"),
        )],
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
                    [(axum::http::header::CONTENT_TYPE, "text/html")],
                    bytes,
                )
                    .into_response();
            }
            return (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "text/html")],
                "<html><body><p>Already admin.</p><a href=/isadmin>Check</a></body></html>",
            )
                .into_response();
        }
    }
    let tpl_path = state.static_dir.join("admin_auth.html");
    match fs::read(&tpl_path).await {
        Ok(bytes) => (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/html")], bytes).into_response(),
        Err(_) => (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/html")], "<html><body><form method=post action=/auth><input type=password name=key autofocus placeholder=Admin+Key><button type=submit>Auth</button></form></body></html>").into_response(),
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
    // read current key from state
    let current_key = { state.admin_key.read().await.clone() };
    if current_key.is_empty() {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "no_key",
            "admin key unavailable",
        );
    }
    if subtle_equals(submitted.as_bytes(), current_key.as_bytes()) {
        let token = crate::util::new_id();
        state.create_admin_session(token.clone()).await;
        state.persist_admin_sessions().await;
        let cookie = format!(
            "adm={}; Path=/; HttpOnly; Max-Age={}; SameSite=Strict",
            token, ADMIN_SESSION_TTL
        );
        let mut resp = (
            StatusCode::SEE_OTHER,
            [(axum::http::header::LOCATION, HeaderValue::from_static("/"))],
        )
            .into_response();
        resp.headers_mut().append(
            axum::http::header::SET_COOKIE,
            HeaderValue::from_str(&cookie).unwrap(),
        );
        return resp;
    }
    json_error(StatusCode::UNAUTHORIZED, "invalid_key", "invalid key")
}

pub async fn is_admin_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(tok) = get_cookie(&headers, "adm") {
        if state.is_admin(&tok).await {
            return (StatusCode::OK, Json(serde_json::json!({"admin": true}))).into_response();
        }
    }
    (StatusCode::OK, Json(serde_json::json!({"admin": false}))).into_response()
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

pub async fn add_security_headers(req: axum::http::Request<Body>, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    if !h.contains_key("X-Content-Type-Options") {
        h.insert("Content-Security-Policy", "default-src 'self'; script-src 'self' 'unsafe-inline' https://static.cloudflareinsights.com; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:".parse().unwrap());
    }
    if !h.contains_key("Permissions-Policy") {
        h.insert(
            "Permissions-Policy",
            "camera=(), microphone=(), geolocation=(), fullscreen=(), payment=()"
                .parse()
                .unwrap(),
        );
    }
    // Ensure charset is specified for HTML responses
    if let Some(ct_val) = h
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        let ct_lower = ct_val.to_ascii_lowercase();
        if ct_lower.starts_with("text/html") && !ct_lower.contains("charset=") {
            h.insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
        }
    }
    resp
}

pub async fn enforce_host(req: axum::http::Request<Body>, next: Next) -> Response {
    let host = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    if host == PROD_HOST {
        next.run(req).await
    } else {
        let uri = format!(
            "https://{}{}",
            PROD_HOST,
            req.uri()
                .path_and_query()
                .map(|pq| pq.as_str())
                .unwrap_or("/")
        );
        let hv = HeaderValue::from_str(&uri).unwrap();
        (
            StatusCode::MOVED_PERMANENTLY,
            [(axum::http::header::LOCATION, hv)],
        )
            .into_response()
    }
}

// Global middleware: if IP banned, immediately return a themed banned page.
pub async fn ban_gate(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    req: Request<Body>,
    _next: Next,
) -> Response {
    let path = req.uri().path();
    if path.starts_with("/css/") || path.starts_with("/js/") {
        return _next.run(req).await;
    }
    let ip = extract_client_ip(req.headers(), Some(addr.ip()));
    if !state.is_banned(&ip).await {
        return _next.run(req).await;
    }
    let (reason, time) = {
        let bans = state.bans.read().await;
        if let Some(b) = bans.iter().find(|b| b.ip == ip) {
            (b.reason.clone(), b.time)
        } else {
            (String::new(), 0)
        }
    };
    let safe_reason = htmlescape::encode_minimal(&reason);
    let time_line = if time > 0 {
        format!("<br><span class=code>Time: {time}</span>")
    } else {
        String::new()
    };
    let tpl_path = state.static_dir.join("banned.html");
    if let Ok(bytes) = fs::read(&tpl_path).await {
        let mut body = String::from_utf8_lossy(&bytes).into_owned();
        body = body
            .replace("{{REASON}}", &safe_reason)
            .replace("{{IP}}", &ip)
            .replace("{{TIME_LINE}}", &time_line);
        return (
            StatusCode::FORBIDDEN,
            [(axum::http::header::CONTENT_TYPE, "text/html")],
            body,
        )
            .into_response();
    }
    let fallback = format!(
        "<html><body><h1>Banned</h1><p>{}</p><p>{}</p></body></html>",
        safe_reason, ip
    );
    (
        StatusCode::FORBIDDEN,
        [(axum::http::header::CONTENT_TYPE, "text/html")],
        fallback,
    )
        .into_response()
}

pub async fn admin_files_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(tok) = get_cookie(&headers, "adm") {
        if !state.is_admin(&tok).await {
            return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
        }
    } else {
        return json_error(StatusCode::UNAUTHORIZED, "not_admin", "auth required");
    }
    // Build table rows (file, owner, expires human, size)
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
                [(axum::http::header::CONTENT_TYPE, "text/html")],
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
    let hv = HeaderValue::from_static("/admin/files");
    (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, hv)]).into_response()
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
                [(axum::http::header::CONTENT_TYPE, "text/html")],
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
    let hv = HeaderValue::from_static("/admin/reports");
    (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, hv)]).into_response()
}

pub async fn config_handler() -> Response {
    let resp = ConfigResponse {
        max_file_bytes: crate::util::max_file_bytes(),
        max_file_size_str: crate::util::format_bytes(crate::util::max_file_bytes()),
    };
    Json(resp).into_response()
}

// Placeholder handlers for /simple endpoints
pub async fn simple_list_handler() -> axum::response::Response {
    (axum::http::StatusCode::NOT_IMPLEMENTED, "Not implemented").into_response()
}

#[axum::debug_handler]
pub async fn simple_upload_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let ip = real_client_ip(&headers, &addr);
    let mut ttl_code = "3d".to_string();
    let mut files_to_process = Vec::new();
    let mut forbidden_error: Option<String> = None;
    let mut has_forbidden = false;

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

        if name == "file" || name.starts_with("file") {
            let original_name = field.file_name().map(|s| s.to_string());
            let content_type = field.content_type().map(|m| m.to_string());
            if let Ok(data) = field.bytes().await {
                if !data.is_empty() {
                    // MIME type check against all forbidden extensions
                    let forbidden_mimes: Vec<mime::Mime> = crate::util::FORBIDDEN_EXTENSIONS
                        .iter()
                        .flat_map(|ext| {
                            mime_guess::from_ext(ext)
                                .iter()
                                .filter_map(|m| m.essence_str().parse().ok())
                        })
                        .collect();
                    let mime_type = if let Some(ref ct) = content_type {
                        ct.parse::<mime::Mime>().ok()
                    } else if let Some(ref orig) = original_name {
                        mime_guess::from_path(orig)
                            .first_raw()
                            .and_then(|m| m.parse().ok())
                    } else {
                        None
                    };
                    // Content-based detection using infer
                    let is_forbidden_content = if let Some(kind) = infer::get(&data) {
                        let ext = kind.extension();
                        crate::util::FORBIDDEN_EXTENSIONS.contains(&ext)
                    } else {
                        false
                    };
                    if is_forbidden_content {
                        tracing::warn!(
                            ?original_name,
                            "Simple upload rejected: forbidden file content detected by infer"
                        );
                        forbidden_error = Some("File type not allowed (forbidden content)".to_string());
                        has_forbidden = true;
                        break;
                    }
                    if let Some(mime) = &mime_type {
                        if forbidden_mimes.iter().any(|forb| forb == mime) {
                            tracing::warn!(?original_name, mime = %mime, "Simple upload rejected: forbidden MIME type");
                            forbidden_error = Some("File type not allowed (forbidden MIME type)".to_string());
                            has_forbidden = true;
                            break;
                        }
                    }
                    files_to_process.push((original_name, data));
                }
            }
        }
    }

    // Priority: forbidden filetype error > no files error
    if has_forbidden {
        let msg = forbidden_error.unwrap_or_else(|| "File type not allowed".to_string());
        return json_error(
            StatusCode::BAD_REQUEST,
            "bad_filetype",
            match msg.as_str() {
                "File type not allowed (forbidden content)" => {
                    "File type not allowed (forbidden content)"
                }
                "File type not allowed (forbidden MIME type)" => {
                    "File type not allowed (forbidden MIME type)"
                }
                _ => "File type not allowed",
            },
        );
    }

    if files_to_process.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "no_files",
            "no files were uploaded",
        );
    }

    let expires = now_secs() + ttl_to_duration(&ttl_code).as_secs();
    let mut saved_files = Vec::new();

    for (original_name, data) in &files_to_process {
        let now = now_secs();
        if data.len() as u64 > max_file_bytes() {
            tracing::warn!(ip = %ip, ?original_name, size = data.len(), "Simple upload rejected: file too large");
            continue;
        }
        // Compute SHA-256 hash
        let mut hasher = sha2::Sha256::new();
        hasher.update(&data);
        let hash = format!("{:x}", hasher.finalize());
        let storage_name = make_storage_name(original_name.as_deref());
        if is_forbidden_extension(&storage_name) {
            tracing::warn!(ip = %ip, ?original_name, file = %storage_name, "Simple upload rejected: forbidden extension");
            continue;
        }
        let path = state.upload_dir.join(&storage_name);
        if fs::write(&path, data).await.is_ok() {
            let meta = FileMeta {
                owner: ip.clone(),
                expires,
                original: original_name.clone().unwrap_or_default(),
                created: now,
                hash: hash.clone(),
            };
            state.owners.insert(storage_name.clone(), meta);
            tracing::info!(ip = %ip, file = %storage_name, size = data.len(), "Simple file uploaded successfully");
            saved_files.push(storage_name);
        } else {
            tracing::error!(ip = %ip, file = %storage_name, "Failed to write simple uploaded file");
        }
    }

    state.persist_owners().await;
    spawn_integrity_check(state.clone());

    let truncated = saved_files.len() < files_to_process.len();
    // For the simple UI, redirect back to /simple with a message
    let msg = if saved_files.is_empty() {
        "No files uploaded."
    } else if truncated {
        "Some files were too large or invalid and were skipped."
    } else {
        "Upload successful!"
    };
    let url = format!("/simple?m={}", urlencoding::encode(msg));
    (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, url)]).into_response()
}

// Accepts query param for GET
pub async fn simple_delete_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    Query(frm): Query<SimpleDeleteForm>,
) -> Response {
    handle_simple_delete(state, addr, headers, frm.f).await
}

// Accepts form data for POST
#[axum::debug_handler]
pub async fn simple_delete_post_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    Form(frm): Form<SimpleDeleteForm>,
) -> Response {
    handle_simple_delete(state, addr, headers, frm.f).await
}

async fn handle_simple_delete(
    state: AppState,
    addr: ClientAddr,
    headers: HeaderMap,
    f: String,
) -> Response {
    println!("[DEBUG] handle_simple_delete: called with f='{}'", f);
    let ip = real_client_ip(&headers, &addr);
    println!("[DEBUG] handle_simple_delete: real_client_ip='{}'", ip);
    let fname = f.trim();
    if fname.is_empty() || fname.contains('/') || fname.contains("..") || fname.contains('\\') {
        println!(
            "[DEBUG] handle_simple_delete: invalid file name '{}', returning error",
            fname
        );
        let url = format!("/simple?m={}", urlencoding::encode("Invalid file name."));
        return (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, url)]).into_response();
    }
    // Only allow delete if file is owned by this IP
    let can_delete = match state.owners.get(fname) {
        Some(meta) if meta.value().owner == ip => true,
        _ => false,
    };
    if can_delete {
        println!(
            "[DEBUG] handle_simple_delete: found file '{}' owned by '{}', deleting",
            fname, ip
        );
        state.owners.remove(fname);
        let path = state.upload_dir.join(fname);
        let _ = fs::remove_file(&path).await;
        println!(
            "[DEBUG] handle_simple_delete: file '{}' removed from disk (if existed)",
            fname
        );
        state.persist_owners().await;
        println!("[DEBUG] handle_simple_delete: owners persisted");
        let url = format!(
            "/simple?m={}",
            urlencoding::encode("File Deleted Successfully.")
        );
        println!("[DEBUG] handle_simple_delete: returning success redirect");
        (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, url)]).into_response()
    } else {
        println!("[DEBUG] handle_simple_delete: file '{}' not found or not owned by '{}', returning error", fname, ip);
        let url = format!(
            "/simple?m={}",
            urlencoding::encode("File not found or not owned by you.")
        );
        (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, url)]).into_response()
    }
}

// Add cache middleware for static asset dirs (css/js)
pub async fn add_cache_headers(req: axum::http::Request<Body>, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let mut resp = next.run(req).await;
    if (path.starts_with("/css/") || path.starts_with("/js/")) && !path.contains("../") {
        let headers = resp.headers_mut();
        let max_age = 86400; // 1 day (filenames not fingerprinted)
        headers.insert(
            CACHE_CONTROL,
            HeaderValue::from_str(&format!("public, max-age={max_age}")).unwrap(),
        );
        let exp_time = SystemTime::now() + Duration::from_secs(max_age as u64);
        headers.insert(
            EXPIRES,
            HeaderValue::from_str(&httpdate::fmt_http_date(exp_time)).unwrap(),
        );
    }
    resp
}

pub fn build_router(state: AppState) -> Router {
    let static_root = state.static_dir.clone();
    let css_service = ServeDir::new(static_root.join("css"));
    let js_service = ServeDir::new(static_root.join("js"));
    Router::new()
        .route("/checkhash", get(checkhash_handler))
        .route("/upload", post(upload_handler))
        .route("/list", get(list_handler))
        .route("/mine", get(list_handler))
        .route("/f/{file}", get(fetch_file_handler).delete(delete_handler))
        .route("/d/{file}", delete(delete_handler))
        .route(
            "/report",
            get(report_page_handler_i18n).post(report_handler),
        )
        .route("/unban", post(unban_post_handler))
        .route("/healthz", get(|| async { "ok" }))
        .route("/simple", get(simple_handler))
        .route("/simple/upload", post(simple_upload_handler))
        .route(
            "/simple/delete",
            get(simple_delete_handler).post(simple_delete_post_handler),
        )
        .route("/auth", get(auth_get_handler).post(auth_post_handler))
        .route("/isadmin", get(is_admin_handler))
        .route("/debug-ip", get(debug_ip_handler))
        .route("/admin/ban", get(ban_page_handler).post(ban_post_handler))
        .route(
            "/admin/files",
            get(admin_files_handler).post(admin_file_delete_handler),
        )
        .route(
            "/admin/reports",
            get(admin_reports_handler).post(admin_report_delete_handler),
        )
        .route("/faq", get(faq_handler))
        .route("/terms", get(terms_handler))
        .route("/api/config", get(config_handler))
        .nest_service("/css", css_service.clone())
        .nest_service("/js", js_service.clone())
        .route("/", get(root_handler))
        .route("/{*path}", get(file_handler))
        .with_state(state)
}
