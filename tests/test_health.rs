mod common;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use juicebox::handlers::build_router;
use tower::ServiceExt;

#[tokio::test]
async fn test_health_check() {
    let (state, _temp_dir) = common::setup_test_app();
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], b"ok");
}

#[tokio::test]
async fn test_health_check_wrong_method() {
    let (state, _temp_dir) = common::setup_test_app();
    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Should be 405 Method Not Allowed or 200 if allowed
    assert!(
        response.status() == StatusCode::METHOD_NOT_ALLOWED || response.status() == StatusCode::OK
    );
}

#[tokio::test]
async fn test_health_check_with_headers() {
    let (state, _temp_dir) = common::setup_test_app();
    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .header("X-Test", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], b"ok");
}
