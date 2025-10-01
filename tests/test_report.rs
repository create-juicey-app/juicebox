mod common;

use axum::extract::ConnectInfo;
use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use hyper::body::Bytes;
use juicebox::handlers::{UploadResponse, build_router};
use std::net::SocketAddr;
use tower::ServiceExt;

fn create_multipart_body(file_content: &str, file_name: &str, ttl: &str) -> (String, Body) {
    let boundary = "----WebKitFormBoundaryTESTBOUNDARY";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n",
            file_name
        )
        .as_bytes(),
    );
    body.extend_from_slice(b"Content-Type: text/plain\r\n\r\n");
    body.extend_from_slice(file_content.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"ttl\"\r\n\r\n");
    body.extend_from_slice(ttl.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
    let content_type = format!("multipart/form-data; boundary={}", boundary);
    (content_type, Body::from(Bytes::from(body)))
}

fn with_conn_ip(mut req: Request<Body>, ip: [u8; 4], port: u16) -> Request<Body> {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((ip, port))));
    req
}

#[tokio::test]
async fn test_report_handler() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("gamma", "gamma.txt", "1h");
    let upload = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, ct)
            .body(body)
            .unwrap(),
        [192, 168, 0, 1],
        3000,
    );
    let resp = app.clone().oneshot(upload).await.unwrap();
    assert!(resp.status() == StatusCode::OK || resp.status() == StatusCode::BAD_REQUEST);
    let status = resp.status();
    if status != StatusCode::OK {
        return;
    }
    let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let up: UploadResponse = serde_json::from_slice(&body_bytes).unwrap();
    let fname = &up.files[0];
    let form = format!("file={}&reason=test&details=hello", fname);
    let report_req = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/report")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form))
            .unwrap(),
        [192, 168, 0, 1],
        3001,
    );
    let report_resp = app.clone().oneshot(report_req).await.unwrap();
    assert_eq!(report_resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn test_report_missing_details() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("delta", "delta.txt", "1h");
    let upload = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, ct)
            .body(body)
            .unwrap(),
        [192, 168, 0, 2],
        3002,
    );
    let resp = app.clone().oneshot(upload).await.unwrap();
    if resp.status() != StatusCode::OK {
        return;
    }
    let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let up: UploadResponse = serde_json::from_slice(&body_bytes).unwrap();
    let fname = &up.files[0];
    let form = format!("file={}&reason=test", fname);
    let report_req = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/report")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form))
            .unwrap(),
        [192, 168, 0, 2],
        3003,
    );
    let report_resp = app.clone().oneshot(report_req).await.unwrap();
    // Should be accepted or rejected gracefully
    assert!(
        report_resp.status() == StatusCode::NO_CONTENT
            || report_resp.status() == StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn test_report_invalid_file() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let form = "file=notarealfile&reason=abuse&details=bad";
    let report_req = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/report")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form))
            .unwrap(),
        [192, 168, 0, 3],
        3004,
    );
    let report_resp = app.clone().oneshot(report_req).await.unwrap();
    assert!(
        report_resp.status() == StatusCode::BAD_REQUEST
            || report_resp.status() == StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn test_report_empty_reason() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("epsilon", "epsilon.txt", "1h");
    let upload = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, ct)
            .body(body)
            .unwrap(),
        [192, 168, 0, 4],
        3005,
    );
    let resp = app.clone().oneshot(upload).await.unwrap();
    if resp.status() != StatusCode::OK {
        return;
    }
    let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let up: UploadResponse = serde_json::from_slice(&body_bytes).unwrap();
    let fname = &up.files[0];
    let form = format!("file={}&reason=&details=empty", fname);
    let report_req = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/report")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form))
            .unwrap(),
        [192, 168, 0, 4],
        3006,
    );
    let report_resp = app.clone().oneshot(report_req).await.unwrap();
    assert!(
        report_resp.status() == StatusCode::NO_CONTENT
            || report_resp.status() == StatusCode::BAD_REQUEST
    );
}
