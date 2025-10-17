use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Json, Response},
};
use crate::state::AppState;
use std::convert::Infallible;
use serde_json::json;
use tracing::warn;
static DEBUG_ERROR_HTML: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/public/debug-error.html"));

#[axum::debug_handler]
pub async fn debug_error_page() -> impl IntoResponse {
    Html(DEBUG_ERROR_HTML)
}

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

pub async fn block_debug_endpoints(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, Infallible> {
    let path = request.uri().path();
    if state.production
        && (path.starts_with("/debug") || path == "/debug-error" || path == "/debug-error.html")
    {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    Ok(next.run(request).await)
}