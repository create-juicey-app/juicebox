use axum::{http::{StatusCode, HeaderMap}, response::{IntoResponse, Response}, Json};
use serde::{Serialize};
use std::{time::{SystemTime, UNIX_EPOCH, Duration}, net::IpAddr};
use sanitize_filename::sanitize;
// removed rand; using cuid now
use crate::state::AppState;
use once_cell::sync::Lazy;

// Public constants
// RANDOM_NAME_LEN removed (no longer needed with CUID)
pub const UPLOAD_CONCURRENCY: usize = 8;
// Replace const with a static that reads from env at startup
static MAX_FILE_BYTES: Lazy<u64> = Lazy::new(|| {
    std::env::var("MAX_FILE_SIZE")
        .ok()
        .and_then(|v| parse_size_bytes(&v))
        .unwrap_or(500 * 1024 * 1024) // default 500MB
});
pub const PROD_HOST: &str = "box.juicey.dev";
// Disallowed extensions
pub const FORBIDDEN_EXTENSIONS: &[&str] = &["exe","dll","bat","cmd","com","scr","cpl","msi","msp","jar","ps1","psm1","vbs","js","jse","wsf","wsh","reg","sh","php","pl","py","rb","gadget","hta","mht","mhtml"];

#[derive(Serialize)]
pub struct ErrorBody { pub code: &'static str, pub message: &'static str }

pub fn json_error(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    let body = Json(ErrorBody { code, message });
    (status, body).into_response()
}

// New ID generator using CUID v2 (fast, shorter) fallback to v1 on error
pub fn new_id() -> String { cuid::cuid2() }

pub fn is_forbidden_extension(name: &str) -> bool {
    if let Some(dot) = name.rfind('.') { if dot > 0 { let ext = &name[dot+1..].to_ascii_lowercase(); return FORBIDDEN_EXTENSIONS.contains(&ext.as_str()); } }
    false
}

pub fn make_storage_name(original: Option<&str>) -> String {
    if let Some(orig) = original {
        let sanitized = sanitize(orig);
        if let Some(dot) = sanitized.rfind('.') { if dot > 0 { let ext = &sanitized[dot+1..]; if !ext.is_empty() && ext.len() <= 12 && ext.chars().all(|c| c.is_ascii_alphanumeric()) { return format!("{}.{ext}", new_id()); } } }
    }
    new_id()
}

pub fn ttl_to_duration(code: &str) -> Duration {
    match code {"1h"=>Duration::from_secs(3600),"3h"=>Duration::from_secs(3*3600),"12h"=>Duration::from_secs(12*3600),"1d"=>Duration::from_secs(24*3600),"3d"=>Duration::from_secs(3*24*3600),"7d"=>Duration::from_secs(7*24*3600),"14d"=>Duration::from_secs(14*24*3600),_=>Duration::from_secs(3*24*3600)}
}

pub fn now_secs() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0)).as_secs() }

pub fn qualify_path(state: &AppState, path: &str) -> String {
    if state.production { let p = path.trim_start_matches('/'); format!("https://{}/{}", PROD_HOST, p) } else { path.to_string() }
}

// Network / IP helpers
fn ip_in_cidr(ip: IpAddr, cidr: &str) -> bool {
    let mut parts = cidr.split('/');
    let base = parts.next().unwrap_or("");
    let prefix: u8 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    if let Ok(base_ip) = base.parse::<IpAddr>() { match (ip, base_ip) {
        (IpAddr::V4(a), IpAddr::V4(b)) => { let mask = if prefix==0 {0} else {u32::MAX << (32 - prefix)}; (u32::from(a) & mask) == (u32::from(b) & mask) }
        (IpAddr::V6(a), IpAddr::V6(b)) => { let a_bytes=a.octets(); let b_bytes=b.octets(); let full=(prefix/8) as usize; if a_bytes[..full]!=b_bytes[..full]{return false;} let rem=prefix%8; if rem==0 {return true;} let mask=0xFF << (8-rem); (a_bytes[full] & mask)==(b_bytes[full] & mask) }
        _=>false }} else { false }
}

#[allow(dead_code)]
pub fn is_cloudflare_edge(remote: IpAddr) -> bool {
    const CF_CIDRS: &[&str] = &["173.245.48.0/20","103.21.244.0/22","103.22.200.0/22","103.31.4.0/22","141.101.64.0/18","108.162.192.0/18","190.93.240.0/20","188.114.96.0/20","197.234.240.0/22","198.41.128.0/17","162.158.0.0/15","104.16.0.0/13","104.24.0.0/14","172.64.0.0/13","131.0.72.0/22","2400:cb00::/32","2606:4700::/32","2803:f800::/32","2405:b500::/32","2405:8100::/32","2a06:98c0::/29","2c0f:f248::/32"];
    CF_CIDRS.iter().any(|c| ip_in_cidr(remote, c))
}

pub fn extract_client_ip(headers: &HeaderMap, fallback: Option<IpAddr>) -> String {
    if let Some(val) = headers.get("CF-Connecting-IP").and_then(|v| v.to_str().ok()) { if let Ok(ip)=val.trim().parse::<IpAddr>() { return ip.to_string(); } }
    if let Some(val) = headers.get("X-Forwarded-For").and_then(|v| v.to_str().ok()) { if let Some(first)=val.split(',').next() { if let Ok(ip)=first.trim().parse::<IpAddr>() { return ip.to_string(); } } }
    fallback.map(|ip| ip.to_string()).unwrap_or_else(|| "unknown".into())
}

pub fn real_client_ip(headers: &HeaderMap, fallback: &std::net::SocketAddr) -> String { extract_client_ip(headers, Some(fallback.ip())) }

// new: max simultaneous active files per IP
pub const MAX_ACTIVE_FILES_PER_IP: usize = 5;

// admin session ttl (seconds)
pub const ADMIN_SESSION_TTL: u64 = 24 * 3600;

// admin key ttl (seconds) - duration before rotating the underlying master key used at /auth
pub const ADMIN_KEY_TTL: u64 = 30 * 24 * 3600; // 30 days

pub fn get_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookie_header = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    for part in cookie_header.split(';') {
        let mut kv = part.trim().splitn(2, '=');
        let k = kv.next()?.trim();
        if k == name { return kv.next().map(|v| v.trim().to_string()); }
    }
    None
}

// Helper: parse human-readable size (e.g. "500MB", "1GB")
fn parse_size_bytes(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    if let Some(num) = s.strip_suffix("gb") {
        num.trim().parse::<u64>().ok().map(|n| n * 1024 * 1024 * 1024)
    } else if let Some(num) = s.strip_suffix("mb") {
        num.trim().parse::<u64>().ok().map(|n| n * 1024 * 1024)
    } else if let Some(num) = s.strip_suffix("kb") {
        num.trim().parse::<u64>().ok().map(|n| n * 1024)
    } else if let Some(num) = s.strip_suffix("b") {
        num.trim().parse::<u64>().ok()
    } else {
        s.parse::<u64>().ok()
    }
}

// Helper: format bytes as human readable (e.g. 500MB, 1GB)
pub fn format_bytes(n: u64) -> String {
    if n >= 1024 * 1024 * 1024 {
        format!("{:.0}GB", n as f64 / 1024.0 / 1024.0 / 1024.0)
    } else if n >= 1024 * 1024 {
        format!("{:.0}MB", n as f64 / 1024.0 / 1024.0)
    } else if n >= 1024 {
        format!("{:.0}KB", n as f64 / 1024.0)
    } else {
        format!("{}B", n)
    }
}

pub fn max_file_bytes() -> u64 {
    *MAX_FILE_BYTES
}
