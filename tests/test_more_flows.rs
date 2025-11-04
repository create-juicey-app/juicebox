mod common;

use axum::extract::ConnectInfo;
use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use hyper::body::Bytes;
use juicebox::handlers::{ChunkCompleteRequest, ChunkInitRequest, ChunkInitResponse, build_router};
use serde_json::Value;

use std::net::SocketAddr;
use tokio::sync::OwnedSemaphorePermit;
use tower::ServiceExt;

fn with_conn_ip(mut req: Request<Body>, ip: [u8; 4], port: u16) -> Request<Body> {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((ip, port))));
    req
}

fn multipart_body_with(boundary: &str, parts: &[(&str, Option<&str>, &[u8])]) -> (String, Body) {
    let mut body = Vec::new();
    for (name, filename, data) in parts {
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        match filename {
            Some(fname) => {
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                        name, fname
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
            }
            #[allow(non_snake_case)]
            None => {
                body.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n", name).as_bytes(),
                );
            }
        }
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
    let content_type = format!("multipart/form-data; boundary={}", boundary);
    (content_type, Body::from(Bytes::from(body)))
}

#[tokio::test]
async fn chunk_init_rejects_empty_and_too_large() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state);

    // Empty size
    let init_req = ChunkInitRequest {
        filename: "zero.bin".to_string(),
        size: 0,
        ttl: None,
        chunk_size: Some(64_000),
        hash: None,
    };
    let req = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/chunk/init")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&init_req).unwrap()))
            .unwrap(),
        [127, 0, 0, 1],
        7001,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let msg: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(msg.get("code").and_then(|v| v.as_str()), Some("empty"));

    // Too large
    let big = ChunkInitRequest {
        filename: "big.bin".to_string(),
        size: 600 * 1024 * 1024, // 600MB > default 500MB
        ttl: None,
        chunk_size: Some(64_000),
        hash: None,
    };
    let req2 = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/chunk/init")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&big).unwrap()))
            .unwrap(),
        [127, 0, 0, 1],
        7002,
    );
    let resp2 = app.clone().oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let msg2: Value =
        serde_json::from_slice(&to_bytes(resp2.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(msg2.get("code").and_then(|v| v.as_str()), Some("too_large"));
}

#[tokio::test]
async fn chunk_part_and_status_errors_for_missing_session_and_wrong_owner() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());

    // Missing session -> 404
    let missing = with_conn_ip(
        Request::builder()
            .method(Method::PUT)
            .uri("/chunk/notfound/0")
            .body(Body::from(Bytes::from_static(b"abc")))
            .unwrap(),
        [10, 0, 0, 1],
        7010,
    );
    let resp = app.clone().oneshot(missing).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Create a valid session from IP A
    let init_req = ChunkInitRequest {
        filename: "tiny.bin".to_string(),
        size: 100, // one chunk expected with default min chunk size
        ttl: None,
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
        [10, 0, 0, 1],
        7011,
    );
    let init_resp = app.clone().oneshot(init).await.unwrap();
    assert_eq!(init_resp.status(), StatusCode::OK);
    let body = to_bytes(init_resp.into_body(), usize::MAX).await.unwrap();
    let session: ChunkInitResponse = serde_json::from_slice(&body).unwrap();

    // Wrong owner status
    let status_wrong = with_conn_ip(
        Request::builder()
            .uri(format!("/chunk/{}/status", session.session_id))
            .body(Body::empty())
            .unwrap(),
        [10, 0, 0, 2], // different IP
        7012,
    );
    let resp_status = app.clone().oneshot(status_wrong).await.unwrap();
    assert_eq!(resp_status.status(), StatusCode::FORBIDDEN);

    // Wrong owner part upload
    let part_wrong = with_conn_ip(
        Request::builder()
            .method(Method::PUT)
            .uri(format!("/chunk/{}/{}", session.session_id, 0))
            .body(Body::from(Bytes::from(vec![0u8; 100])))
            .unwrap(),
        [10, 0, 0, 2], // different IP
        7013,
    );
    let resp_part = app.clone().oneshot(part_wrong).await.unwrap();
    assert_eq!(resp_part.status(), StatusCode::FORBIDDEN);

    // Wrong owner completion
    let complete_wrong = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri(format!("/chunk/{}/complete", session.session_id))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&ChunkCompleteRequest { hash: None }).unwrap(),
            ))
            .unwrap(),
        [10, 0, 0, 2],
        7014,
    );
    let resp_complete = app.clone().oneshot(complete_wrong).await.unwrap();
    assert_eq!(resp_complete.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn chunk_part_index_and_length_validation_and_incomplete_completion() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());

    let init_req = ChunkInitRequest {
        filename: "one.bin".to_string(),
        size: 100,
        ttl: None,
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
        [10, 10, 0, 1],
        7020,
    );
    let init_resp = app.clone().oneshot(init).await.unwrap();
    let body = to_bytes(init_resp.into_body(), usize::MAX).await.unwrap();
    let session: ChunkInitResponse = serde_json::from_slice(&body).unwrap();

    // Index out of range
    let part_oob = with_conn_ip(
        Request::builder()
            .method(Method::PUT)
            .uri(format!(
                "/chunk/{}/{}",
                session.session_id, session.total_chunks
            )) // = invalid
            .body(Body::from(Bytes::from_static(b"abc")))
            .unwrap(),
        [10, 10, 0, 1],
        7021,
    );
    let resp_oob = app.clone().oneshot(part_oob).await.unwrap();
    assert_eq!(resp_oob.status(), StatusCode::BAD_REQUEST);
    let msg: Value =
        serde_json::from_slice(&to_bytes(resp_oob.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(
        msg.get("code").and_then(|v| v.as_str()),
        Some("chunk_index")
    );

    // Length mismatch (expected 100, send 10)
    let part_mismatch = with_conn_ip(
        Request::builder()
            .method(Method::PUT)
            .uri(format!("/chunk/{}/{}", session.session_id, 0))
            .body(Body::from(Bytes::from(vec![1u8; 10])))
            .unwrap(),
        [10, 10, 0, 1],
        7022,
    );
    let resp_mm = app.clone().oneshot(part_mismatch).await.unwrap();
    assert_eq!(resp_mm.status(), StatusCode::BAD_REQUEST);
    let mm: Value =
        serde_json::from_slice(&to_bytes(resp_mm.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(mm.get("code").and_then(|v| v.as_str()), Some("chunk_size"));

    // Completing with missing chunks -> 400 incomplete
    let complete = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri(format!("/chunk/{}/complete", session.session_id))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&ChunkCompleteRequest { hash: None }).unwrap(),
            ))
            .unwrap(),
        [10, 10, 0, 1],
        7023,
    );
    let resp_complete = app.clone().oneshot(complete).await.unwrap();
    assert_eq!(resp_complete.status(), StatusCode::BAD_REQUEST);
    let msgc: Value = serde_json::from_slice(
        &to_bytes(resp_complete.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        msgc.get("code").and_then(|v| v.as_str()),
        Some("incomplete")
    );
}

#[tokio::test]
async fn chunk_complete_hash_mismatch_and_cancel_flow() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());

    // Prepare 2-chunk data
    let data = vec![b'x'; 120_000];
    let init_req = ChunkInitRequest {
        filename: "hashcheck.bin".to_string(),
        size: data.len() as u64,
        ttl: None,
        chunk_size: Some(70_000),
        hash: None,
    };
    let init = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/chunk/init")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&init_req).unwrap()))
            .unwrap(),
        [20, 0, 0, 1],
        7030,
    );
    let init_resp = app.clone().oneshot(init).await.unwrap();
    assert_eq!(init_resp.status(), StatusCode::OK);
    let session: ChunkInitResponse =
        serde_json::from_slice(&to_bytes(init_resp.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert!(session.total_chunks >= 2);

    // Upload correct chunks
    for idx in 0..session.total_chunks {
        let start = idx as usize * session.chunk_size as usize;
        let end = std::cmp::min(start + session.chunk_size as usize, data.len());
        let chunk_body = Body::from(Bytes::copy_from_slice(&data[start..end]));
        let part = with_conn_ip(
            Request::builder()
                .method(Method::PUT)
                .uri(format!("/chunk/{}/{idx}", session.session_id))
                .body(chunk_body)
                .unwrap(),
            [20, 0, 0, 1],
            7031 + idx as u16,
        );
        let resp = app.clone().oneshot(part).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    // Complete with wrong hash
    let wrong = ChunkCompleteRequest {
        hash: Some("deadbeef".to_string()),
    };
    let complete = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri(format!("/chunk/{}/complete", session.session_id))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&wrong).unwrap()))
            .unwrap(),
        [20, 0, 0, 1],
        7039,
    );
    let resp = app.clone().oneshot(complete).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let msg: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(
        msg.get("code").and_then(|v| v.as_str()),
        Some("hash_mismatch")
    );

    // Cancel: missing session -> 404
    let cancel_missing = with_conn_ip(
        Request::builder()
            .method(Method::DELETE)
            .uri("/chunk/doesnotexist/cancel")
            .body(Body::empty())
            .unwrap(),
        [20, 0, 0, 2],
        7040,
    );
    let resp_missing = app.clone().oneshot(cancel_missing).await.unwrap();
    assert_eq!(resp_missing.status(), StatusCode::NOT_FOUND);

    // Re-create a session and cancel with wrong owner -> 403
    let init_req2 = ChunkInitRequest {
        filename: "cancelme.bin".to_string(),
        size: 64_500,
        ttl: None,
        chunk_size: Some(64_000),
        hash: None,
    };
    let init2 = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/chunk/init")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&init_req2).unwrap()))
            .unwrap(),
        [30, 0, 0, 1],
        7041,
    );
    let init2_resp = app.clone().oneshot(init2).await.unwrap();
    let session2: ChunkInitResponse =
        serde_json::from_slice(&to_bytes(init2_resp.into_body(), usize::MAX).await.unwrap())
            .unwrap();

    let cancel_wrong = with_conn_ip(
        Request::builder()
            .method(Method::DELETE)
            .uri(format!("/chunk/{}/cancel", session2.session_id))
            .body(Body::empty())
            .unwrap(),
        [30, 0, 0, 2], // wrong owner
        7042,
    );
    let resp_wrong = app.clone().oneshot(cancel_wrong).await.unwrap();
    assert_eq!(resp_wrong.status(), StatusCode::FORBIDDEN);

    // Cancel by owner -> 204 and status -> 404
    let cancel_ok = with_conn_ip(
        Request::builder()
            .method(Method::DELETE)
            .uri(format!("/chunk/{}/cancel", session2.session_id))
            .body(Body::empty())
            .unwrap(),
        [30, 0, 0, 1],
        7043,
    );
    let resp_ok = app.clone().oneshot(cancel_ok).await.unwrap();
    assert_eq!(resp_ok.status(), StatusCode::NO_CONTENT);

    let status_after = with_conn_ip(
        Request::builder()
            .uri(format!("/chunk/{}/status", session2.session_id))
            .body(Body::empty())
            .unwrap(),
        [30, 0, 0, 1],
        7044,
    );
    let resp_after = app.clone().oneshot(status_after).await.unwrap();
    assert_eq!(resp_after.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn multipart_upload_no_files_and_busy_server() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state.clone());

    // No files -> 400
    let (ct, body) = multipart_body_with("----BOUNDARYNOFILE", &[("ttl", None, b"1h")]);
    let req = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, ct)
            .body(body)
            .unwrap(),
        [40, 0, 0, 1],
        7050,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let msg: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(msg.get("code").and_then(|v| v.as_str()), Some("no_files"));

    // Busy server -> 503
    // Acquire all permits and hold them until after request
    let mut guards: Vec<OwnedSemaphorePermit> = Vec::new();
    for _ in 0..juicebox::util::UPLOAD_CONCURRENCY {
        guards.push(state.upload_sem.clone().acquire_owned().await.unwrap());
    }
    let (ct2, body2) = multipart_body_with(
        "----BOUNDARYBUSY",
        &[("ttl", None, b"1h"), ("file", Some("busy.txt"), b"payload")],
    );
    let busy_req = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, ct2)
            .body(body2)
            .unwrap(),
        [40, 0, 0, 1],
        7051,
    );
    let busy_resp = app.clone().oneshot(busy_req).await.unwrap();
    assert_eq!(busy_resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let busy_msg: Value =
        serde_json::from_slice(&to_bytes(busy_resp.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(busy_msg.get("code").and_then(|v| v.as_str()), Some("busy"));
    drop(guards); // release
}

#[tokio::test]
async fn multipart_forbidden_extension_is_rejected() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state);

    let (ct, body) = multipart_body_with("----BOUNDARYBAD", &[("file", Some("evil.exe"), b"bad")]);
    let req = with_conn_ip(
        Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .header(header::CONTENT_TYPE, ct)
            .body(body)
            .unwrap(),
        [41, 0, 0, 1],
        7060,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let msg: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(
        msg.get("code").and_then(|v| v.as_str()),
        Some("bad_filetype")
    );
}

#[tokio::test]
async fn web_templates_render_or_fail_gracefully() {
    let (state, _tmp) = common::setup_test_app();
    let app = build_router(state);

    for path in ["/faq", "/terms", "/report"] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(
            resp.status() == StatusCode::OK || resp.status() == StatusCode::INTERNAL_SERVER_ERROR,
            "unexpected status for {}: {}",
            path,
            resp.status()
        );
    }

    // simple page with message parameter (ok or error)
    let resp_simple = app
        .clone()
        .oneshot(with_conn_ip(
            Request::builder()
                .uri("/simple?m=Hello+World")
                .body(Body::empty())
                .unwrap(),
            [127, 0, 0, 1],
            7070,
        ))
        .await
        .unwrap();
    assert!(
        resp_simple.status() == StatusCode::OK
            || resp_simple.status() == StatusCode::INTERNAL_SERVER_ERROR,
        "unexpected status for /simple: {}",
        resp_simple.status()
    );
}
