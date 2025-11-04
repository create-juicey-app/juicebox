mod common;

use axum::http::{HeaderMap, HeaderValue, header};

use juicebox::util::{
    IpVersion, format_bytes, get_cookie, hash_ip_addr, hash_ip_string, hash_network_from_cidr,
    hash_network_from_ip, is_forbidden_extension, looks_like_hash, make_storage_name, qualify_path,
    ttl_to_duration,
};

#[test]
fn test_format_bytes_variants() {
    assert_eq!(format_bytes(0), "0B");
    assert_eq!(format_bytes(1023), "1023B");
    assert_eq!(format_bytes(1024), "1KB");
    assert_eq!(format_bytes(2 * 1024 * 1024), "2MB");
    assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3GB");
}

#[test]
fn test_is_forbidden_extension_case_insensitive() {
    assert!(is_forbidden_extension("virus.EXE"));
    assert!(is_forbidden_extension("payload.sh"));
    assert!(!is_forbidden_extension("document.pdf"));
    assert!(!is_forbidden_extension("noext"));
}

#[test]
fn test_make_storage_name_preserves_extension_and_sanitizes() {
    let storage = make_storage_name(Some("Report.PDF"));
    // Should end with .PDF (case may be preserved)
    assert!(
        storage.ends_with(".PDF") || storage.ends_with(".pdf"),
        "unexpected storage name: {storage}"
    );
    assert!(storage.len() > ".PDF".len());

    // Disallow invalid extension characters; fall back to bare id
    let storage2 = make_storage_name(Some("bad name.invalid-ext!"));
    assert!(
        !storage2.ends_with(".invalid-ext!") && !storage2.contains(' '),
        "unexpected storage name: {storage2}"
    );
}

#[test]
fn test_ttl_to_duration_mapping_and_default() {
    assert_eq!(ttl_to_duration("1h").as_secs(), 3600);
    assert_eq!(ttl_to_duration("3h").as_secs(), 3 * 3600);
    assert_eq!(ttl_to_duration("12h").as_secs(), 12 * 3600);
    assert_eq!(ttl_to_duration("1d").as_secs(), 24 * 3600);
    assert_eq!(ttl_to_duration("3d").as_secs(), 3 * 24 * 3600);
    assert_eq!(ttl_to_duration("7d").as_secs(), 7 * 24 * 3600);
    assert_eq!(ttl_to_duration("14d").as_secs(), 14 * 24 * 3600);
    // default fallback
    assert_eq!(ttl_to_duration("bogus").as_secs(), 3 * 24 * 3600);
}

#[test]
fn test_looks_like_hash_validation() {
    let good = "a".repeat(64);
    assert!(looks_like_hash(&good));
    assert!(!looks_like_hash("g".repeat(64).as_str())); // non-hex
    assert!(!looks_like_hash(&"a".repeat(63))); // wrong length
}

#[test]
fn test_get_cookie_parsing() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        HeaderValue::from_static("foo=bar; adm = xyz ; other=a=b"),
    );
    assert_eq!(get_cookie(&headers, "foo").as_deref(), Some("bar"));
    assert_eq!(get_cookie(&headers, "adm").as_deref(), Some("xyz"));
    assert_eq!(get_cookie(&headers, "other").as_deref(), Some("a=b"));
    assert!(get_cookie(&headers, "missing").is_none());

    // no cookie header present
    let headers2 = HeaderMap::new();
    assert!(get_cookie(&headers2, "foo").is_none());
}

#[test]
fn test_hash_ip_determinism_and_version() {
    let ip_v4 = "198.51.100.7";
    let ip_v6 = "2001:db8::1";

    // Deterministic for same IP + secret
    let (ver4_a, h4_a) = hash_ip_string(&common::PRIMARY_HASH_SECRET, ip_v4).unwrap();
    let (ver4_b, h4_b) = hash_ip_string(&common::PRIMARY_HASH_SECRET, ip_v4).unwrap();
    assert_eq!(ver4_a, IpVersion::V4);
    assert_eq!(ver4_b, IpVersion::V4);
    assert_eq!(h4_a, h4_b);

    // Different secret should produce different hash
    let (_ver4_c, h4_c) = hash_ip_string(&common::SECONDARY_HASH_SECRET, ip_v4).unwrap();
    assert_ne!(h4_a, h4_c);

    // IPv6 tag
    let (ver6, _h6) = hash_ip_string(&common::PRIMARY_HASH_SECRET, ip_v6).unwrap();
    assert_eq!(ver6, IpVersion::V6);

    // Direct addr API matches string API
    let addr_v4: std::net::IpAddr = ip_v4.parse().unwrap();
    let (ver4_d, h4_d) = hash_ip_addr(&common::PRIMARY_HASH_SECRET, &addr_v4);
    assert_eq!(ver4_d, IpVersion::V4);
    assert_eq!(h4_d, h4_a);
}

#[test]
fn test_network_hashing_matches_cidr_and_prefix() {
    // Same /24 should hash the same; different /24 should not
    let a: std::net::IpAddr = "203.0.113.10".parse().unwrap();
    let b: std::net::IpAddr = "203.0.113.200".parse().unwrap();
    let c: std::net::IpAddr = "203.0.114.5".parse().unwrap();

    let (va, pa, ha) = hash_network_from_ip(&common::PRIMARY_HASH_SECRET, &a, 24).unwrap();
    let (vb, pb, hb) = hash_network_from_ip(&common::PRIMARY_HASH_SECRET, &b, 24).unwrap();
    let (vc, pc, hc) = hash_network_from_ip(&common::PRIMARY_HASH_SECRET, &c, 24).unwrap();
    assert_eq!(va, IpVersion::V4);
    assert_eq!((va, pa), (vb, pb));
    assert_eq!(ha, hb);
    assert_ne!(ha, hc);
    assert_eq!(pc, 24);
    assert_eq!(vc, IpVersion::V4);

    // CIDR string should match network-from-ip
    let from_cidr = hash_network_from_cidr(&common::PRIMARY_HASH_SECRET, "203.0.113.0/24").unwrap();
    assert_eq!(from_cidr.0, va);
    assert_eq!(from_cidr.1, pa);
    assert_eq!(from_cidr.2, ha);
}

#[test]
fn test_qualify_path_non_production_passthrough() {
    let (state, _tmp) = common::setup_test_app();
    assert!(
        !state.production,
        "test state should be non-production by default"
    );

    let p1 = qualify_path(&state, "/alpha/beta");
    let p2 = qualify_path(&state, "alpha/beta");
    assert_eq!(p1, "/alpha/beta");
    assert_eq!(p2, "alpha/beta");
}

#[test]
fn test_qualify_path_production_prefix() {
    let (mut state, _tmp) = common::setup_test_app();
    state.production = true;

    let q1 = qualify_path(&state, "/a/b");
    let q2 = qualify_path(&state, "a/b");

    // Both should normalize to the same canonical URL
    assert!(q1.starts_with("https://"));
    assert!(q2.starts_with("https://"));
    assert!(q1.ends_with("/a/b"), "unexpected: {q1}");
    assert!(q2.ends_with("/a/b"), "unexpected: {q2}");
    assert_eq!(q1, q2);
}
