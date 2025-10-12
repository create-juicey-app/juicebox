use axum::Json;
use axum::extract::{Path, State};
use axum::http::header::{
    ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_TYPE, EXPIRES, VARY,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use mime_guess::MimeGuess;
use serde::Serialize;
use std::env;
use tokio::fs;

use crate::state::{AppState, cleanup_expired};
use crate::util::{format_bytes, json_error, max_file_bytes, now_secs};

#[derive(Serialize)]
pub struct ConfigResponse {
    pub max_file_bytes: u64,
    pub max_file_size_str: String,
    pub enable_streaming_uploads: bool,
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
            let mime = MimeGuess::from_path(&file_path).first_or_octet_stream();
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, mime.as_ref().parse().unwrap());
            if meta_expires > now {
                let remaining = meta_expires - now;
                // If the object expires far in the future, mark it immutable so CDNs cache aggressively.
                // Otherwise use the remaining TTL as max-age.
                if remaining > 60 * 60 * 24 * 7 {
                    // more than 7 days -> long cache
                    headers.insert(
                        CACHE_CONTROL,
                        HeaderValue::from_static("public, max-age=31536000, immutable"),
                    );
                } else {
                    headers.insert(
                        CACHE_CONTROL,
                        HeaderValue::from_str(&format!("public, max-age={}", remaining)).unwrap(),
                    );
                }
                let exp_time = std::time::SystemTime::UNIX_EPOCH
                    + std::time::Duration::from_secs(meta_expires);
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

pub async fn file_handler(
    State(state): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    let rel = path.trim_start_matches('/');
    if rel.contains("..") || rel.contains('\\') {
        return (StatusCode::BAD_REQUEST, "bad path").into_response();
    }
    let mut candidate = state.static_dir.join(rel);
    if !candidate.exists() {
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
    let accept = headers
        .get(ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let wants_br = accept.contains("br");
    let wants_gzip = accept.contains("gzip");

    let mime = MimeGuess::from_path(&candidate).first_or_octet_stream();
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(CONTENT_TYPE, mime.as_ref().parse().unwrap());

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
            let max_age = 86400;
            resp_headers.insert(
                CACHE_CONTROL,
                HeaderValue::from_str(&format!("public, max-age={max_age}")).unwrap(),
            );
            let exp_time = std::time::SystemTime::now() + std::time::Duration::from_secs(max_age);
            resp_headers.insert(
                EXPIRES,
                HeaderValue::from_str(&httpdate::fmt_http_date(exp_time)).unwrap(),
            );
        }
    }

    if wants_br {
        if let Some(ext) = candidate.extension().and_then(|e| e.to_str()) {
            let br_path = candidate.with_extension(format!("{ext}.br"));
            if fs::metadata(&br_path).await.is_ok() {
                match fs::read(&br_path).await {
                    Ok(bytes) => {
                        resp_headers.insert(CONTENT_ENCODING, HeaderValue::from_static("br"));
                        resp_headers.insert(VARY, HeaderValue::from_static("Accept-Encoding"));
                        return (resp_headers, bytes).into_response();
                    }
                    Err(_) => {}
                }
            }
        }
    }
    if wants_gzip {
        if let Some(ext) = candidate.extension().and_then(|e| e.to_str()) {
            let gz_path = candidate.with_extension(format!("{ext}.gz"));
            if fs::metadata(&gz_path).await.is_ok() {
                match fs::read(&gz_path).await {
                    Ok(bytes) => {
                        resp_headers.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
                        resp_headers.insert(VARY, HeaderValue::from_static("Accept-Encoding"));
                        return (resp_headers, bytes).into_response();
                    }
                    Err(_) => {}
                }
            }
        }
    }

    match fs::read(&candidate).await {
        Ok(bytes) => (resp_headers, bytes).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "cant read file").into_response(),
    }
}

pub async fn config_handler() -> Response {
    let streaming_opt_in = env::var("ENABLE_STREAMING_UPLOADS")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    let resp = ConfigResponse {
        max_file_bytes: max_file_bytes(),
        max_file_size_str: format_bytes(max_file_bytes()),
        enable_streaming_uploads: streaming_opt_in,
    };
    Json(resp).into_response()
}
