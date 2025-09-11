use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response, Redirect},
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
use tokio::sync::RwLock;
use axum::http::header::CACHE_CONTROL;
use std::time::{SystemTime, Duration, UNIX_EPOCH};

#[derive(Clone)]
struct AppState {
    upload_dir: Arc<PathBuf>,          // ./files (binary storage)
    static_dir: Arc<PathBuf>,          // ./public (static assets)
    metadata_path: Arc<PathBuf>,       // ./files/file_owners.json
    owners: Arc<RwLock<HashMap<String, FileMeta>>>, // filename -> meta
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
}

#[derive(Serialize)]
struct FileMetaEntry {
    file: String,
    expires: u64,
}

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
        if let Err(e) = fs::write(&*state.metadata_path, json).await {
            eprintln!("failed to persist owners: {e}");
        }
    }
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

#[axum::debug_handler]
async fn upload_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    mut multipart: Multipart,
) -> Response {
    if let Err(e) = fs::create_dir_all(&*state.upload_dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create base folder: {e}"),
        )
            .into_response();
    }

    let mut saved_files: Vec<String> = Vec::new();
    let mut ttl_choice: Option<String> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let name_opt = field.name().map(|s| s.to_string());
        if name_opt.as_deref() == Some("ttl") {
            if let Ok(v) = field.text().await { ttl_choice = Some(v); }
            continue;
        }
        let mut field = field; // keep mutable for reading file
        if let Some(filename) = field.file_name() {
            let original = sanitize(filename);
            let rand_part = random_name(12);
            let new_name = if let Some(ext) = std::path::Path::new(&original).extension().and_then(|s| s.to_str()) {
                format!("{}.{}", rand_part, ext)
            } else {
                rand_part
            };
            let path = state.upload_dir.join(&new_name);

            let mut file = match fs::File::create(&path).await {
                Ok(f) => f,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("failed to create file: {e}"),
                    )
                        .into_response();
                }
            };

            while let Some(chunk) = field.chunk().await.unwrap_or(None) {
                if let Err(e) = file.write_all(&chunk).await {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("failed writing chunk: {e}"),
                    )
                        .into_response();
                }
            }
            let ttl_code = ttl_choice.clone().unwrap_or_else(|| "3d".to_string());
            let expires = now_secs() + ttl_to_duration(&ttl_code).as_secs();
            {
                let mut owners = state.owners.write().await;
                owners.insert(new_name.clone(), FileMeta { owner: addr.ip().to_string(), expires });
            }
            persist_owners(&state).await;
            saved_files.push(format!("f/{}", new_name));
        }
    }

    if saved_files.is_empty() {
        return (StatusCode::BAD_REQUEST, "no files uploaded").into_response();
    }

    Json(UploadResponse { files: saved_files }).into_response()
}

async fn list_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
) -> Response {
    let ip = addr.ip().to_string();
    cleanup_expired(&state).await;
    let owners = state.owners.read().await;
    let mut files: Vec<(String,u64)> = owners
        .iter()
        .filter_map(|(file, meta)| if meta.owner == ip { Some((file.clone(), meta.expires)) } else { None })
        .collect();
    files.sort_by(|a,b| a.0.cmp(&b.0));
    let only_names: Vec<String> = files.iter().map(|(n,_)| format!("f/{}", n)).collect();
    let metas: Vec<FileMetaEntry> = files.into_iter().map(|(n,e)| FileMetaEntry { file: format!("f/{}", n), expires: e }).collect();
    let body = Json(ListResponse { files: only_names, metas });
    let mut resp = body.into_response();
    resp.headers_mut().insert(CACHE_CONTROL, "no-store".parse().unwrap());
    resp
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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

    let state = AppState { upload_dir, static_dir, metadata_path, owners: Arc::new(RwLock::new(owners_map)) };

    // spawn periodic cleanup
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(600)); // every 10 min
        loop { interval.tick().await; cleanup_expired(&cleanup_state).await; }
    });

    let app = Router::new()
        .route("/", get(|| async { Redirect::to("/index.html") }))
        .route("/upload", post(upload_handler))
        .route("/mine", get(list_handler))
        .route("/d/{file}", delete(delete_handler))
        .route("/f/{file}", get(fetch_file_handler))
        .route("/{*path}", get(file_handler))
        .layer(DefaultBodyLimit::max(1024 * 1024 * 512))
        .with_state(state);

    let addr: SocketAddr = ([0, 0, 0, 0], 3000).into();
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
    match fs::read(&file_path).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&file_path).first_or_octet_stream();
            ([(axum::http::header::CONTENT_TYPE, mime.as_ref())], bytes).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "cant read file").into_response(),
    }
}

async fn file_handler(
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> Response {
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

async fn delete_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    Path(file): Path<String>,
) -> Response {
    if file.contains('/') {
        return (StatusCode::BAD_REQUEST, "bad file").into_response();
    }
    cleanup_expired(&state).await;
    {
        let owners = state.owners.read().await;
        match owners.get(&file) {
            Some(meta) if meta.owner == addr.ip().to_string() && meta.expires > now_secs() => {},
            _ => return (StatusCode::FORBIDDEN, "forbidden").into_response(),
        }
    }
    let file_path = state.upload_dir.join(&file);
    match fs::remove_file(&file_path).await {
        Ok(_) => {
            {
                // acquire write lock and remove, then drop before persisting to avoid deadlock
                let mut owners = state.owners.write().await;
                owners.remove(&file);
            }
            // write JSON after releasing write lock
            persist_owners(&state).await;
            StatusCode::OK.into_response()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "delete failed").into_response(),
    }
}
