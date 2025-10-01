use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, EXPIRES, LOCATION};
use axum::http::{HeaderValue, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr as ClientAddr;
use std::time::{Duration, SystemTime};
use tokio::fs;

use crate::state::AppState;
use crate::util::{PROD_HOST, extract_client_ip};

pub async fn add_security_headers(req: Request<Body>, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    if !h.contains_key("X-Content-Type-Options") {
        h.insert(
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self' 'unsafe-inline' https://static.cloudflareinsights.com; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:".parse().unwrap(),
        );
    }
    if !h.contains_key("Permissions-Policy") {
        h.insert(
            "Permissions-Policy",
            "camera=(), microphone=(), geolocation=(), fullscreen=(), payment=()"
                .parse()
                .unwrap(),
        );
    }
    if let Some(ct_val) = h.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()) {
        let ct_lower = ct_val.to_ascii_lowercase();
        if ct_lower.starts_with("text/html") && !ct_lower.contains("charset=") {
            h.insert(
                CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
        }
    }
    resp
}

pub async fn enforce_host(req: Request<Body>, next: Next) -> Response {
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
        (StatusCode::MOVED_PERMANENTLY, [(LOCATION, hv)]).into_response()
    }
}

pub async fn ban_gate(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    if path.starts_with("/css/") || path.starts_with("/js/") {
        return next.run(req).await;
    }
    let ip = extract_client_ip(req.headers(), Some(addr.ip()));
    if !state.is_banned(&ip).await {
        return next.run(req).await;
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
            [(CONTENT_TYPE, HeaderValue::from_static("text/html"))],
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
        [(CONTENT_TYPE, HeaderValue::from_static("text/html"))],
        fallback,
    )
        .into_response()
}

pub async fn add_cache_headers(req: Request<Body>, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let mut resp = next.run(req).await;
    if (path.starts_with("/css/") || path.starts_with("/js/")) && !path.contains("../") {
        let headers = resp.headers_mut();
        let max_age = 86400;
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
