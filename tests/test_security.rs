mod common;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use http_body_util::BodyExt;
use juicebox::handlers::admin_files_handler;
use juicebox::state::FileMeta;
use juicebox::util::{extract_client_ip, now_secs, set_trusted_proxy_config_for_tests};
use once_cell::sync::Lazy;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Mutex;
use urlencoding::encode;

static PROXY_GUARD: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

#[test]
fn extract_client_ip_uses_socket_when_headers_not_trusted() {
    let _lock = PROXY_GUARD.lock().unwrap();
    set_trusted_proxy_config_for_tests(false, Vec::new());
    let mut headers = HeaderMap::new();
    headers.insert("CF-Connecting-IP", HeaderValue::from_static("203.0.113.5"));
    let remote = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let result = extract_client_ip(&headers, Some(remote));
    assert_eq!(result, "127.0.0.1");
}

#[test]
fn extract_client_ip_requires_trusted_proxy_source() {
    let _lock = PROXY_GUARD.lock().unwrap();
    set_trusted_proxy_config_for_tests(true, vec!["10.0.0.1/32".into()]);
    let mut headers = HeaderMap::new();
    headers.insert("CF-Connecting-IP", HeaderValue::from_static("198.51.100.9"));
    // Proxy not trusted (203.0.113.1)
    let remote = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1));
    let result = extract_client_ip(&headers, Some(remote));
    assert_eq!(result, "203.0.113.1");
    set_trusted_proxy_config_for_tests(false, Vec::new());
}

#[test]
fn extract_client_ip_trusts_headers_from_allowed_proxy() {
    let _lock = PROXY_GUARD.lock().unwrap();
    set_trusted_proxy_config_for_tests(true, vec!["10.0.0.1/32".into()]);
    let mut headers = HeaderMap::new();
    headers.insert("CF-Connecting-IP", HeaderValue::from_static("198.51.100.9"));
    let remote = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    let result = extract_client_ip(&headers, Some(remote));
    assert_eq!(result, "198.51.100.9");
    set_trusted_proxy_config_for_tests(false, Vec::new());
}

#[tokio::test]
async fn admin_files_handler_escapes_html_entities() {
    let (state, _tmp) = common::setup_test_app();
    let token = "admintoken".to_string();
    state.create_admin_session(token.clone()).await;

    let now = now_secs();
    let file_name = "bad\">\"<script>alert(1)</script>.txt";
    let owner = "<script>alert('pwn')</script>";
    state.owners.insert(
        file_name.to_string(),
        FileMeta {
            owner: owner.to_string(),
            expires: now + 3600,
            original: file_name.to_string(),
            created: now,
            hash: "deadbeef".into(),
        },
    );

    tokio::fs::write(
        state.static_dir.join("admin_files.html"),
        "<html><body><table>{{FILE_ROWS}}</table></body></html>",
    )
    .await
    .unwrap();

    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!("adm={token}")).unwrap(),
    );

    let resp = admin_files_handler(State(state.clone()), headers).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body_bytes.to_vec()).unwrap();
    let escaped_owner = htmlescape::encode_minimal(owner);
    assert!(body.contains(&escaped_owner));
    assert!(!body.contains(owner));

    let expected_href = format!("/f/{}", encode(file_name));
    assert!(body.contains(&expected_href));

    let escaped_file = htmlescape::encode_minimal(file_name);
    assert!(body.contains(&format!("value=\"{}\"", escaped_file)));
}
