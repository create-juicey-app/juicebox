use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, EXPIRES, LOCATION};
use axum::http::{HeaderValue, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr as ClientAddr;
use std::time::{Duration, SystemTime};
use tokio::fs;

use crate::state::{AppState, IpBan};
use crate::util::{extract_client_ip, PROD_HOST};

pub async fn add_security_headers(req: Request<Body>, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    if !h.contains_key("Content-Security-Policy") {
        h.insert(
            "Content-Security-Policy",
            HeaderValue::from_static(
                "default-src 'self'; script-src 'self' 'unsafe-inline' https://static.cloudflareinsights.com; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:",
            ),
        );
    }
    if !h.contains_key("Permissions-Policy") {
        h.insert(
            "Permissions-Policy",
            HeaderValue::from_static(
                "camera=(), microphone=(), geolocation=(), fullscreen=(), payment=()",
            ),
        );
    }
    if !h.contains_key("Strict-Transport-Security") {
        h.insert(
            "Strict-Transport-Security",
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    if !h.contains_key("Referrer-Policy") {
        h.insert("Referrer-Policy", HeaderValue::from_static("same-origin"));
    }
    if !h.contains_key("X-Content-Type-Options") {
        h.insert(
            "X-Content-Type-Options",
            HeaderValue::from_static("nosniff"),
        );
    }
    if !h.contains_key("X-Frame-Options") {
        h.insert("X-Frame-Options", HeaderValue::from_static("DENY"));
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
    let (reason, time, label) = match state.find_ban_for_input(&ip).await {
        Some(ban) => (ban.reason.clone(), ban.time, ban_label(&ban)),
        None => (String::new(), 0, short_hash(&ip)),
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
            .replace("{{IP}}", &label)
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
        safe_reason, label
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

fn ban_label(ban: &IpBan) -> String {
    if let Some(label) = ban.label.as_ref().filter(|l| !l.trim().is_empty()) {
        return label.trim().to_string();
    }
    short_hash(ban.subject.key())
}

fn short_hash(value: &str) -> String {
    if value.len() <= 12 {
        value.to_string()
    } else {
        format!("{}…", &value[..12])
    }
}
