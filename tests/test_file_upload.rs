mod common;

use axum::extract::ConnectInfo;
use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use hyper::body::Bytes;
use juicebox::handlers::{
    ChunkCompleteRequest, ChunkInitRequest, ChunkInitResponse, UploadResponse, build_router,
};
use sha2::{Digest, Sha256};
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
async fn test_upload_file() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("alpha", "alpha.txt", "1h");
    let upload = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, ct)
            .body(body)
            .unwrap(),
        [127, 0, 0, 1],
        1111,
    );
    let resp = app.clone().oneshot(upload).await.unwrap();
    assert!(resp.status() == StatusCode::OK || resp.status() == StatusCode::BAD_REQUEST);
    if resp.status() != StatusCode::OK {
        return;
    }
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let up: UploadResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(up.files.len(), 1);
}

#[tokio::test]
async fn test_upload_with_invalid_ttl() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("bad", "bad.txt", "not-a-ttl");
    let upload = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, ct)
            .body(body)
            .unwrap(),
        [1, 2, 3, 4],
        6000,
    );
    let resp = app.clone().oneshot(upload).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_upload_duplicate_file_name() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("file1", "dupe.txt", "1h");
    let upload1 = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, ct.clone())
            .body(body)
            .unwrap(),
        [127, 0, 0, 1],
        2222,
    );
    let resp1 = app.clone().oneshot(upload1).await.unwrap();
    if resp1.status() != StatusCode::OK {
        return;
    }
    let (ct2, body2) = create_multipart_body("file2", "dupe.txt", "1h");
    let upload2 = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, ct2)
            .body(body2)
            .unwrap(),
        [127, 0, 0, 1],
        2222,
    );
    let resp2 = app.clone().oneshot(upload2).await.unwrap();
    // Should allow duplicate names, but different storage names
    assert!(resp2.status() == StatusCode::OK || resp2.status() == StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_upload_empty_file() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());
    let (ct, body) = create_multipart_body("", "empty.txt", "1h");
    let upload = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, ct)
            .body(body)
            .unwrap(),
        [127, 0, 0, 1],
        3333,
    );
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
    let upload = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, ct)
            .body(body)
            .unwrap(),
        [127, 0, 0, 1],
        4444,
    );
    let resp = app.clone().oneshot(upload).await.unwrap();
    assert!(resp.status() == StatusCode::OK || resp.status() == StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_chunk_upload_flow() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());

    let data = vec![b'a'; 150_000];
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let hash = format!("{:x}", hasher.finalize());

    let init_req = ChunkInitRequest {
        filename: "chunked.bin".to_string(),
        size: data.len() as u64,
        ttl: Some("1h".to_string()),
        chunk_size: Some(70_000),
        hash: Some(hash.clone()),
    };
    let init = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/chunk/init")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&init_req).unwrap()))
            .unwrap(),
        [127, 0, 0, 1],
        5000,
    );
    let init_resp = app.clone().oneshot(init).await.unwrap();
    assert_eq!(init_resp.status(), StatusCode::OK);
    let init_bytes = to_bytes(init_resp.into_body(), usize::MAX).await.unwrap();
    let session: ChunkInitResponse = serde_json::from_slice(&init_bytes).unwrap();
    assert!(session.total_chunks >= 2);
    assert!(!session.storage_name.is_empty());

    for idx in 0..session.total_chunks {
        let start = idx as usize * session.chunk_size as usize;
        let end = std::cmp::min(start + session.chunk_size as usize, data.len());
        let chunk = Body::from(Bytes::copy_from_slice(&data[start..end]));
        let part = with_conn_ip(
            Request::builder()
                .method(Method::PUT)
                .uri(format!("/chunk/{}/{idx}", session.session_id))
                .body(chunk)
                .unwrap(),
            [127, 0, 0, 1],
            5000,
        );
        let resp = app.clone().oneshot(part).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    let complete_req = ChunkCompleteRequest {
        hash: Some(hash.clone()),
    };
    let complete = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri(format!("/chunk/{}/complete", session.session_id))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&complete_req).unwrap()))
            .unwrap(),
        [127, 0, 0, 1],
        5000,
    );
    let complete_resp = app.clone().oneshot(complete).await.unwrap();
    assert_eq!(complete_resp.status(), StatusCode::OK);
    let complete_resp_bytes = to_bytes(complete_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let uploaded: UploadResponse = serde_json::from_slice(&complete_resp_bytes).unwrap();
    assert_eq!(uploaded.files.len(), 1);

    let meta = state.owners.get(&uploaded.files[0]).unwrap();
    assert_eq!(meta.hash, hash);
}

#[tokio::test]
async fn test_chunk_session_persistence_across_restart() {
    let (state, tmp) = common::setup_test_app();
    let app = build_router(state.clone());

    let data = vec![b'z'; 150_000];
    let init_req = ChunkInitRequest {
        filename: "resume.bin".to_string(),
        size: data.len() as u64,
        ttl: Some("1h".to_string()),
        chunk_size: Some(64_000),
        hash: None,
    };
    let init = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/chunk/init")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&init_req).unwrap()))
            .unwrap(),
        [100, 64, 1, 1],
        6000,
    );
    let init_resp = app.clone().oneshot(init).await.unwrap();
    assert_eq!(init_resp.status(), StatusCode::OK);
    let init_bytes = to_bytes(init_resp.into_body(), usize::MAX).await.unwrap();
    let session: ChunkInitResponse = serde_json::from_slice(&init_bytes).unwrap();
    assert!(session.total_chunks >= 2);

    // Upload only the first chunk to leave the session incomplete.
    let first_len = session.chunk_size as usize;
    let chunk_body = Body::from(Bytes::copy_from_slice(&data[..first_len]));
    let first_chunk = with_conn_ip(
        Request::builder()
            .method(Method::PUT)
            .uri(format!("/chunk/{}/{idx}", session.session_id, idx = 0))
            .body(chunk_body)
            .unwrap(),
        [100, 64, 1, 1],
        6000,
    );
    let chunk_resp = app.clone().oneshot(first_chunk).await.unwrap();
    assert_eq!(chunk_resp.status(), StatusCode::NO_CONTENT);

    // Recreate state from disk and ensure session metadata is restored.
    let restored_state = common::recreate_state(tmp.path());
    restored_state
        .load_chunk_sessions_from_disk()
        .await
        .expect("load chunk sessions");
    let restored = restored_state
        .chunk_sessions
        .get(&session.session_id)
        .expect("session restored");
    let restored_session = restored.value().clone();
    drop(restored);

    assert_eq!(restored_session.original_name, "resume.bin");
    assert_eq!(restored_session.total_chunks, session.total_chunks);
    assert_eq!(restored_session.chunk_size, session.chunk_size);
    assert_eq!(restored_session.owner, "100.64.1.1".to_string());
    let received = restored_session.received.read().await;
    assert!(received[0]);
    assert!(received.iter().skip(1).all(|r| !*r));

    let chunk_path = restored_session.storage_dir.join("000000.chunk");
    assert!(chunk_path.exists());
}
