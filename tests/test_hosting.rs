mod common;

use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Request, StatusCode, header};
use juicebox::handlers::build_router;
use juicebox::state::FileMeta;
use juicebox::util::{max_file_bytes, now_secs};
use once_cell::sync::Lazy;
use serde_json::Value;
use std::sync::Mutex;
use tower::ServiceExt;

static ENV_GUARD: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

#[tokio::test]
async fn test_config_handler_includes_telemetry_and_streaming_flag() {
    let _lock = ENV_GUARD.lock().unwrap();

    // Default: no ENABLE_STREAMING_UPLOADS
    // not modifying env in tests
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    // Required fields
    assert!(json.get("max_file_bytes").is_some());
    assert!(json.get("max_file_size_str").is_some());
    assert!(
        json.get("enable_streaming_uploads")
            .and_then(|v| v.as_bool())
            .is_some()
    );

    // Telemetry present and mirrors test TelemetryState from common::setup_test_app
    let tele = json.get("telemetry").and_then(|v| v.get("sentry")).unwrap();
    assert_eq!(tele.get("enabled").and_then(|v| v.as_bool()), Some(false));
    assert!(tele.get("dsn").map(|v| v.is_null()).unwrap_or(true));
    assert_eq!(
        tele.get("release").and_then(|v| v.as_str()),
        Some("test-release")
    );
    assert_eq!(
        tele.get("environment").and_then(|v| v.as_str()),
        Some("test")
    );
    assert_eq!(
        tele.get("traces_sample_rate").and_then(|v| v.as_f64()),
        Some(0.0)
    );
    assert_eq!(
        tele.get("profiles_sample_rate").and_then(|v| v.as_f64()),
        Some(0.0)
    );
    let targets = tele
        .get("trace_propagation_targets")
        .and_then(|v| v.as_array())
        .unwrap();
    assert!(targets.iter().any(|t| t.as_str() == Some("^/")));

    assert!(json.get("quota").is_none());

    // Skipping env-toggling assertions; ensure the field exists and is boolean (checked above).
}

#[tokio::test]
async fn test_config_handler_returns_quota_payload_when_enabled() {
    let _lock = ENV_GUARD.lock().unwrap();

    let (mut state, _tmp) = common::setup_test_app();
    state.max_storage_quota = Some(1_048_576); // 1 MiB
    let app = build_router(state.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    let quota = json
        .get("quota")
        .and_then(|q| q.as_object())
        .expect("quota payload missing");

    assert_eq!(
        quota.get("max_bytes").and_then(Value::as_u64),
        Some(1_048_576)
    );
    assert_eq!(
        quota.get("uploads_blocked").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        quota.get("remaining_bytes").and_then(Value::as_u64),
        Some(1_048_576)
    );
    assert!(quota.get("message").is_none());
}

#[tokio::test]
async fn test_quota_endpoint_reports_blocked_status() {
    let _lock = ENV_GUARD.lock().unwrap();

    let (mut state, _tmp) = common::setup_test_app();
    state.max_storage_quota = Some(512);
    let now = now_secs();
    let owner = common::hash_fixture_ip("203.0.113.42");
    state.owners.insert(
        "blocked.bin".to_string(),
        FileMeta {
            owner_hash: owner,
            expires: now + 3_600,
            original: "blocked.bin".to_string(),
            created: now,
            hash: String::new(),
            size: 512,
        },
    );

    let app = build_router(state.clone());

    // Config endpoint should reflect blocked quota state
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    let quota = json
        .get("quota")
        .and_then(|q| q.as_object())
        .expect("quota payload missing from config");
    assert_eq!(
        quota.get("uploads_blocked").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        quota.get("remaining_bytes").and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        quota.get("message").and_then(Value::as_str),
        Some("Maximum storage quota has been reached. You cannot upload for now.")
    );

    // Quota endpoint should match the same data shape
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/quota")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    let quota = json
        .get("quota")
        .and_then(|q| q.as_object())
        .expect("quota payload missing from quota endpoint");
    assert_eq!(
        quota.get("uploads_blocked").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        quota.get("remaining_bytes").and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        quota.get("message").and_then(Value::as_str),
        Some("Maximum storage quota has been reached. You cannot upload for now.")
    );
}

#[tokio::test]
async fn test_quota_guard_triggers_when_only_max_file_space_remains() {
    let _lock = ENV_GUARD.lock().unwrap();

    let guard = max_file_bytes();
    let limit = guard.saturating_mul(5).max(guard.saturating_add(1));

    let (mut state, _tmp) = common::setup_test_app();
    state.max_storage_quota = Some(limit);

    let now = now_secs();
    let owner = common::hash_fixture_ip("198.51.100.77");
    state.owners.insert(
        "guard.bin".to_string(),
        FileMeta {
            owner_hash: owner,
            expires: now + 3_600,
            original: "guard.bin".to_string(),
            created: now,
            hash: String::new(),
            size: limit.saturating_sub(guard),
        },
    );

    let app = build_router(state.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/quota")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    let quota = json
        .get("quota")
        .and_then(|q| q.as_object())
        .expect("quota payload missing");

    assert_eq!(
        quota.get("uploads_blocked").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        quota.get("remaining_bytes").and_then(Value::as_u64),
        Some(guard)
    );
    assert_eq!(
        quota.get("message").and_then(Value::as_str),
        Some("Maximum storage quota has been reached. You cannot upload for now.")
    );
}

#[tokio::test]
async fn test_fetch_file_serves_and_sets_cache_headers() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());

    // Prepare a file on disk and metadata
    let file_name = "hello.txt".to_string();
    let file_path = state.upload_dir.join(&file_name);
    std::fs::write(&file_path, b"hi there").unwrap();

    let owner = common::hash_fixture_ip("127.0.0.1");
    let exp = now_secs() + 120; // 2 minutes -> short cache
    state.owners.insert(
        file_name.clone(),
        FileMeta {
            owner_hash: owner,
            expires: exp,
            original: "hello.txt".to_string(),
            created: now_secs(),
            hash: String::new(),
            size: b"hi there".len() as u64,
        },
    );

    // Fetch the file via router
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/f/{}", file_name))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let headers = resp.headers();
    // Content-Type is based on file extension
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    assert_eq!(ct, Some("text/plain"));

    // Cache headers present with max-age policy
    let cc = headers
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        cc.contains("public") && cc.contains("max-age="),
        "unexpected cache-control: {cc}"
    );
    // Expires header should also be present
    assert!(headers.get(header::EXPIRES).is_some());

    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], b"hi there");
}

#[tokio::test]
async fn test_fetch_file_404_missing_or_orphan() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());

    // Missing file & metadata
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/f/not_found.txt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Orphaned metadata (present in owners but no file on disk)
    let orphan = "ghost.txt".to_string();
    state.owners.insert(
        orphan.clone(),
        FileMeta {
            owner_hash: common::hash_fixture_ip("10.0.0.1"),
            expires: now_secs() + 3600,
            original: orphan.clone(),
            created: now_secs(),
            hash: String::new(),
            size: 0,
        },
    );
    let resp2 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/f/{}", orphan))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_static_file_served_with_cache_and_precompressed_when_requested() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());

    // Create a CSS file and its precompressed .br sibling
    let css_name = "foo.css";
    let css_path = state.static_dir.join(css_name);
    std::fs::write(&css_path, b"body{background:#fff}").unwrap();

    let br_path = state.static_dir.join(format!("{css_name}.br"));
    std::fs::write(&br_path, b"NOTREALBR").unwrap(); // Not real brotli, but server does not validate

    // 1) Without Accept-Encoding -> original file, cache headers present for css
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/{}", css_name))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let h1 = resp.headers();
    assert_eq!(
        h1.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
        Some("text/css")
    );
    assert!(h1.get(header::CACHE_CONTROL).is_some());
    assert!(h1.get(header::EXPIRES).is_some());
    assert!(h1.get(header::CONTENT_ENCODING).is_none());

    // 2) With Accept-Encoding: br -> should serve .br variant and set encoding
    let resp2 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/{}", css_name))
                .header(header::ACCEPT_ENCODING, HeaderValue::from_static("br"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let h2 = resp2.headers();
    assert_eq!(
        h2.get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("br")
    );
    // Vary header should be set when content encoding is used
    assert!(h2.get(header::VARY).is_some());
    let body2 = to_bytes(resp2.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body2[..], b"NOTREALBR");
}

#[tokio::test]
async fn test_file_handler_rejects_traversal_attempt() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());

    // Attempt directory traversal in path
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/../../etc/passwd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], b"bad path");
}
