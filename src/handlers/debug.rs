use crate::{state::AppState, util::now_secs};
use axum::{
    body::Body,
    extract::{Query, State},
    http::{Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use once_cell::sync::Lazy;
use pprof::{ProfilerGuardBuilder, protos::Message};
use serde::Deserialize;
use serde_json::json;
use std::{convert::Infallible, time::Duration};
use tokio::{sync::Mutex, time::sleep};
use tracing::warn;

#[axum::debug_handler]
pub async fn debug_client_error() -> impl IntoResponse {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": "client_error", "message": "Intentional 400 for testing." })),
    )
}

#[axum::debug_handler]
pub async fn debug_custom_error() -> impl IntoResponse {
    (
        StatusCode::IM_A_TEAPOT,
        Json(json!({ "error": "teapot", "message": "Short and stout." })),
    )
}

#[axum::debug_handler]
pub async fn debug_rate_limit() -> impl IntoResponse {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({ "error": "rate_limited", "retry_after": 30 })),
    )
}

#[axum::debug_handler]
pub async fn debug_server_error() -> impl IntoResponse {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "server_error", "message": "Intentional 500 for testing." })),
    )
}

#[axum::debug_handler]
pub async fn debug_panic() -> Response {
    warn!("debug endpoint: triggering panic");
    panic!("Debug endpoint: intentional panic for Sentry testing");
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProfileQuery {
    #[serde(default)]
    pub seconds: Option<u64>,
    #[serde(default)]
    pub frequency: Option<u32>,
    #[serde(default)]
    pub format: Option<String>,
}

const DEFAULT_PROFILE_SECONDS: u64 = 15;
const MAX_PROFILE_SECONDS: u64 = 60;
const MIN_PROFILE_SECONDS: u64 = 1;
const DEFAULT_FREQUENCY: i32 = 99;
const MIN_FREQUENCY: i32 = 10;
const MAX_FREQUENCY: i32 = 2000;

static PROFILER_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

async fn capture_profile(duration: Duration, frequency: i32) -> Result<pprof::Report, String> {
    let guard = ProfilerGuardBuilder::default()
        .frequency(frequency)
        .build()
        .map_err(|err| format!("failed to start profiler: {err}"))?;

    sleep(duration).await;

    tokio::task::spawn_blocking(move || guard.report().build())
        .await
        .map_err(|err| format!("join error: {err}"))?
        .map_err(|err| format!("failed to build profiling report: {err}"))
}

fn parse_options(options: ProfileQuery) -> (Duration, i32) {
    let secs = options
        .seconds
        .unwrap_or(DEFAULT_PROFILE_SECONDS)
        .clamp(MIN_PROFILE_SECONDS, MAX_PROFILE_SECONDS);
    let freq = options
        .frequency
        .map(|value| value as i32)
        .unwrap_or(DEFAULT_FREQUENCY)
        .clamp(MIN_FREQUENCY, MAX_FREQUENCY);
    (Duration::from_secs(secs), freq)
}

fn profiler_busy_response() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "error": "profiler_busy",
            "message": "Another profile session is already running. Try again shortly.",
        })),
    )
        .into_response()
}

fn profiler_error_response(err: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "profile_failed", "message": err })),
    )
        .into_response()
}

pub async fn debug_profile_pprof(Query(options): Query<ProfileQuery>) -> Response {
    let format = options
        .format
        .clone()
        .unwrap_or_else(|| "protobuf".to_string())
        .to_ascii_lowercase();
    let (duration, frequency) = parse_options(options);

    let guard = PROFILER_LOCK.try_lock();
    let _lock = match guard {
        Ok(lock) => lock,
        Err(_) => return profiler_busy_response(),
    };

    let report = match capture_profile(duration, frequency).await {
        Ok(report) => report,
        Err(err) => return profiler_error_response(err),
    };

    let timestamp = now_secs();
    match format.as_str() {
        "protobuf" | "pprof" | "pb" => {
            let profile = match report.pprof() {
                Ok(profile) => profile,
                Err(err) => {
                    return profiler_error_response(format!("failed to build profile: {err}"));
                }
            };
            let mut body = Vec::new();
            if let Err(err) = profile.encode(&mut body) {
                return profiler_error_response(format!("failed to encode profile: {err}"));
            }
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header(
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"cpu-profile-{timestamp}.pb\""),
                )
                .body(Body::from(body))
                .unwrap_or_else(|err| {
                    profiler_error_response(format!("failed to build response: {err}"))
                })
        }
        other => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_format", "message": format!("unsupported format: {other}") })),
        )
            .into_response(),
    }
}

pub async fn block_debug_endpoints(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, Infallible> {
    let path = request.uri().path();
    if state.production && path.starts_with("/debug") {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    Ok(next.run(request).await)
}
