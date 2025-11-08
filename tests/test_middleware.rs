mod common;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, EXPIRES};
use axum::http::{HeaderValue, Request, StatusCode};
use axum::routing::get;
use axum::response::Response;
use axum::Router;
use juicebox::handlers::{add_cache_headers, add_security_headers, ban_gate};
use juicebox::state::{BanSubject, IpBan, TelemetryState};
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt;

fn with_conn_ip(mut req: Request<Body>, ip: [u8; 4], port: u16) -> Request<Body> {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((ip, port))));
    req
}

#[tokio::test]
async fn test_cache_headers_for_static_assets_and_not_for_api() {
    // Router that returns content for static and non-static paths
    let app = Router::new()
        .route("/css/app.css", get(|| async { "body { color: black; }" }))
        .route("/js/app.js", get(|| async { "console.log('ok');" }))
        .route("/api/ping", get(|| async { "pong" }))
        .layer(axum::middleware::from_fn(add_cache_headers));

    // CSS should receive cache headers
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/css/app.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let h = resp.headers();
    assert!(h.get(CACHE_CONTROL).is_some(), "missing cache-control");
    assert!(h.get(EXPIRES).is_some(), "missing expires");

    // JS should receive cache headers
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/js/app.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let h = resp.headers();
    assert!(h.get(CACHE_CONTROL).is_some(), "missing cache-control");
    assert!(h.get(EXPIRES).is_some(), "missing expires");

    // Non-static API path should not get cache headers from middleware
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/ping")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let h = resp.headers();
    assert!(h.get(CACHE_CONTROL).is_none());
    assert!(h.get(EXPIRES).is_none());
}

#[tokio::test]
async fn test_security_headers_injected_and_csp_includes_connect_origin() {
    // Set up state with Sentry DSN so connect-src includes the origin
    let (mut state, _tmp) = common::setup_test_app();
    state.telemetry = Arc::new(TelemetryState {
        sentry_dsn: Some("https://abc123@sentry.example.com/42".to_string()),
        release: "test-release".to_string(),
        environment: "test".to_string(),
        traces_sample_rate: 0.0,
        error_sample_rate: 0.0,
        trace_propagation_targets: vec!["^/".to_string()],
    });

    let app = Router::new()
        .route("/plain", get(|| async { "plain" }))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            add_security_headers,
        ));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/plain")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let h = resp.headers();

    // Core headers should be present
    let csp = h
        .get("Content-Security-Policy")
        .and_then(|v| v.to_str().ok());
    assert!(csp.is_some(), "missing CSP header");
    let csp = csp.unwrap();
    assert!(
        csp.contains("default-src 'self'"),
        "CSP missing default-src: {csp}"
    );
    assert!(
        csp.contains("connect-src 'self'"),
        "CSP missing connect-src self: {csp}"
    );
    // DSN origin should be present in connect-src
    assert!(
        csp.contains("https://sentry.example.com"),
        "CSP connect-src missing sentry origin: {csp}"
    );

    // Other security headers normalized/injected
    assert!(h.get("Permissions-Policy").is_some());
    assert!(h.get("Strict-Transport-Security").is_some());
    assert!(h.get("Referrer-Policy").is_some());
    assert!(h.get("X-Content-Type-Options").is_some());
    assert!(h.get("X-Frame-Options").is_some());
}

#[tokio::test]
async fn test_security_headers_respect_existing_and_normalize_charset() {
    let (state, _tmp) = common::setup_test_app();
    let app = Router::new()
        .route(
            "/html",
            get(|| async {
                let mut resp = Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from("<h1>ok</h1>"))
                    .unwrap();
                let headers = resp.headers_mut();
                headers.insert(
                    "Content-Security-Policy",
                    HeaderValue::from_static("default-src 'none'"),
                );
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/html"));
                resp
            }),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            add_security_headers,
        ));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/html")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let headers = resp.headers();
    // Existing CSP should be preserved verbatim.
    assert_eq!(
        headers
            .get("Content-Security-Policy")
            .and_then(|v| v.to_str().ok()),
        Some("default-src 'none'")
    );
    // Content-Type should gain UTF-8 charset when missing.
    assert_eq!(
        headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    // Other baseline headers should still be applied.
    assert!(headers.get("Permissions-Policy").is_some());
    assert!(headers.get("Strict-Transport-Security").is_some());
    assert!(headers.get("X-Content-Type-Options").is_some());
}

#[tokio::test]
async fn test_ban_gate_blocks_banned_ip_and_bypasses_static_assets() {
    let (state, _tmp) = common::setup_test_app();

    // Ban exact hashed IP: 198.51.100.9
    let ip_str = "198.51.100.9";
    let hash = state
        .hash_ip_to_string(ip_str)
        .expect("hash of fixture ip available");
    state
        .add_ban(IpBan {
            subject: BanSubject::Exact { hash },
            label: Some("test-ban".to_string()),
            reason: "testing".to_string(),
            time: 0,
        })
        .await;

    let app = Router::new()
        .route("/private/hello", get(|| async { "secret" }))
        .route("/css/site.css", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            ban_gate,
        ));

    // Blocked on non-static route
    let req = with_conn_ip(
        Request::builder()
            .uri("/private/hello")
            .body(Body::empty())
            .unwrap(),
        [198, 51, 100, 9],
        5000,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let headers = resp.headers();
    let ct = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok());
    assert_eq!(ct, Some("text/html"));
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("Banned"),
        "ban gate fallback/template not rendered: {text}"
    );

    // Bypass for static assets path even if banned
    let req = with_conn_ip(
        Request::builder()
            .uri("/css/site.css")
            .body(Body::empty())
            .unwrap(),
        [198, 51, 100, 9],
        5001,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], b"ok");

    // Non-banned IP can access private route
    let req = with_conn_ip(
        Request::builder()
            .uri("/private/hello")
            .body(Body::empty())
            .unwrap(),
        [203, 0, 113, 7],
        5002,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], b"secret");
}

#[tokio::test]
async fn test_ban_gate_falls_back_when_template_missing() {
    let (mut state, _tmp) = common::setup_test_app();
    state.tera = Arc::new(tera::Tera::default());

    let ip = "203.0.113.99";
    let hash = state
        .hash_ip_to_string(ip)
        .expect("fixture ip hash");
    state
        .add_ban(IpBan {
            subject: BanSubject::Exact { hash },
            label: None,
            reason: "<b>bad".to_string(),
            time: 0,
        })
        .await;

    let app = Router::new()
        .route("/secure", get(|| async { "secured" }))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            ban_gate,
        ));

    let resp = app
        .oneshot(with_conn_ip(
            Request::builder()
                .uri("/secure")
                .body(Body::empty())
                .unwrap(),
            [203, 0, 113, 99],
            8080,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();

    assert!(body.contains("Banned"));
    assert!(
        !body.contains("<b>bad"),
        "fallback body should escape reason output"
    );
    let label = state
        .find_ban_for_input(ip)
        .await
        .expect("ban present")
        .subject
        .key()
        .to_string();
    let expected_label = if label.len() <= 12 {
        label.clone()
    } else {
        format!("{}…", &label[..12])
    };
    assert!(
        body.contains(&expected_label),
        "fallback body should include short hash label"
    );
}
