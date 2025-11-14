use axum::{
    Json,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use hmac::{Hmac, Mac};
use sanitize_filename::sanitize;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
// removed rand; using cuid now
use crate::state::AppState;
use once_cell::sync::Lazy;
use std::sync::RwLock;

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
pub static PROD_HOST: Lazy<String> = Lazy::new(|| {
    std::env::var("JUICEBOX_PROD_HOST")
        .ok()
        .map(|value| {
            let trimmed = value.trim();
            if let Some((_, rest)) = trimmed.split_once("//") {
                trim_host(rest)
            } else {
                trim_host(trimmed)
            }
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "box.juicey.dev".to_string())
});

const UNKNOWN: &str = "unknown";

fn trim_host(input: &str) -> String {
    let without_path = input.split(['/', '?', '#']).next().unwrap_or(input);
    without_path.trim().trim_matches('/').to_string()
}
// Disallowed extensions
pub const FORBIDDEN_EXTENSIONS: &[&str] = &[
    "exe", "dll", "bat", "cmd", "com", "scr", "cpl", "msi", "msp", "jar", "ps1", "psm1", "vbs",
    "js", "jse", "wsf", "wsh", "reg", "sh", "php", "pl", "py", "rb", "gadget", "hta", "mht",
    "mhtml",
];

#[derive(Debug)]
struct TrustedProxyConfig {
    allow_headers: bool,
    trusted_proxies: Vec<String>,
}

fn parse_truthy_env(var: &str) -> bool {
    std::env::var(var)
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

static TRUSTED_PROXY_CONFIG: Lazy<RwLock<TrustedProxyConfig>> = Lazy::new(|| {
    let allow_headers = parse_truthy_env("TRUST_PROXY_HEADERS");
    let trusted_proxies = std::env::var("TRUSTED_PROXY_CIDRS")
        .map(|raw| {
            raw.split(',')
                .map(|segment| segment.trim())
                .filter(|segment| !segment.is_empty())
                .map(|segment| segment.to_string())
                .collect::<Vec<String>>()
        })
        .unwrap_or_else(|_| Vec::new());
    RwLock::new(TrustedProxyConfig {
        allow_headers,
        trusted_proxies,
    })
});

fn is_local_proxy(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
    }
}

fn proxy_source_trusted(cfg: &TrustedProxyConfig, source_ip: IpAddr) -> bool {
    if is_local_proxy(&source_ip) {
        return true;
    }
    cfg.trusted_proxies.is_empty()
        || cfg
            .trusted_proxies
            .iter()
            .any(|cidr| ip_in_cidr(source_ip, cidr))
}

#[derive(Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: &'static str,
}

pub fn json_error(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    let body = Json(ErrorBody { code, message });
    let mut resp = (status, body).into_response();
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    resp
}

// New ID generator using CUID v2 (fast, shorter) fallback to v1 on error
pub fn new_id() -> String {
    cuid::cuid2()
}

pub fn is_forbidden_extension(name: &str) -> bool {
    if let Some(dot) = name.rfind('.') {
        if dot > 0 {
            let ext = &name[dot + 1..].to_ascii_lowercase();
            return FORBIDDEN_EXTENSIONS.contains(&ext.as_str());
        }
    }
    false
}

pub fn make_storage_name(original: Option<&str>) -> String {
    if let Some(orig) = original {
        let sanitized = sanitize(orig);
        if let Some(dot) = sanitized.rfind('.') {
            if dot > 0 {
                let ext = &sanitized[dot + 1..];
                if !ext.is_empty()
                    && ext.len() <= 12
                    && ext.chars().all(|c| c.is_ascii_alphanumeric())
                {
                    return format!("{}.{ext}", new_id());
                }
            }
        }
    }
    new_id()
}

pub fn ttl_to_duration(code: &str) -> Duration {
    match code {
        "1h" => Duration::from_secs(3600),
        "3h" => Duration::from_secs(3 * 3600),
        "12h" => Duration::from_secs(12 * 3600),
        "1d" => Duration::from_secs(24 * 3600),
        "3d" => Duration::from_secs(3 * 24 * 3600),
        "7d" => Duration::from_secs(7 * 24 * 3600),
        "14d" => Duration::from_secs(14 * 24 * 3600),
        _ => Duration::from_secs(3 * 24 * 3600),
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

pub fn looks_like_hash(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IpVersion {
    V4,
    V6,
}

type HmacSha256 = Hmac<Sha256>;

fn hash_with_secret(secret: &[u8], payload: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret)
        .expect("HMAC key initialization should accept arbitrary key length");
    mac.update(payload);
    let result = mac.finalize().into_bytes();
    let mut hex = String::with_capacity(result.len() * 2);
    for byte in result {
        hex.push_str(&format!("{:02x}", byte));
    }
    hex
}

fn ip_version_tag(ip: &IpAddr) -> (&'static str, IpVersion) {
    match ip {
        IpAddr::V4(_) => ("v4", IpVersion::V4),
        IpAddr::V6(_) => ("v6", IpVersion::V6),
    }
}

fn normalize_v4(addr: Ipv4Addr, prefix: u8) -> Option<Ipv4Addr> {
    if prefix > 32 {
        return None;
    }
    if prefix == 0 {
        return Some(Ipv4Addr::UNSPECIFIED);
    }
    let mask = u32::MAX << (32 - prefix);
    Some(Ipv4Addr::from(u32::from(addr) & mask))
}

fn normalize_v6(addr: Ipv6Addr, prefix: u8) -> Option<Ipv6Addr> {
    if prefix > 128 {
        return None;
    }
    if prefix == 0 {
        return Some(Ipv6Addr::UNSPECIFIED);
    }
    let mask: u128 = u128::MAX << (128 - prefix);
    Some(Ipv6Addr::from(u128::from(addr) & mask))
}

pub fn hash_ip_string(secret: &[u8], raw: &str) -> Option<(IpVersion, String)> {
    let ip: IpAddr = raw.trim().parse().ok()?;
    Some(hash_ip_addr(secret, &ip))
}

pub fn hash_ip_addr(secret: &[u8], ip: &IpAddr) -> (IpVersion, String) {
    let (tag, version) = ip_version_tag(ip);
    let payload = match ip {
        IpAddr::V4(v4) => format!("ip|{tag}|{v4}"),
        IpAddr::V6(v6) => format!("ip|{tag}|{v6}"),
    };
    (version, hash_with_secret(secret, payload.as_bytes()))
}

pub fn hash_network_from_ip(
    secret: &[u8],
    ip: &IpAddr,
    prefix: u8,
) -> Option<(IpVersion, u8, String)> {
    match ip {
        IpAddr::V4(v4) => {
            if prefix > 32 {
                return None;
            }
            let base = normalize_v4(*v4, prefix)?;
            let payload = format!("net|v4|{prefix}|{base}");
            Some((
                IpVersion::V4,
                prefix,
                hash_with_secret(secret, payload.as_bytes()),
            ))
        }
        IpAddr::V6(v6) => {
            if prefix > 128 {
                return None;
            }
            let base = normalize_v6(*v6, prefix)?;
            let payload = format!("net|v6|{prefix}|{base}");
            Some((
                IpVersion::V6,
                prefix,
                hash_with_secret(secret, payload.as_bytes()),
            ))
        }
    }
}

pub fn hash_network_from_cidr(secret: &[u8], cidr: &str) -> Option<(IpVersion, u8, String)> {
    let mut parts = cidr.split('/');
    let base = parts.next()?.trim();
    let prefix = parts.next()?.trim().parse::<u8>().ok()?;
    let ip: IpAddr = base.parse().ok()?;
    hash_network_from_ip(secret, &ip, prefix)
}

pub fn qualify_path(state: &AppState, path: &str) -> String {
    if state.production {
        let p = path.trim_start_matches('/');
        format!("https://{}/{}", PROD_HOST.as_str(), p)
    } else {
        path.to_string()
    }
}

// Network / IP helpers
fn ip_in_cidr(ip: IpAddr, cidr: &str) -> bool {
    let mut parts = cidr.split('/');
    let base = parts.next().unwrap_or("");
    let prefix: u8 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    if let Ok(base_ip) = base.parse::<IpAddr>() {
        match (ip, base_ip) {
            (IpAddr::V4(a), IpAddr::V4(b)) => {
                let mask = if prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - prefix)
                };
                (u32::from(a) & mask) == (u32::from(b) & mask)
            }
            (IpAddr::V6(a), IpAddr::V6(b)) => {
                let a_bytes = a.octets();
                let b_bytes = b.octets();
                let full = (prefix / 8) as usize;
                if a_bytes[..full] != b_bytes[..full] {
                    return false;
                }
                let rem = prefix % 8;
                if rem == 0 {
                    return true;
                }
                let mask = 0xFF << (8 - rem);
                (a_bytes[full] & mask) == (b_bytes[full] & mask)
            }
            _ => false,
        }
    } else {
        false
    }
}

#[allow(dead_code)]
pub fn is_cloudflare_edge(remote: IpAddr) -> bool {
    const CF_CIDRS: &[&str] = &[
        "173.245.48.0/20",
        "103.21.244.0/22",
        "103.22.200.0/22",
        "103.31.4.0/22",
        "141.101.64.0/18",
        "108.162.192.0/18",
        "190.93.240.0/20",
        "188.114.96.0/20",
        "197.234.240.0/22",
        "198.41.128.0/17",
        "162.158.0.0/15",
        "104.16.0.0/13",
        "104.24.0.0/14",
        "172.64.0.0/13",
        "131.0.72.0/22",
        "2400:cb00::/32",
        "2606:4700::/32",
        "2803:f800::/32",
        "2405:b500::/32",
        "2405:8100::/32",
        "2a06:98c0::/29",
        "2c0f:f248::/32",
    ];
    CF_CIDRS.iter().any(|c| ip_in_cidr(remote, c))
}

fn parse_ip(value: &str) -> Option<IpAddr> {
    value.trim().parse::<IpAddr>().ok()
}

pub fn extract_client_ip(headers: &HeaderMap, fallback: Option<IpAddr>) -> String {
    {
        let cfg = TRUSTED_PROXY_CONFIG
            .read()
            .expect("trusted proxy configuration poisoned");
        if cfg.allow_headers {
            if let Some(source_ip) = fallback {
                if proxy_source_trusted(&cfg, source_ip) {
                    if let Some(ip) = headers
                        .get("CF-Connecting-IP")
                        .and_then(|v| v.to_str().ok())
                        .and_then(parse_ip)
                    {
                        return ip.to_string();
                    }
                    if let Some(ip) = headers
                        .get("True-Client-IP")
                        .and_then(|v| v.to_str().ok())
                        .and_then(parse_ip)
                    {
                        return ip.to_string();
                    }
                    if let Some(ip) = headers
                        .get("X-Real-IP")
                        .and_then(|v| v.to_str().ok())
                        .and_then(parse_ip)
                    {
                        return ip.to_string();
                    }
                    if let Some(val) = headers.get("X-Forwarded-For").and_then(|v| v.to_str().ok())
                    {
                        for candidate in val.split(',') {
                            if let Some(ip) = parse_ip(candidate) {
                                return ip.to_string();
                            }
                        }
                    }
                }
            }
        }
    }
    fallback
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "unknown".into())
}

pub fn real_client_ip(headers: &HeaderMap, fallback: &std::net::SocketAddr) -> String {
    extract_client_ip(headers, Some(fallback.ip()))
}

/// Return whether forwarded headers from the provided `headers` should be trusted
/// for a connection that arrived from `fallback` (the socket peer IP).
pub fn headers_trusted(_headers: &HeaderMap, fallback: Option<IpAddr>) -> bool {
    let cfg = TRUSTED_PROXY_CONFIG
        .read()
        .expect("trusted proxy configuration poisoned");
    if !cfg.allow_headers {
        return false;
    }
    if let Some(source_ip) = fallback {
        return proxy_source_trusted(&cfg, source_ip);
    }
    false
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn set_trusted_proxy_config_for_tests(allow_headers: bool, cidrs: Vec<String>) {
    if let Ok(mut cfg) = TRUSTED_PROXY_CONFIG.write() {
        cfg.allow_headers = allow_headers;
        cfg.trusted_proxies = cidrs;
    }
}

// new: max simultaneous active files per IP
pub const MAX_ACTIVE_FILES_PER_IP: usize = 10;

// admin session ttl (seconds)
pub const ADMIN_SESSION_TTL: u64 = 24 * 3600;

// admin key ttl (seconds) - duration before rotating the underlying master key used at /auth
pub const ADMIN_KEY_TTL: u64 = 30 * 24 * 3600; // 30 days

pub fn get_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookie_header = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    for part in cookie_header.split(';') {
        let mut kv = part.trim().splitn(2, '=');
        let k = kv.next()?.trim();
        if k == name {
            return kv.next().map(|v| v.trim().to_string());
        }
    }
    None
}

// Helper: parse human-readable size (e.g. "500MB", "1GB")
pub fn parse_size_bytes(input: &str) -> Option<u64> {
    let cleaned: String = input
        .trim()
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '_')
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    let lower = cleaned.to_ascii_lowercase();
    let (number, multiplier): (&str, u128) = [
        ("tib", 1024_u128.pow(4)),
        ("tb", 1024_u128.pow(4)),
        ("gib", 1024_u128.pow(3)),
        ("gb", 1024_u128.pow(3)),
        ("mib", 1024_u128.pow(2)),
        ("mb", 1024_u128.pow(2)),
        ("kib", 1024_u128),
        ("kb", 1024_u128),
        ("bytes", 1_u128),
        ("byte", 1_u128),
        ("b", 1_u128),
    ]
    .into_iter()
    .find_map(|(suffix, mult)| lower.strip_suffix(suffix).map(|num| (num, mult)))
    .unwrap_or_else(|| (lower.as_str(), 1_u128));
    let value = number.parse::<u128>().ok()?;
    let total = value.checked_mul(multiplier)?;
    if total > u64::MAX as u128 {
        return None;
    }
    Some(total as u64)
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

pub fn git_branch() -> &'static str {
    sanitized_env(option_env!("JUICEBOX_GIT_BRANCH")).unwrap_or(UNKNOWN)
}

pub fn git_commit() -> &'static str {
    sanitized_env(option_env!("JUICEBOX_GIT_COMMIT"))
        .or_else(|| sanitized_env(option_env!("JUICEBOX_GIT_COMMIT_SHORT")))
        .unwrap_or(UNKNOWN)
}

pub fn git_commit_short() -> &'static str {
    if let Some(short) = sanitized_env(option_env!("JUICEBOX_GIT_COMMIT_SHORT")) {
        return truncate_commit(short);
    }

    if let Some(full) = sanitized_env(option_env!("JUICEBOX_GIT_COMMIT")) {
        return truncate_commit(full);
    }

    UNKNOWN
}

fn sanitized_env(value: Option<&'static str>) -> Option<&'static str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

fn truncate_commit(value: &'static str) -> &'static str {
    value
        .char_indices()
        .nth(12)
        .map(|(idx, _)| &value[..idx])
        .unwrap_or(value)
}
