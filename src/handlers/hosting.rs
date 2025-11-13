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
use tracing::{debug, info, trace, warn};

use crate::state::{AppState, cleanup_expired};
use crate::util::{format_bytes, json_error, max_file_bytes, now_secs};

const DEFAULT_QUOTA_MESSAGE: &str =
    "Maximum storage quota has been reached. You cannot upload for now.";

#[derive(Serialize)]
pub struct ConfigResponse {
    pub max_file_bytes: u64,
    pub max_file_size_str: String,
    pub enable_streaming_uploads: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<FrontendTelemetry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota: Option<FrontendQuota>,
}

#[derive(Serialize)]
pub struct FrontendTelemetry {
    pub sentry: FrontendSentryTelemetry,
}

#[derive(Serialize)]
pub struct FrontendSentryTelemetry {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dsn: Option<String>,
    pub release: String,
    pub environment: String,
    #[serde(rename = "traces_sample_rate")]
    pub traces_sample_rate: f32,
    #[serde(rename = "profiles_sample_rate")]
    pub profiles_sample_rate: f32,
    #[serde(default)]
    pub trace_propagation_targets: Vec<String>,
}

#[derive(Serialize)]
pub struct FrontendQuota {
    pub max_bytes: u64,
    pub used_bytes: u64,
    pub remaining_bytes: u64,
    pub uploads_blocked: bool,
    pub max_bytes_str: String,
    pub used_bytes_str: String,
    pub remaining_bytes_str: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

fn compute_frontend_quota(state: &AppState, now: u64) -> Option<FrontendQuota> {
    let threshold_opt = state.storage_quota_block_threshold();
    state.storage_quota_limit().map(|limit| {
        let used = state.global_reserved_storage_bytes(now);
        let remaining = limit.saturating_sub(used);
        let threshold = threshold_opt.unwrap_or(limit);
        let blocked = used >= threshold;
        FrontendQuota {
            max_bytes: limit,
            used_bytes: used,
            remaining_bytes: remaining,
            uploads_blocked: blocked,
            max_bytes_str: format_bytes(limit),
            used_bytes_str: format_bytes(used),
            remaining_bytes_str: format_bytes(remaining),
            message: blocked.then(|| DEFAULT_QUOTA_MESSAGE.to_string()),
        }
    })
}

#[axum::debug_handler]
#[tracing::instrument(name = "files.fetch", skip(state), fields(file = %file))]
pub async fn fetch_file_handler(
    State(state): State<AppState>,
    Path(file): Path<String>,
) -> Response {
    trace!(file = %file, "fetch file request received");
    if file.contains('/') {
        warn!(file = %file, "fetch rejected: invalid path");
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
        debug!(file = %file, expired, "fetch request for missing or expired file");
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let file_path = state.upload_dir.join(&file);
    if !file_path.exists() {
        warn!(path = ?file_path, "fetch request missing file on disk");
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
            info!(file = %file, size = bytes.len(), "serving file");
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
        warn!(path = %path, "static file request rejected: traversal attempt");
        return (StatusCode::BAD_REQUEST, "bad path").into_response();
    }
    let mut candidate = state.static_dir.join(rel);
    if !candidate.exists() {
        if !rel.is_empty() && !rel.contains('.') {
            let alt = state.static_dir.join(format!("{}.html", rel));
            if alt.exists() {
                candidate = alt;
            } else {
                debug!(request = %rel, "static asset not found");
                return (StatusCode::NOT_FOUND, "not found").into_response();
            }
        } else {
            debug!(request = %rel, "static asset not found");
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
            trace!(path = ?candidate, max_age, "applied cache headers to static asset");
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
                        debug!(path = ?br_path, encoding = "br", "serving precompressed asset");
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
                        debug!(path = ?gz_path, encoding = "gzip", "serving precompressed asset");
                        return (resp_headers, bytes).into_response();
                    }
                    Err(_) => {}
                }
            }
        }
    }

    match fs::read(&candidate).await {
        Ok(bytes) => {
            info!(path = ?candidate, size = bytes.len(), "serving static asset");
            (resp_headers, bytes).into_response()
        }
        Err(err) => {
            warn!(?err, path = ?candidate, "failed to read static asset");
            (StatusCode::INTERNAL_SERVER_ERROR, "cant read file").into_response()
        }
    }
}

pub async fn config_handler(State(state): State<AppState>) -> Response {
    let streaming_opt_in = env::var("ENABLE_STREAMING_UPLOADS")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    let telemetry = state.telemetry.as_ref();
    let sentry_enabled = telemetry.sentry_enabled();
    let telemetry_payload = FrontendTelemetry {
        sentry: FrontendSentryTelemetry {
            enabled: sentry_enabled,
            dsn: telemetry.sentry_dsn.clone(),
            release: telemetry.release.clone(),
            environment: telemetry.environment.clone(),
            traces_sample_rate: telemetry.traces_sample_rate,
            profiles_sample_rate: telemetry.profiles_sample_rate,
            trace_propagation_targets: telemetry.trace_propagation_targets.clone(),
        },
    };
    state.cleanup_chunk_sessions().await;
    let now = now_secs();
    let quota = compute_frontend_quota(&state, now);
    let resp = ConfigResponse {
        max_file_bytes: max_file_bytes(),
        max_file_size_str: format_bytes(max_file_bytes()),
        enable_streaming_uploads: streaming_opt_in,
        telemetry: Some(telemetry_payload),
        quota,
    };
    debug!(
        enable_streaming_uploads = resp.enable_streaming_uploads,
        max_file_bytes = resp.max_file_bytes,
        sentry_enabled,
        "serving config"
    );
    Json(resp).into_response()
}

#[derive(Serialize)]
pub struct QuotaResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota: Option<FrontendQuota>,
}

pub async fn quota_handler(State(state): State<AppState>) -> Response {
    state.cleanup_chunk_sessions().await;
    let now = now_secs();
    let quota = compute_frontend_quota(&state, now);
    let resp = QuotaResponse { quota };
    debug!(
        uploads_blocked = resp
            .quota
            .as_ref()
            .map(|q| q.uploads_blocked)
            .unwrap_or(false),
        "serving quota status"
    );
    Json(resp).into_response()
}
