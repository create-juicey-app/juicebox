mod common;

use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use http_body_util::BodyExt;
use juicebox::handlers::{admin_files_handler, visitor_debug_handler};
use juicebox::state::FileMeta;
use juicebox::util::{
    extract_client_ip, headers_trusted, now_secs, set_trusted_proxy_config_for_tests,
};
use once_cell::sync::Lazy;
use serde_json::Value;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
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

#[test]
fn extract_client_ip_trusts_loopback_proxy_without_cidr_match() {
    let _lock = PROXY_GUARD.lock().unwrap();
    set_trusted_proxy_config_for_tests(true, vec!["203.0.113.0/24".into()]);
    let mut headers = HeaderMap::new();
    headers.insert("CF-Connecting-IP", HeaderValue::from_static("198.51.100.9"));
    let remote = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let result = extract_client_ip(&headers, Some(remote));
    assert_eq!(result, "198.51.100.9");
    set_trusted_proxy_config_for_tests(false, Vec::new());
}

#[test]
fn headers_trusted_respects_private_sources() {
    let _lock = PROXY_GUARD.lock().unwrap();
    set_trusted_proxy_config_for_tests(true, vec!["198.51.100.0/24".into()]);
    let headers = HeaderMap::new();
    let loopback_v4 = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    assert!(headers_trusted(&headers, Some(loopback_v4)));
    let private_v4 = IpAddr::V4(Ipv4Addr::new(192, 168, 0, 5));
    assert!(headers_trusted(&headers, Some(private_v4)));
    let loopback_v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
    assert!(headers_trusted(&headers, Some(loopback_v6)));
    let ula_v6: Ipv6Addr = "fd12:3456:789a::1".parse().unwrap();
    assert!(headers_trusted(&headers, Some(IpAddr::V6(ula_v6))));
    let global_v4 = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5));
    assert!(!headers_trusted(&headers, Some(global_v4)));
    set_trusted_proxy_config_for_tests(false, Vec::new());
}

#[tokio::test]
async fn visitor_debug_reports_hash_and_owner_snapshot() {
    let _lock = PROXY_GUARD.lock().unwrap();
    set_trusted_proxy_config_for_tests(false, Vec::new());

    let (state, _tmp) = common::setup_test_app();
    let owner_hash = common::hash_fixture_ip("127.0.0.1");
    let now = now_secs();
    state.owners.insert(
        "demo.bin".to_string(),
        FileMeta {
            owner_hash: owner_hash.clone(),
            expires: now + 3600,
            original: "demo.bin".to_string(),
            created: now,
            hash: "deadbeef".into(),
        },
    );

    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("badbox.juicey.dev"));
    headers.insert("CF-Connecting-IP", HeaderValue::from_static("198.51.100.9"));
    headers.insert(
        "X-Forwarded-For",
        HeaderValue::from_static("198.51.100.9, 203.0.113.5"),
    );
    let addr: SocketAddr = SocketAddr::from(([127, 0, 0, 1], 4444));

    let resp = visitor_debug_handler(State(state.clone()), ConnectInfo(addr), headers).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let data: Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(data["client"]["real_ip"], "127.0.0.1");
    assert_eq!(data["client"]["headers_trusted"], false);
    assert!(data["client"]["hash"].is_object());
    assert!(data["client"]["edge_hash"].is_object());
    assert_eq!(data["owner"]["hash"], owner_hash);
    assert_eq!(data["owner"]["active_count"], 1);
    let files = data["owner"]["files_preview"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["file"], "demo.bin");
    assert_eq!(data["ban"], Value::Null);

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
            owner_hash: owner.to_string(),
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
    let truncated = if owner.len() <= 12 {
        owner.to_string()
    } else {
        format!("{}…", &owner[..12])
    };
    let escaped_owner = htmlescape::encode_minimal(&truncated);
    assert!(body.contains(&escaped_owner));
    assert!(!body.contains(owner));

    let expected_href = format!("/f/{}", encode(file_name));
    assert!(body.contains(&expected_href));

    let escaped_file = htmlescape::encode_minimal(file_name);
    assert!(body.contains(&format!("value=\"{}\"", escaped_file)));
}
