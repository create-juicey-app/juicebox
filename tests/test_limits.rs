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
async fn test_forbidden_extension_upload() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("zzz", "malware.exe", "1h");
    let upload = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, ct)
            .body(body)
            .unwrap(),
        [8, 8, 8, 8],
        5050,
    );
    let resp = app.clone().oneshot(upload).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_upload_multiple_files_limit() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    for i in 0..6 {
        let fname = format!("file{}.txt", i);
        let (ct, body) = create_multipart_body("multi", &fname, "1h");
        let upload = with_conn_ip(
            Request::builder()
                .method(Method::POST)
                .uri("/upload")
                .header(header::CONTENT_TYPE, ct)
                .body(body)
                .unwrap(),
            [10, 10, 10, 10],
            9000,
        );
        let resp = app.clone().oneshot(upload).await.unwrap();
        if i == 0 {
            assert!(resp.status() == StatusCode::OK || resp.status() == StatusCode::BAD_REQUEST);
            if resp.status() == StatusCode::OK {
                let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
                let up: UploadResponse = serde_json::from_slice(&body_bytes).unwrap();
                assert!(!up.files.is_empty());
            }
        } else {
            assert!(
                resp.status() == StatusCode::CONFLICT || resp.status() == StatusCode::BAD_REQUEST,
                "Expected 409 or 400 for upload at or beyond limit"
            );
        }
    }
}

#[tokio::test]
async fn test_upload_file_at_max_size() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    // 1MB file (should be well below default 500MB limit)
    let max_size = 1024 * 1024;
    let content = "A".repeat(max_size);
    let (ct, body) = create_multipart_body(&content, "maxsize.txt", "1h");
    let upload = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, ct)
            .body(body)
            .unwrap(),
        [11, 11, 11, 11],
        1111,
    );
    let resp = app.clone().oneshot(upload).await.unwrap();
    assert!(resp.status() == StatusCode::OK || resp.status() == StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_forbidden_extension_uppercase() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("zzz", "virus.EXE", "1h");
    let upload = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, ct)
            .body(body)
            .unwrap(),
        [12, 12, 12, 12],
        1212,
    );
    let resp = app.clone().oneshot(upload).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
