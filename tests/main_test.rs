mod common;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use tower::ServiceExt;
use juicebox::handlers::build_router;

#[tokio::test]
async fn test_health_check() {
    let (state, _temp_dir) = common::setup_test_app();
    let app = build_router(state);

    let response = app
        .oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], b"ok");
}
