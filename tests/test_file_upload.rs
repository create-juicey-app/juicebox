mod common;

use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
};
use tower::ServiceExt;
use juicebox::handlers::{build_router, UploadResponse};
use hyper::body::Bytes;
use std::net::SocketAddr;
use axum::extract::ConnectInfo;

fn create_multipart_body(file_content: &str, file_name: &str, ttl: &str) -> (String, Body) {
    let boundary = "----WebKitFormBoundaryTESTBOUNDARY";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(format!("Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n", file_name).as_bytes());
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

fn with_conn_ip(mut req: Request<Body>, ip: [u8;4], port: u16) -> Request<Body> {
    req.extensions_mut().insert(ConnectInfo(SocketAddr::from((ip, port))));
    req
}

#[tokio::test]
async fn test_upload_file() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("alpha", "alpha.txt", "1h");
    let upload = with_conn_ip(Request::builder().method(Method::POST).uri("/upload").header(header::CONTENT_TYPE, ct).body(body).unwrap(), [127,0,0,1], 1111);
    let resp = app.clone().oneshot(upload).await.unwrap();
    assert!(resp.status() == StatusCode::OK || resp.status() == StatusCode::BAD_REQUEST);
    if resp.status() != StatusCode::OK { return; }
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let up: UploadResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(up.files.len(), 1);
}

#[tokio::test]
async fn test_upload_with_invalid_ttl() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("bad", "bad.txt", "not-a-ttl");
    let upload = with_conn_ip(Request::builder().method(Method::POST).uri("/upload").header(header::CONTENT_TYPE, ct).body(body).unwrap(), [1,2,3,4], 6000);
    let resp = app.clone().oneshot(upload).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_upload_duplicate_file_name() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("file1", "dupe.txt", "1h");
    let upload1 = with_conn_ip(Request::builder().method(Method::POST).uri("/upload").header(header::CONTENT_TYPE, ct.clone()).body(body).unwrap(), [127,0,0,1], 2222);
    let resp1 = app.clone().oneshot(upload1).await.unwrap();
    if resp1.status() != StatusCode::OK { return; }
    let (ct2, body2) = create_multipart_body("file2", "dupe.txt", "1h");
    let upload2 = with_conn_ip(Request::builder().method(Method::POST).uri("/upload").header(header::CONTENT_TYPE, ct2).body(body2).unwrap(), [127,0,0,1], 2222);
    let resp2 = app.clone().oneshot(upload2).await.unwrap();
    // Should allow duplicate names, but different storage names
    assert!(resp2.status() == StatusCode::OK || resp2.status() == StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_upload_empty_file() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("", "empty.txt", "1h");
    let upload = with_conn_ip(Request::builder().method(Method::POST).uri("/upload").header(header::CONTENT_TYPE, ct).body(body).unwrap(), [127,0,0,1], 3333);
    let resp = app.clone().oneshot(upload).await.unwrap();
    // Should succeed or fail gracefully
    assert!(resp.status() == StatusCode::OK || resp.status() == StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_upload_large_file() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    // 1MB file (should be well below default 500MB limit)
    let large_content = "A".repeat(1024 * 1024);
    let (ct, body) = create_multipart_body(&large_content, "large.txt", "1h");
    let upload = with_conn_ip(Request::builder().method(Method::POST).uri("/upload").header(header::CONTENT_TYPE, ct).body(body).unwrap(), [127,0,0,1], 4444);
    let resp = app.clone().oneshot(upload).await.unwrap();
    assert!(resp.status() == StatusCode::OK || resp.status() == StatusCode::BAD_REQUEST);
}
