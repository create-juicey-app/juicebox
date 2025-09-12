use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{StatusCode, HeaderMap},
    response::{IntoResponse, Response},
    routing::{get, post, delete},
    Router, Json,
};
use sanitize_filename::sanitize;
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use axum::extract::ConnectInfo;
use std::net::SocketAddr as ClientAddr;
use serde::{Serialize, Deserialize};
use serde_json;
use rand::{Rng, rng};
use std::collections::HashMap;
use tokio::sync::{RwLock, Semaphore};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use std::time::{SystemTime, Duration, UNIX_EPOCH};
use tokio_util::io::ReaderStream;
use std::task::{Context, Poll};
use std::future::Future;
use std::pin::Pin;
use std::time::Instant;
use hyper::Request;
use axum::body::Body;
use axum::middleware::{self, Next};
use axum::Form;
use axum::extract::Query;
use std::borrow::Cow;
use axum::http::HeaderValue;
use std::net::{IpAddr};

// === Simple in-memory token bucket rate limiting (per IP) ===
struct RateLimitConfig { capacity: u32, refill_per_second: u32 }

#[derive(Clone)]
struct RateLimiterInner { buckets: Arc<RwLock<HashMap<String, RateBucket>>>, cfg: Arc<RateLimitConfig> }

#[derive(Clone, Debug)]
struct RateBucket { tokens: f64, last: Instant }

impl RateLimiterInner {
    fn new(capacity: u32, refill_per_second: u32) -> Self {
        Self { buckets: Arc::new(RwLock::new(HashMap::new())), cfg: Arc::new(RateLimitConfig{capacity, refill_per_second}) }
    }
    async fn check(&self, ip: &str) -> bool {
        let mut map = self.buckets.write().await;
        let entry = map.entry(ip.to_string()).or_insert(RateBucket{ tokens: self.cfg.capacity as f64, last: Instant::now() });
        let now = Instant::now();
        let elapsed = now.duration_since(entry.last).as_secs_f64();
        if elapsed > 0.0 {
            let refill = elapsed * self.cfg.refill_per_second as f64;
            entry.tokens = (entry.tokens + refill).min(self.cfg.capacity as f64);
            entry.last = now;
        }
        if entry.tokens >= 1.0 { entry.tokens -= 1.0; true } else { false }
    }
}

#[derive(Clone)]
struct RateLimitLayer { limiter: RateLimiterInner }

impl<S> tower::Layer<S> for RateLimitLayer {
    type Service = RateLimitService<S>;
    fn layer(&self, inner: S) -> Self::Service { RateLimitService { inner, limiter: self.limiter.clone() } }
}

#[derive(Clone)]
struct RateLimitService<S> { inner: S, limiter: RateLimiterInner }

impl<S> tower::Service<Request<Body>> for RateLimitService<S>
where S: tower::Service<Request<Body>, Response = Response> + Clone + Send + 'static,
      S::Error: std::fmt::Display,
      S::Future: Send + 'static {
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output=Result<Self::Response, Self::Error>> + Send>>;
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> { self.inner.poll_ready(cx) }
    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let limiter = self.limiter.clone();
        let mut inner = self.inner.clone();
        let headers_clone = req.headers().clone();
        let maybe_ci = req.extensions().get::<ConnectInfo<ClientAddr>>().cloned();
        let ip = if let Some(ci) = maybe_ci {
            let remote = ci.0.ip();
            if is_cloudflare_edge(remote) {
                if let Some(cf) = headers_clone.get("cf-connecting-ip").and_then(|v| v.to_str().ok()).and_then(|s| s.parse::<IpAddr>().ok()) { cf.to_string() }
                else if let Some(xff) = headers_clone.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
                    xff.split(',').next().and_then(|first| first.trim().parse::<IpAddr>().ok()).map(|ip| ip.to_string()).unwrap_or_else(|| remote.to_string())
                } else { remote.to_string() }
            } else { remote.to_string() }
        } else { "unknown".into() };
        Box::pin(async move {
            if !limiter.check(&ip).await {
                return Ok(json_error(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "slow down"));
            }
            inner.call(req).await
        })
    }
}

const RANDOM_NAME_LEN: usize = 18; // increased length for reduced collision probability
const UPLOAD_CONCURRENCY: usize = 8; // simple cap on simultaneous uploads
const MAX_FILE_BYTES: u64 = 500 * 1024 * 1024; // 500MB soft limit (server body limit 512MB)
const PROD_HOST: &str = "box.juicey.dev"; // canonical production hostname

#[derive(Serialize)]
struct ErrorBody { code: &'static str, message: &'static str }

fn json_error(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    let body = Json(ErrorBody { code, message });
    (status, body).into_response()
}

#[derive(Clone)]
struct AppState {
    upload_dir: Arc<PathBuf>,          // ./files (binary storage)
    static_dir: Arc<PathBuf>,          // ./public (static assets)
    metadata_path: Arc<PathBuf>,       // ./files/file_owners.json
    owners: Arc<RwLock<HashMap<String, FileMeta>>>, // filename -> meta
    upload_sem: Arc<Semaphore>,        // limit concurrent uploads
    production: bool,                  // new: whether running in production (APP_ENV=production)
    last_meta_mtime: Arc<RwLock<SystemTime>>, // new: track metadata file mtime for live admin edits
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct FileMeta {
    owner: String,
    expires: u64, // unix epoch seconds
}

#[derive(Serialize)]
struct UploadResponse {
    files: Vec<String>, // return paths like "f/<filename>" no IP leakage
}

#[derive(Serialize)]
struct ListResponse {
    files: Vec<String>,
    metas: Vec<FileMetaEntry>,
    reconcile: Option<ReconcileReport>, // new: report of admin / disk adjustments
}

#[derive(Serialize)]
struct FileMetaEntry {
    file: String,
    expires: u64,
}

#[derive(Serialize)]
struct ReconcileReport { added: Vec<String>, removed: Vec<String>, updated: Vec<String> }

fn random_name(len: usize) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rng();
    (0..len)
        .map(|_| {
            let idx = rng.random_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

async fn persist_owners(state: &AppState) {
    let owners = state.owners.read().await;
    if let Ok(json) = serde_json::to_vec(&*owners) {
        // atomic write: write to temp then rename
        let tmp = state.metadata_path.with_extension("tmp");
        if let Ok(mut f) = fs::File::create(&tmp).await {
            if let Err(e) = f.write_all(&json).await { eprintln!("persist write_all failed: {e}"); return; }
            if let Err(e) = f.sync_all().await { eprintln!("persist sync failed: {e}"); }
            if let Err(e) = fs::rename(&tmp, &*state.metadata_path).await { eprintln!("persist rename failed: {e}"); }
            if let Ok(md) = fs::metadata(&*state.metadata_path).await { if let Ok(modified) = md.modified() { let mut lm = state.last_meta_mtime.write().await; *lm = modified; } }
        }
    }
}

// New: integrity check to remove metadata entries whose files are missing
async fn check_storage_integrity(state: &AppState) {
    let mut to_remove: Vec<String> = Vec::new();
    {
        let owners = state.owners.read().await;
        for (fname, _meta) in owners.iter() {
            if !state.upload_dir.join(fname).exists() {
                to_remove.push(fname.clone());
            }
        }
    }
    if to_remove.is_empty() { return; }
    {
        let mut owners = state.owners.write().await;
        for f in &to_remove { owners.remove(f); }
    }
    persist_owners(state).await;
}

// Spawn integrity check in background so upload response is not delayed
fn spawn_integrity_check(state: AppState) {
    tokio::spawn(async move { check_storage_integrity(&state).await; });
}

// New: create storage name preserving safe extension (alphanumeric only, <=12 chars)
fn make_storage_name(original: Option<&str>) -> String {
    if let Some(orig) = original {
        let sanitized = sanitize(orig);
        // Treat leading-dot files (e.g. ".bashrc") as extensionless
        if let Some(dot) = sanitized.rfind('.') {
            if dot > 0 { // ensure not a leading dot
                let ext = &sanitized[dot+1..];
                if !ext.is_empty() && ext.len() <= 12 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                    return format!("{}.{}", random_name(RANDOM_NAME_LEN), ext);
                }
            }
        }
    }
    random_name(RANDOM_NAME_LEN)
}

fn ttl_to_duration(code: &str) -> Duration {
    match code {
        "1h" => Duration::from_secs(3600),
        "3h" => Duration::from_secs(3*3600),
        "12h" => Duration::from_secs(12*3600),
        "1d" => Duration::from_secs(24*3600),
        "3d" => Duration::from_secs(3*24*3600),
        "7d" => Duration::from_secs(7*24*3600),
        "14d" => Duration::from_secs(14*24*3600),
        _ => Duration::from_secs(3*24*3600),
    }
}

fn now_secs() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0)).as_secs() }

// Helper: prefix file path with production hostname when applicable
fn qualify_path(state: &AppState, path: &str) -> String {
    if state.production {
        // ensure single slash between host and path
        let p = path.trim_start_matches('/');
        format!("https://{}/{}", PROD_HOST, p)
    } else { path.to_string() }
}

async fn cleanup_expired(state: &AppState) {
    let now = now_secs();
    let mut to_delete: Vec<String> = Vec::new();
    {
        let owners = state.owners.read().await;
        for (file, meta) in owners.iter() {
            if meta.expires <= now { to_delete.push(file.clone()); }
        }
    }
    if to_delete.is_empty() { return; }
    {
        let mut owners = state.owners.write().await;
        for f in &to_delete { owners.remove(f); }
    }
    for f in &to_delete { let _ = fs::remove_file(state.upload_dir.join(f)).await; }
    persist_owners(state).await;
}

// Replace helper to extract real client IP with Cloudflare edge validation
fn ip_in_cidr(ip: IpAddr, cidr: &str) -> bool {
    // cidr like "173.245.48.0/20" or IPv6
    let mut parts = cidr.split('/');
    let base = parts.next().unwrap_or("");
    let prefix: u8 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    if let Ok(base_ip) = base.parse::<IpAddr>() {
        match (ip, base_ip) {
            (IpAddr::V4(a), IpAddr::V4(b)) => {
                let mask = if prefix==0 {0} else {u32::MAX << (32 - prefix) };
                (u32::from(a) & mask) == (u32::from(b) & mask)
            }
            (IpAddr::V6(a), IpAddr::V6(b)) => {
                let a_bytes = a.octets();
                let b_bytes = b.octets();
                let full_bytes = (prefix / 8) as usize;
                if a_bytes[..full_bytes] != b_bytes[..full_bytes] { return false; }
                let rem_bits = prefix % 8;
                if rem_bits == 0 { return true; }
                let mask = 0xFF << (8 - rem_bits);
                (a_bytes[full_bytes] & mask) == (b_bytes[full_bytes] & mask)
            }
            _ => false
        }
    } else { false }
}

fn is_cloudflare_edge(remote: IpAddr) -> bool {
    // Cloudflare published IP ranges (snapshot)
    const CF_CIDRS: &[&str] = &[
        // IPv4
        "173.245.48.0/20","103.21.244.0/22","103.22.200.0/22","103.31.4.0/22","141.101.64.0/18","108.162.192.0/18","190.93.240.0/20","188.114.96.0/20","197.234.240.0/22","198.41.128.0/17","162.158.0.0/15","104.16.0.0/13","104.24.0.0/14","172.64.0.0/13","131.0.72.0/22",
        // IPv6
        "2400:cb00::/32","2606:4700::/32","2803:f800::/32","2405:b500::/32","2405:8100::/32","2a06:98c0::/29","2c0f:f248::/32"
    ];
    CF_CIDRS.iter().any(|c| ip_in_cidr(remote, c))
}

fn real_client_ip(headers: &HeaderMap, fallback: &ClientAddr) -> String {
    let remote = fallback.ip();
    if is_cloudflare_edge(remote) {
        if let Some(cf) = headers.get("cf-connecting-ip").and_then(|v| v.to_str().ok()) {
            if let Ok(parsed) = cf.parse::<IpAddr>() { return parsed.to_string(); }
        }
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = xff.split(',').next() {
                let cand = first.trim();
                if let Ok(parsed) = cand.parse::<IpAddr>() { return parsed.to_string(); }
            }
        }
    }
    remote.to_string()
}

#[axum::debug_handler]
async fn upload_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let client_ip = real_client_ip(&headers, &addr);
    // acquire permit to limit concurrent uploads
    let _permit = match state.upload_sem.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return json_error(StatusCode::SERVICE_UNAVAILABLE, "upload_busy", "too many concurrent uploads"),
    };
    if let Err(_e) = fs::create_dir_all(&*state.upload_dir).await {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "fs_error", "create directory failed");
    }

    let mut saved_files: Vec<String> = Vec::new();
    let mut ttl_choice: Option<String> = None;
    let mut any_aborted = false; // track if any file stream aborted

    while let Ok(Some(field)) = multipart.next_field().await {
        let name_opt = field.name().map(|s| s.to_string());
        if name_opt.as_deref() == Some("ttl") {
            if let Ok(v) = field.text().await { ttl_choice = Some(v); }
            continue;
        }
        let mut field = field; // keep mutable for reading file
        if let Some(filename) = field.file_name() {
            let storage_name = make_storage_name(Some(filename));
            let path = state.upload_dir.join(&storage_name);

            let mut file = match fs::File::create(&path).await {
                Ok(f) => f,
                Err(_) => {
                    return json_error(StatusCode::INTERNAL_SERVER_ERROR, "fs_error", "failed to create file");
                }
            };

            let mut written: u64 = 0;
            let mut aborted = false; // aborted for this file
            loop {
                let next = field.chunk().await;
                match next {
                    Ok(opt) => {
                        if let Some(chunk) = opt {
                            written += chunk.len() as u64;
                            if written > MAX_FILE_BYTES {
                                let _ = fs::remove_file(&path).await;
                                let mut resp = json_error(StatusCode::PAYLOAD_TOO_LARGE, "file_too_large", "file exceeds 500MB limit");
                                resp.headers_mut().insert("X-File-Too-Large", "1".parse().unwrap());
                                return resp;
                            }
                            if let Err(_) = file.write_all(&chunk).await {
                                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "fs_error", "failed writing chunk");
                            }
                        } else { break; } // completed normally
                    }
                    Err(_e) => {
                        // client aborted / stream error
                        aborted = true; any_aborted = true; break;
                    }
                }
            }
            if aborted || written == 0 {
                let _ = fs::remove_file(&path).await; // remove partial file
                continue; // do NOT record metadata or include in response
            }
            let ttl_code = ttl_choice.clone().unwrap_or_else(|| "3d".to_string());
            let expires = now_secs() + ttl_to_duration(&ttl_code).as_secs();
            {
                let mut owners = state.owners.write().await;
                owners.insert(storage_name.clone(), FileMeta { owner: client_ip.clone(), expires });
            }
            persist_owners(&state).await;
            saved_files.push(format!("f/{}", storage_name));
        }
    }

    if saved_files.is_empty() {
        if any_aborted { return json_error(StatusCode::BAD_REQUEST, "upload_aborted", "upload aborted"); }
        return json_error(StatusCode::BAD_REQUEST, "no_files", "no files uploaded");
    }

    // After successful saves and before building final response, trigger integrity check
    if !saved_files.is_empty() { spawn_integrity_check(state.clone()); }

    // Map saved files to qualified URLs in production
    let out_files: Vec<String> = saved_files.iter().map(|f| qualify_path(&state, f)).collect();
    Json(UploadResponse { files: out_files }).into_response()
}

async fn list_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
) -> Response {
    reload_metadata_if_changed(&state).await; // reflect admin edits immediately
    let client_ip = real_client_ip(&headers, &addr);
    let reconcile_report = verify_user_entries_with_report(&state, &client_ip).await; // new: produce difference report
    cleanup_expired(&state).await;
    check_storage_integrity(&state).await; // ensure removed disk files vanish from listing
    let owners = state.owners.read().await;
    let mut files: Vec<(String,u64)> = owners
        .iter()
        .filter_map(|(file, meta)| if meta.owner == client_ip { Some((file.clone(), meta.expires)) } else { None })
        .collect();
    files.sort_by(|a,b| a.0.cmp(&b.0));
    let only_names: Vec<String> = files.iter().map(|(n,_)| qualify_path(&state, &format!("f/{}", n))).collect();
    let metas: Vec<FileMetaEntry> = files.into_iter().map(|(n,e)| FileMetaEntry { file: qualify_path(&state, &format!("f/{}", n)), expires: e }).collect();
    let body = Json(ListResponse { files: only_names, metas, reconcile: reconcile_report });
    let mut resp = body.into_response();
    resp.headers_mut().insert(CACHE_CONTROL, "no-store".parse().unwrap());
    resp
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let production = std::env::var("APP_ENV").map(|v| v.eq_ignore_ascii_case("production")).unwrap_or(false);
    let static_dir = Arc::new(PathBuf::from("./public"));
    let upload_dir = Arc::new(PathBuf::from("./files"));
    let metadata_path = Arc::new(PathBuf::from("./files/file_owners.json"));
    fs::create_dir_all(&*static_dir).await?;
    fs::create_dir_all(&*upload_dir).await?;
    let owners_map: HashMap<String, FileMeta> = match fs::read(&*metadata_path).await {
        Ok(data) => {
            // backward compatibility: old format was HashMap<String,String>
            if let Ok(old_map) = serde_json::from_slice::<HashMap<String,String>>(&data) {
                old_map.into_iter().map(|(k,v)| (k, FileMeta { owner: v, expires: now_secs() + ttl_to_duration("3d").as_secs() })).collect()
            } else {
                serde_json::from_slice(&data).unwrap_or_default()
            }
        },
        Err(_) => HashMap::new(),
    };
    let initial_mtime = match fs::metadata(&*metadata_path).await.and_then(|m| Ok(m.modified().unwrap_or(SystemTime::UNIX_EPOCH))) { Ok(t)=>t, Err(_)=>SystemTime::UNIX_EPOCH };
    let state = AppState { upload_dir, static_dir, metadata_path, owners: Arc::new(RwLock::new(owners_map)), upload_sem: Arc::new(Semaphore::new(UPLOAD_CONCURRENCY)), production, last_meta_mtime: Arc::new(RwLock::new(initial_mtime)) };

    // spawn periodic cleanup
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(600)); // every 10 min
        loop { interval.tick().await; cleanup_expired(&cleanup_state).await; }
    });

    // health & readiness simple endpoints
    async fn health() -> &'static str { "ok" }
    async fn ready(State(state): State<AppState>) -> Response {
        // simple readiness: check we can read lock and metadata path directory exists
        if state.metadata_path.parent().map(|p| p.exists()).unwrap_or(false) { "ready".into_response() } else { json_error(StatusCode::SERVICE_UNAVAILABLE, "not_ready", "storage not ready") }
    }

    // Host enforcement middleware (redirect to canonical host in production)
    async fn enforce_host(req: Request<Body>, next: Next) -> Response {
        if let Some(host) = req.headers().get(axum::http::header::HOST).and_then(|h| h.to_str().ok()) {
            if host != PROD_HOST && host != "localhost:1200" && host != "127.0.0.1:1200" && host != "0.0.0.0:1200" {
                let uri = req.uri();
                let pq = match uri.path_and_query() { Some(pq)=>pq.as_str(), None=>uri.path() };
                let loc = format!("https://{}{}", PROD_HOST, pq);
                return (StatusCode::PERMANENT_REDIRECT, [(axum::http::header::LOCATION, HeaderValue::from_str(&loc).unwrap())]).into_response();
            }
        }
        next.run(req).await
    }

    let base_router = Router::new()
        .route("/", get(root_handler))
        .route("/upload", post(upload_handler))
        .route("/mine", get(list_handler))
        .route("/d/{file}", delete(delete_handler))
        .route("/f/{file}", get(fetch_file_handler))
        .route("/simple", get(simple_root))
        .route("/simple/upload", post(simple_upload))
        .route("/simple/delete", post(simple_delete))
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/{*path}", get(file_handler));

    // Build middleware stack: body limit, rate limit, (future) timeout.
    let limiter_layer = RateLimitLayer { limiter: RateLimiterInner::new(60, 1) };
    let mut app = base_router
        .layer(middleware::from_fn(add_security_headers))
        .layer(limiter_layer)
        .layer(DefaultBodyLimit::max(1024 * 1024 * 512));
    if state.production { app = app.layer(middleware::from_fn(enforce_host)); }
    let app = app.with_state(state);

    let addr: SocketAddr = ([0, 0, 0, 0], 1200).into();
    println!("listening on {addr}");
    axum_server::bind(addr)
        .serve(app.into_make_service_with_connect_info::<ClientAddr>())
        .await?;
    Ok(())
}

async fn fetch_file_handler(
    State(state): State<AppState>,
    Path(file): Path<String>,
) -> Response {
    if file.contains('/') { return (StatusCode::BAD_REQUEST, "bad file").into_response(); }
    cleanup_expired(&state).await;
    let expired = {
        let owners = state.owners.read().await;
        if let Some(meta) = owners.get(&file) {
            meta.expires <= now_secs()
        } else {
            true
        }
    };
    if expired { return (StatusCode::NOT_FOUND, "not found").into_response(); }
    let file_path = state.upload_dir.join(&file);
    if !file_path.exists() {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match fs::File::open(&file_path).await {
        Ok(file) => {
            let stream = ReaderStream::new(file);
            let body = axum::body::Body::from_stream(stream);
            let mime = mime_guess::from_path(&file_path).first_or_octet_stream();
            let mut headers = HeaderMap::new();
            headers.insert(axum::http::header::CONTENT_TYPE, mime.as_ref().parse().unwrap());
            (headers, body).into_response()
        }
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "fs_error", "cant read file"),
    }
}

// New secure delete handler (returns 404 for non-owned files to avoid leakage)
#[axum::debug_handler]
async fn delete_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    Path(file): Path<String>,
) -> Response {
    let ip = real_client_ip(&headers, &addr);
    if file.contains('/') || file.contains("..") || file.contains('\\') {
        return json_error(StatusCode::BAD_REQUEST, "bad_file", "invalid file name");
    }
    cleanup_expired(&state).await;
    {
        let owners = state.owners.read().await;
        match owners.get(&file) {
            Some(meta) if meta.owner == ip => {},
            _ => return (StatusCode::NOT_FOUND, "not found").into_response(),
        }
    }
    {
        let mut owners = state.owners.write().await;
        owners.remove(&file);
    }
    let path = state.upload_dir.join(&file);
    let _ = fs::remove_file(&path).await;
    persist_owners(&state).await;
    (StatusCode::NO_CONTENT, ()).into_response()
}

async fn file_handler(
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> Response {
    if path.contains("..") || path.contains('\\') { return (StatusCode::BAD_REQUEST, "bad path").into_response(); }
    let file_path = state.static_dir.join(&*path); // serve only static assets from public
    if !file_path.exists() {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    match fs::read(&file_path).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&file_path).first_or_octet_stream();
            ([(axum::http::header::CONTENT_TYPE, mime.as_ref())], bytes).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "cant read file").into_response(),
    }
}

// New root handler to directly serve index.html instead of redirect
async fn root_handler(State(state): State<AppState>) -> Response {
    let index_path = state.static_dir.join("index.html");
    if !index_path.exists() { return (StatusCode::NOT_FOUND, "index missing").into_response(); }
    match fs::read(&index_path).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&index_path).first_or_octet_stream();
            ([(axum::http::header::CONTENT_TYPE, mime.as_ref())], bytes).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "cant read index").into_response(),
    }
}

// Security headers middleware
async fn add_security_headers(req: Request<Body>, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    if !h.contains_key("X-Content-Type-Options") { h.insert("X-Content-Type-Options", "nosniff".parse().unwrap()); }
    if !h.contains_key("X-Frame-Options") { h.insert("X-Frame-Options", "DENY".parse().unwrap()); }
    if !h.contains_key("Referrer-Policy") { h.insert("Referrer-Policy", "no-referrer".parse().unwrap()); }
    if (!h.contains_key("Content-Security-Policy")) { h.insert("Content-Security-Policy", "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:".parse().unwrap()); }
    if !h.contains_key("Cache-Control") { h.insert("Cache-Control", "private, max-age=0, no-store".parse().unwrap()); }
    resp
}

#[derive(Deserialize)]
struct SimpleQuery { m: Option<String> }

#[derive(Deserialize)]
struct DeleteQuery { f: String }

fn simple_page(message: Option<&str>, files: &[(String,u64)], now: u64) -> String {
    let mut rows = String::new();
    for (name, exp) in files.iter() {
        let remain = if *exp <= now { Cow::Borrowed("expired") } else {
            let mut secs = *exp as i64 - now as i64;
            let d = secs / 86400; secs %= 86400;
            let h = secs / 3600; secs %= 3600;
            let m = secs / 60; secs %= 60;
            let mut parts = Vec::new();
            if d>0 { parts.push(format!("{}d", d)); }
            if h>0 { parts.push(format!("{}h", h)); }
            if m>0 && parts.len()<2 { parts.push(format!("{}m", m)); }
            if parts.is_empty() { parts.push(format!("{}s", secs)); }
            Cow::Owned(parts.join(" "))
        };
        rows.push_str(&format!("<tr><td style='font-family:monospace'><a href='/f/{0}'>{0}</a></td><td>{1}</td><td><form method='post' action='/simple/delete' style='display:inline'><input type='hidden' name='f' value='{0}'/><button style='background:#942;padding:.25rem .6rem;border:1px solid #b54;color:#fff;border-radius:4px;cursor:pointer;font-size:.65rem'>del</button></form></td></tr>", name, remain));
    }
    let msg_html = message.map(|m| format!("<div style='background:#223038;border:1px solid #33464f;padding:.5rem .7rem;margin:0 0 .8rem;border-radius:6px;font-size:.7rem'>{}</div>", htmlescape::encode_minimal(m))).unwrap_or_default();
    format!("<!DOCTYPE html><html lang='en'><head><meta charset='utf-8'/><title>JuiceBox – Simple</title><meta name='viewport' content='width=device-width,initial-scale=1'/><style>body{{background:#0f141b;color:#e8ecf3;font-family:system-ui,Segoe UI,Roboto,sans-serif;margin:0;padding:1.2rem;max-width:880px}}h1{{margin:.2rem 0 .8rem;font-size:1.6rem}}form.upload{{background:#1a2230;padding:1rem 1.1rem 1.2rem;border:1px solid #273242;border-radius:12px;margin:0 0 1.1rem}}fieldset{{border:none;padding:0;margin:0}}label{{font-size:.7rem;letter-spacing:.5px;display:block;margin:0 0 .4rem;opacity:.75}}input[type=file]{{display:block;margin:.4rem 0 .8rem}}select,button,input[type=file]{{font-size:.75rem}}table{{width:100%;border-collapse:collapse;font-size:.65rem}}th,td{{padding:.45rem .5rem;border-bottom:1px solid #273242;text-align:left}}th{{font-weight:600;letter-spacing:.5px;font-size:.6rem;text-transform:uppercase;opacity:.8}}tr:hover td{{background:#1f2935}}.note{{font-size:.58rem;opacity:.55;line-height:1.4;margin-top:.8rem}}.ttl-box{{display:flex;align-items:center;gap:.5rem;margin:.4rem 0 .9rem}}.ttl-box select{{background:#121b24;color:#e8ecf3;border:1px solid #2b394a;padding:.35rem .5rem;border-radius:6px}}button.primary{{background:#ff9800;color:#111;border:1px solid #ffa733;padding:.5rem 1rem;border-radius:8px;cursor:pointer;font-weight:600;letter-spacing:.5px}}button.primary:hover{{filter:brightness(1.1)}}.files-panel{{background:#1a2230;padding:1rem 1.1rem 1.25rem;border:1px solid #273242;border-radius:12px}}</style></head><body><h1>JuiceBox – No&nbsp;Script</h1><p style='margin:0 0 1.1rem;font-size:.8rem;opacity:.75'>Basic uploader for old browsers and disabled JavaScript.</p>{msg_html}<form class='upload' method='post' enctype='multipart/form-data' action='/simple/upload'><fieldset><label>Files</label><input type='file' name='file' multiple required/><div class='ttl-box'><label for='ttl' style='margin:0'>Retention:</label><select name='ttl' id='ttl'><option>1h</option><option>3h</option><option>12h</option><option>1d</option><option selected>3d</option><option>7d</option><option>14d</option></select><span style='font-size:.6rem;opacity:.55'>auto delete</span></div><button class='primary' type='submit'>Upload</button></fieldset></form><div class='files-panel'><h2 style='margin:.1rem 0  .6rem;font-size:.9rem;letter-spacing:.5px;opacity:.8;text-transform:uppercase'>Your Files</h2><table><thead><tr><th>Name</th><th>Expires In</th><th>Delete</th></tr></thead><tbody>{rows}</tbody></table><p class='note'>Files are linked to your IP. They expire automatically. Keep page for reference or bookmark links.</p></div><p class='note'>Return to <a href='/' style='color:#ff9800'>JS interface</a>.</p></body></html>")
}

async fn simple_root(State(state): State<AppState>, ConnectInfo(addr): ConnectInfo<ClientAddr>, headers: HeaderMap, Query(q): Query<SimpleQuery>) -> Response {
    reload_metadata_if_changed(&state).await; // ensure admin changes visible here too
    let ip = real_client_ip(&headers, &addr);
    verify_user_entries(&state, &ip).await; // NEW: reconcile
    cleanup_expired(&state).await;
    check_storage_integrity(&state).await;
    // Restrict listing to files owned by this client's IP only (privacy)
    let now = now_secs();
    let owners = state.owners.read().await;
    let mut my_files: Vec<(String,u64)> = owners.iter()
        .filter(|(_, meta)| meta.owner == ip)
        .map(|(name, meta)| (name.clone(), meta.expires))
        .collect();
    my_files.sort_by(|a,b| a.0.cmp(&b.0));
    drop(owners);
    let html = simple_page(q.m.as_deref(), &my_files, now);
    (StatusCode::OK,
        [
            (CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"))
        ],
        html
    ).into_response()
}

#[axum::debug_handler]
#[allow(non_snake_case)]
async fn simple_upload(State(state): State<AppState>, ConnectInfo(addr): ConnectInfo<ClientAddr>, headers: HeaderMap, mut multipart: Multipart) -> Response {
    let ip = real_client_ip(&headers, &addr);
    let mut saved: Vec<String> = Vec::new();
    let mut ttl_choice: Option<String> = None;
    while let Ok(Some(field)) = multipart.next_field().await {
        if let Some(name) = field.name() { if name == "ttl" { if let Ok(v) = field.text().await { ttl_choice = Some(v); } continue; } }
        let mut field = field;
        if let Some(orig_filename) = field.file_name() {
            let new_name = make_storage_name(Some(orig_filename));
            let path = state.upload_dir.join(&new_name);
            let mut file = match fs::File::create(&path).await { Ok(f)=>f, Err(_)=> return json_error(StatusCode::INTERNAL_SERVER_ERROR, "fs_error", "failed create") };
            let mut written: u64 = 0;
            let mut aborted = false;
            loop {
                let next = field.chunk().await;
                match next {
                    Ok(opt) => {
                        if let Some(chunk) = opt {
                            written += chunk.len() as u64;
                            if written > MAX_FILE_BYTES {
                                let _=fs::remove_file(&path).await;
                                let msg = urlencoding::encode("File exceeds 500MB limit");
                                let loc = format!("/simple?m={}", msg);
                                let hv = HeaderValue::from_str(&loc).unwrap();
                                return (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, hv)]).into_response();
                            }
                            if file.write_all(&chunk).await.is_err(){ aborted = true; break; }
                        } else { break; }
                    }
                    Err(_) => { aborted = true; let _=fs::remove_file(&path).await; break; }
                }
            }
            if aborted || written == 0 { let _ = fs::remove_file(&path).await; continue; }
            let ttl_code = ttl_choice.clone().unwrap_or_else(||"3d".into());
            let expires = now_secs() + ttl_to_duration(&ttl_code).as_secs();
            { let mut owners = state.owners.write().await; owners.insert(new_name.clone(), FileMeta { owner: ip.clone(), expires }); }
            persist_owners(&state).await;
            saved.push(new_name);
        }
    }
    if saved.is_empty() { return (StatusCode::BAD_REQUEST, "no files").into_response(); }
    // Kick off background integrity verification after upload(s)
    spawn_integrity_check(state.clone());
    let msg = format!("Uploaded {} file(s)", saved.len());
    let redirect = if state.production { format!("https://{}/simple?m={}", PROD_HOST, urlencoding::encode(&msg)) } else { format!("/simple?m={}", urlencoding::encode(&msg)) };
    let hv = HeaderValue::from_str(&redirect).unwrap();
    (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, hv)]).into_response()
}

#[derive(Deserialize)]
struct SimpleDeleteForm { f: String }

#[axum::debug_handler]
async fn simple_delete(State(state): State<AppState>, ConnectInfo(addr): ConnectInfo<ClientAddr>, headers: HeaderMap, Form(frm): Form<SimpleDeleteForm>) -> Response {
    let ip = real_client_ip(&headers, &addr);
    let target = frm.f;
    let owned = { let owners = state.owners.read().await; owners.get(&target).map(|m| m.owner.clone()) };
    if owned.is_some() && owned.unwrap()==ip { let _={ let mut owners = state.owners.write().await; owners.remove(&target); }; let _=fs::remove_file(state.upload_dir.join(&target)).await; persist_owners(&state).await; let hv = HeaderValue::from_static("/simple?m=Deleted"); return (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, hv)]).into_response(); }
    let hv = HeaderValue::from_static("/simple?m=Not+found");
    (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, hv)]).into_response()
}

// New: reload metadata from disk if file_owners.json modified externally (e.g. admin changes)
async fn reload_metadata_if_changed(state: &AppState) {
    let meta_res = fs::metadata(&*state.metadata_path).await;
    let md = match meta_res { Ok(m)=>m, Err(_)=>return };
    let modified = match md.modified() { Ok(t)=>t, Err(_)=>return };
    let need_reload = {
        let lm = state.last_meta_mtime.read().await;
        modified > *lm
    };
    if !need_reload { return; }
    if let Ok(bytes) = fs::read(&*state.metadata_path).await {
        // Try HashMap<String, FileMeta>, fallback to legacy
        if let Ok(map) = serde_json::from_slice::<HashMap<String, FileMeta>>(&bytes) {
            let mut owners = state.owners.write().await;
            owners.clear();
            owners.extend(map.into_iter());
        } else if let Ok(old) = serde_json::from_slice::<HashMap<String,String>>(&bytes) {
            let mut owners = state.owners.write().await;
            owners.clear();
            let default_exp = now_secs() + ttl_to_duration("3d").as_secs();
            owners.extend(old.into_iter().map(|(k,v)|(k, FileMeta{ owner:v, expires: default_exp })));
        }
        let mut lm = state.last_meta_mtime.write().await; *lm = modified;
    }
}

// NEW: verify and reconcile a specific user's entries with on-disk file_owners.json (detect admin tampering)
async fn verify_user_entries(state: &AppState, ip: &str) {
    if let Ok(bytes) = fs::read(&*state.metadata_path).await {
        if let Ok(disk_map) = serde_json::from_slice::<HashMap<String, FileMeta>>(&bytes) {
            // Collect differences only for this user's files (or files that moved away from this user)
            let mut to_remove: Vec<String> = Vec::new();
            let mut to_update: Vec<(String, FileMeta)> = Vec::new();
            let mut to_add: Vec<(String, FileMeta)> = Vec::new();
            {
                let owners = state.owners.read().await;
                // Mark removals / changes
                for (fname, meta_mem) in owners.iter() {
                    if meta_mem.owner == ip {
                        match disk_map.get(fname) {
                            Some(meta_disk) => {
                                if meta_disk.owner != meta_mem.owner || meta_disk.expires != meta_mem.expires { to_update.push((fname.clone(), meta_disk.clone())); }
                            }
                            None => to_remove.push(fname.clone()), // deleted by admin
                        }
                    }
                }
                // Additions: any disk entries for this ip not in memory
                for (fname, meta_disk) in disk_map.iter() {
                    if meta_disk.owner == ip && !owners.contains_key(fname) { to_add.push((fname.clone(), meta_disk.clone())); }
                }
            }
            if !(to_remove.is_empty() && to_update.is_empty() && to_add.is_empty()) {
                let mut owners = state.owners.write().await;
                for f in to_remove { owners.remove(&f); }
                for (f,m) in to_update { owners.insert(f,m); }
                for (f,m) in to_add { owners.insert(f,m); }
            }
        }
    }
}

async fn verify_user_entries_with_report(state: &AppState, ip: &str) -> Option<ReconcileReport> {
    if let Ok(bytes) = fs::read(&*state.metadata_path).await {
        if let Ok(disk_map) = serde_json::from_slice::<HashMap<String, FileMeta>>(&bytes) {
            let mut to_remove: Vec<String> = Vec::new();
            let mut to_update: Vec<(String, FileMeta)> = Vec::new();
            let mut to_add: Vec<(String, FileMeta)> = Vec::new();
            {
                let owners = state.owners.read().await;
                for (fname, meta_mem) in owners.iter() {
                    if meta_mem.owner == ip {
                        match disk_map.get(fname) {
                            Some(meta_disk) => {
                                if meta_disk.owner != meta_mem.owner || meta_disk.expires != meta_mem.expires { to_update.push((fname.clone(), meta_disk.clone())); }
                            }
                            None => to_remove.push(fname.clone()),
                        }
                    }
                }
                for (fname, meta_disk) in disk_map.iter() {
                    if meta_disk.owner == ip && !owners.contains_key(fname) { to_add.push((fname.clone(), meta_disk.clone())); }
                }
            }
            if to_remove.is_empty() && to_update.is_empty() && to_add.is_empty() { return None; }
            {
                let mut owners = state.owners.write().await;
                for f in &to_remove { owners.remove(f); }
                for (f,m) in &to_update { owners.insert(f.clone(), m.clone()); }
                for (f,m) in &to_add { owners.insert(f.clone(), m.clone()); }
            }
            return Some(ReconcileReport { added: to_add.into_iter().map(|(f,_)| f).collect(), removed: to_remove, updated: to_update.into_iter().map(|(f,_)| f).collect() });
        }
    }
    None
}
