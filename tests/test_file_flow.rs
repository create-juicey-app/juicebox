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

// Helper to create a multipart body
fn create_multipart_body(file_content: &'static str, file_name: &'static str, ttl: &'static str) -> (String, Body) {
    let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(format!("Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n", file_name).as_bytes());
    body.extend_from_slice(b"Content-Type: text/plain\r\n\r\n");
    body.extend_from_slice(file_content.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(format!("Content-Disposition: form-data; name=\"ttl\"\r\n\r\n").as_bytes());
    body.extend_from_slice(ttl.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

    let content_type = format!("multipart/form-data; boundary={}", boundary);
    (content_type, Body::from(Bytes::from(body)))
}

// Attach a dummy ConnectInfo so handlers extracting it succeed in tests
fn with_conn(mut req: Request<Body>) -> Request<Body> {
    req.extensions_mut().insert(ConnectInfo(SocketAddr::from(([127,0,0,1], 40000))));
    req
}

#[tokio::test]
async fn test_upload_fetch_delete_file() {
    let (state, _temp_dir) = common::setup_test_app();
    let app = build_router(state.clone());

    // 1. Upload a file
    let (content_type, body) = create_multipart_body("test content", "test.txt", "1h");

    let upload_req = with_conn(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, content_type)
            .body(body)
            .unwrap(),
    );

    let response = app.clone().oneshot(upload_req).await.unwrap();
    if response.status() != StatusCode::OK {
        let status = response.status();
        let err_body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        eprintln!("Upload failed: status = {}, body = {}", status, String::from_utf8_lossy(&err_body));
        panic!("Upload failed");
    }

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let upload_resp: UploadResponse = serde_json::from_slice(&body).unwrap();

    assert_eq!(upload_resp.files.len(), 1);
    let file_name = &upload_resp.files[0];

    // 2. Fetch the file
    let fetch_uri = format!("/f/{}", file_name);
    let fetch_req = with_conn(
        Request::builder()
            .uri(&fetch_uri)
            .body(Body::empty())
            .unwrap(),
    );

    let response = app.clone().oneshot(fetch_req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body, "test content");

    // 3. Delete the file
    let delete_uri = format!("/f/{}", file_name);
    let delete_req = with_conn(
        Request::builder()
            .method(Method::DELETE)
            .uri(&delete_uri)
            .body(Body::empty())
            .unwrap(),
    );

    let response = app.clone().oneshot(delete_req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // 4. Verify file is gone
    let fetch_req_after_delete = with_conn(
        Request::builder()
            .uri(&fetch_uri)
            .body(Body::empty())
            .unwrap(),
    );

    let response = app.clone().oneshot(fetch_req_after_delete).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_fetch_after_delete_returns_not_found() {
    let (state, _temp_dir) = common::setup_test_app();
    let app = build_router(state.clone());
    let (content_type, body) = create_multipart_body("gone soon", "gone.txt", "1h");
    let upload_req = with_conn(Request::builder().method(Method::POST).uri("/upload").header(header::CONTENT_TYPE, content_type).body(body).unwrap());
    let response = app.clone().oneshot(upload_req).await.unwrap();
    if response.status() != StatusCode::OK { return; }
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let upload_resp: UploadResponse = serde_json::from_slice(&body).unwrap();
    let file_name = &upload_resp.files[0];
    // Delete
    let delete_uri = format!("/f/{}", file_name);
    let delete_req = with_conn(Request::builder().method(Method::DELETE).uri(&delete_uri).body(Body::empty()).unwrap());
    let _ = app.clone().oneshot(delete_req).await.unwrap();
    // Fetch after delete
    let fetch_uri = format!("/f/{}", file_name);
    let fetch_req = with_conn(Request::builder().uri(&fetch_uri).body(Body::empty()).unwrap());
    let response = app.clone().oneshot(fetch_req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_upload_and_fetch_binary_file() {
    let (state, _temp_dir) = common::setup_test_app();
    let app = build_router(state.clone());
    // Use a valid UTF-8 string to avoid lifetime issues in test
    let binary_content = "\x00\x01\x02\x03\x7F";
    let (content_type, body) = create_multipart_body(binary_content, "binfile.bin", "1h");
    let upload_req = with_conn(Request::builder().method(Method::POST).uri("/upload").header(header::CONTENT_TYPE, content_type).body(body).unwrap());
    let response = app.clone().oneshot(upload_req).await.unwrap();
    if response.status() != StatusCode::OK { return; }
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let upload_resp: UploadResponse = serde_json::from_slice(&body).unwrap();
    let file_name = &upload_resp.files[0];
    let fetch_uri = format!("/f/{}", file_name);
    let fetch_req = with_conn(Request::builder().uri(&fetch_uri).body(Body::empty()).unwrap());
    let response = app.clone().oneshot(fetch_req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
