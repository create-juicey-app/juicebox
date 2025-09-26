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

fn create_multipart_body(file_content: &'static str, file_name: &'static str, ttl: &'static str) -> (String, Body) {
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
async fn test_list_shows_uploaded_file() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("alpha", "alpha.txt", "1h");
    let upload = with_conn_ip(Request::builder().method(Method::POST).uri("/upload").header(header::CONTENT_TYPE, ct).body(body).unwrap(), [127,0,0,1], 1111);
    let resp = app.clone().oneshot(upload).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let up: UploadResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(up.files.len(), 1);
    // list
    let list_req = with_conn_ip(Request::builder().uri("/list").body(Body::empty()).unwrap(), [127,0,0,1], 1111);
    let list_resp = app.clone().oneshot(list_req).await.unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list_body = to_bytes(list_resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(list_body.to_vec()).unwrap();
    assert!(text.contains(&up.files[0]));
}

#[tokio::test]
async fn test_delete_wrong_owner_not_found() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("beta", "beta.txt", "1h");
    let upload = with_conn_ip(Request::builder().method(Method::POST).uri("/upload").header(header::CONTENT_TYPE, ct).body(body).unwrap(), [10,0,0,1], 2000);
    let resp = app.clone().oneshot(upload).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let up: UploadResponse = serde_json::from_slice(&body_bytes).unwrap();
    let fname = &up.files[0];
    // attempt delete from different IP
    let del_other = with_conn_ip(Request::builder().method(Method::DELETE).uri(format!("/f/{}", fname)).body(Body::empty()).unwrap(), [10,0,0,2], 2001);
    let resp_other = app.clone().oneshot(del_other).await.unwrap();
    assert_eq!(resp_other.status(), StatusCode::NOT_FOUND);
    // correct owner deletes
    let del_owner = with_conn_ip(Request::builder().method(Method::DELETE).uri(format!("/f/{}", fname)).body(Body::empty()).unwrap(), [10,0,0,1], 2000);
    let resp_owner = app.clone().oneshot(del_owner).await.unwrap();
    assert_eq!(resp_owner.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn test_report_handler() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("gamma", "gamma.txt", "1h");
    let upload = with_conn_ip(Request::builder().method(Method::POST).uri("/upload").header(header::CONTENT_TYPE, ct).body(body).unwrap(), [192,168,0,1], 3000);
    let resp = app.clone().oneshot(upload).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let up: UploadResponse = serde_json::from_slice(&body_bytes).unwrap();
    let fname = &up.files[0];
    let form = format!("file={}&reason=test&details=hello", fname);
    let report_req = with_conn_ip(Request::builder().method(Method::POST).uri("/report").header(header::CONTENT_TYPE, "application/x-www-form-urlencoded").body(Body::from(form)).unwrap(), [192,168,0,1], 3001);
    let report_resp = app.clone().oneshot(report_req).await.unwrap();
    assert_eq!(report_resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn test_simple_list_and_delete_flow() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("delta", "delta.txt", "1h");
    let upload = with_conn_ip(Request::builder().method(Method::POST).uri("/upload").header(header::CONTENT_TYPE, ct).body(body).unwrap(), [127,0,0,1], 4000);
    let resp = app.clone().oneshot(upload).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let up: UploadResponse = serde_json::from_slice(&body_bytes).unwrap();
    let fname = &up.files[0];
    // simple list
    let list_req = with_conn_ip(Request::builder().uri("/simple").body(Body::empty()).unwrap(), [127,0,0,1], 4000);
    let list_resp = app.clone().oneshot(list_req).await.unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list_html = to_bytes(list_resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(list_html.to_vec()).unwrap();
    assert!(html.contains(fname));
    let form = format!("f={}", fname);
    let del_req = with_conn_ip(Request::builder().method(Method::POST).uri("/simple/delete").header(header::CONTENT_TYPE, "application/x-www-form-urlencoded").body(Body::from(form)).unwrap(), [127,0,0,1], 4000);
    let del_resp = app.clone().oneshot(del_req).await.unwrap();
    assert_eq!(del_resp.status(), StatusCode::SEE_OTHER);
    let loc = del_resp.headers().get(header::LOCATION).unwrap().to_str().unwrap();
    assert!(loc.contains("Deleted"));
    // verify file not accessible
    let fetch_req = with_conn_ip(Request::builder().uri(format!("/f/{}", fname)).body(Body::empty()).unwrap(), [127,0,0,1], 4000);
    let fetch_resp = app.clone().oneshot(fetch_req).await.unwrap();
    assert_eq!(fetch_resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_forbidden_extension_upload() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("zzz", "malware.exe", "1h");
    let upload = with_conn_ip(Request::builder().method(Method::POST).uri("/upload").header(header::CONTENT_TYPE, ct).body(body).unwrap(), [8,8,8,8], 5050);
    let resp = app.clone().oneshot(upload).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let up: UploadResponse = serde_json::from_slice(&body_bytes).unwrap();
    assert!(up.files.is_empty());
    assert!(up.truncated);
    assert_eq!(up.remaining, 1);
}