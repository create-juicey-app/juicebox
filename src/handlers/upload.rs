use axum::Json;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Multipart, Path, Query as AxumQuery, State};
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use infer;
use mime_guess::mime;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::net::SocketAddr as ClientAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, RwLock};

use crate::state::{
    AppState, ChunkSession, FileMeta, ReconcileReport, check_storage_integrity, cleanup_expired,
    spawn_integrity_check, verify_user_entries_with_report,
};
use crate::util::{
    FORBIDDEN_EXTENSIONS, is_forbidden_extension, json_error, make_storage_name, max_file_bytes,
    new_id, now_secs, qualify_path, real_client_ip, ttl_to_duration,
};

#[derive(Deserialize)]
pub struct CheckHashQuery {
    pub hash: String,
}

#[derive(Serialize, Deserialize)]
pub struct UploadResponse {
    pub files: Vec<String>,
    pub truncated: bool,
    pub remaining: usize,
}

#[derive(Serialize)]
pub struct ListResponse {
    pub files: Vec<String>,
    pub metas: Vec<FileMetaEntry>,
    pub reconcile: Option<ReconcileReport>,
}

#[derive(Serialize)]
pub struct FileMetaEntry {
    pub file: String,
    pub expires: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub original: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<u64>,
}

const DEFAULT_CHUNK_SIZE: u64 = 8 * 1024 * 1024; // 8 MiB
const MIN_CHUNK_SIZE: u64 = 64 * 1024; // 64 KiB
const MAX_CHUNK_SIZE: u64 = 32 * 1024 * 1024; // 32 MiB
const MAX_TOTAL_CHUNKS: u64 = 20_000;

#[derive(Serialize, Deserialize)]
pub struct ChunkInitRequest {
    pub filename: String,
    pub size: u64,
    pub ttl: Option<String>,
    pub chunk_size: Option<u64>,
    pub hash: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ChunkInitResponse {
    pub session_id: String,
    pub chunk_size: u64,
    pub total_chunks: u32,
    pub expires: u64,
    pub storage_name: String,
}

#[derive(Serialize, Deserialize)]
pub struct ChunkCompleteRequest {
    pub hash: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChunkPathParams {
    id: String,
    index: u32,
}

#[derive(Debug, Deserialize)]
pub struct ChunkCompletePath {
    id: String,
}

fn compute_chunk_layout(total_size: u64, requested: Option<u64>) -> Option<(u64, u32)> {
    if total_size == 0 {
        return None;
    }
    let mut chunk = requested.unwrap_or(DEFAULT_CHUNK_SIZE);
    chunk = chunk.clamp(MIN_CHUNK_SIZE, MAX_CHUNK_SIZE);
    if chunk > total_size {
        chunk = total_size;
    }
    if chunk == 0 {
        chunk = total_size;
    }
    let mut total_chunks = ((total_size + chunk - 1) / chunk) as u64;
    if total_chunks == 0 {
        return None;
    }
    if total_chunks > MAX_TOTAL_CHUNKS {
        chunk = ((total_size + MAX_TOTAL_CHUNKS - 1) / MAX_TOTAL_CHUNKS).max(1);
        chunk = chunk.clamp(MIN_CHUNK_SIZE, MAX_CHUNK_SIZE);
        if chunk > total_size {
            chunk = total_size;
        }
        total_chunks = ((total_size + chunk - 1) / chunk) as u64;
        if total_chunks == 0 || total_chunks > MAX_TOTAL_CHUNKS {
            return None;
        }
    }
    Some((chunk, total_chunks as u32))
}

fn expected_chunk_len(session: &ChunkSession, index: u32) -> u64 {
    if session.total_chunks == 0 {
        return 0;
    }
    if index + 1 == session.total_chunks {
        let full = session.chunk_size * (session.total_chunks as u64 - 1);
        session.total_bytes.saturating_sub(full)
    } else {
        session.chunk_size
    }
}

fn find_duplicate_by_hash(state: &AppState, hash: &str) -> Option<(String, FileMeta)> {
    state
        .owners
        .iter()
        .find(|entry| entry.value().hash == hash)
        .map(|entry| (entry.key().clone(), entry.value().clone()))
}

#[axum::debug_handler]
pub async fn init_chunk_upload_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    Json(req): Json<ChunkInitRequest>,
) -> Response {
    if state.is_banned(&real_client_ip(&headers, &addr)).await {
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let ip = real_client_ip(&headers, &addr);
    if req.size == 0 {
        return json_error(StatusCode::BAD_REQUEST, "empty", "file size required");
    }
    if req.size > max_file_bytes() {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "file exceeds configured max size",
        );
    }
    let now = now_secs();
    let ttl_code = req.ttl.clone().unwrap_or_else(|| "24h".to_string());
    let ttl = ttl_to_duration(&ttl_code).as_secs();
    let expires = now + ttl;

    let (chunk_size, total_chunks) = match compute_chunk_layout(req.size, req.chunk_size) {
        Some(layout) => layout,
        None => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "chunk_layout",
                "unable to compute chunk layout",
            );
        }
    };

    if let Some(hash) = req.hash.as_ref() {
        if let Some((file, meta)) = find_duplicate_by_hash(&state, hash) {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "duplicate": true,
                    "file": file,
                    "meta": meta,
                })),
            )
                .into_response();
        }
    }

    let session_id = new_id();
    let storage_name = make_storage_name(Some(&req.filename));
    let storage_dir_path = state.chunk_dir.join(&session_id);
    if let Err(err) = fs::create_dir_all(&storage_dir_path).await {
        tracing::error!(?err, "Failed to create chunk directory");
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "chunk_dir",
            "failed to initialize chunk upload",
        );
    }

    let session = Arc::new(ChunkSession {
        owner: ip,
        original_name: req.filename.clone(),
        storage_name: storage_name.clone(),
        ttl_code: ttl_code.clone(),
        expires,
        total_bytes: req.size,
        chunk_size,
        total_chunks,
        hash: req.hash.clone(),
        storage_dir: Arc::new(storage_dir_path),
        created: now,
        received: RwLock::new(vec![false; total_chunks as usize]),
        completed: AtomicBool::new(false),
        last_update: AtomicU64::new(now),
        persist_lock: Mutex::new(()),
    });
    state
        .chunk_sessions
        .insert(session_id.clone(), session.clone());
    if let Err(err) = state
        .persist_chunk_session(&session_id, session.as_ref())
        .await
    {
        tracing::error!(?err, session_id = %session_id, "failed to persist chunk session metadata");
        state.remove_chunk_session(&session_id).await;
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "chunk_dir",
            "failed to initialize chunk upload",
        );
    }

    Json(ChunkInitResponse {
        session_id,
        chunk_size,
        total_chunks,
        expires,
        storage_name: storage_name.clone(),
    })
    .into_response()
}

#[axum::debug_handler]
pub async fn upload_chunk_part_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    Path(params): Path<ChunkPathParams>,
    body: Bytes,
) -> Response {
    if state.is_banned(&real_client_ip(&headers, &addr)).await {
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let ip = real_client_ip(&headers, &addr);
    let Some(session_ref) = state.chunk_sessions.get(&params.id) else {
        return json_error(
            StatusCode::NOT_FOUND,
            "chunk_session",
            "upload session not found",
        );
    };
    let session = session_ref.value().clone();
    drop(session_ref);
    session.touch();
    if session.owner != ip {
        return json_error(
            StatusCode::FORBIDDEN,
            "not_owner",
            "upload session not owned by ip",
        );
    }
    if session.is_completed() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "completed",
            "upload session already completed",
        );
    }
    if params.index >= session.total_chunks {
        return json_error(
            StatusCode::BAD_REQUEST,
            "chunk_index",
            "invalid chunk index",
        );
    }
    let expected = expected_chunk_len(&session, params.index);
    if expected == 0 {
        return json_error(StatusCode::BAD_REQUEST, "chunk_size", "invalid chunk size");
    }
    if body.is_empty() || body.len() as u64 != expected {
        return json_error(
            StatusCode::BAD_REQUEST,
            "chunk_size",
            "chunk length mismatch",
        );
    }
    let chunk_path = session
        .storage_dir
        .join(format!("{:06}.chunk", params.index));
    if let Err(err) = fs::write(&chunk_path, &body).await {
        tracing::error!(?err, ?chunk_path, "failed to persist chunk");
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "chunk_write",
            "failed to write chunk",
        );
    }
    {
        let mut received = session.received.write().await;
        if let Some(entry) = received.get_mut(params.index as usize) {
            *entry = true;
        }
    }
    if let Err(err) = state
        .persist_chunk_session(&params.id, session.as_ref())
        .await
    {
        tracing::warn!(?err, session_id = %params.id, "failed to persist chunk session after part");
    }
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(axum::body::Body::empty())
        .unwrap()
}

#[axum::debug_handler]
pub async fn complete_chunk_upload_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    Path(path): Path<ChunkCompletePath>,
    Json(req): Json<ChunkCompleteRequest>,
) -> Response {
    if state.is_banned(&real_client_ip(&headers, &addr)).await {
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let ip = real_client_ip(&headers, &addr);
    let Some(session_entry) = state.chunk_sessions.get(&path.id) else {
        return json_error(
            StatusCode::NOT_FOUND,
            "chunk_session",
            "upload session not found",
        );
    };
    let session = session_entry.value().clone();
    drop(session_entry);
    if session.owner != ip {
        return json_error(
            StatusCode::FORBIDDEN,
            "not_owner",
            "upload session not owned by ip",
        );
    }
    if session.is_completed() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "completed",
            "upload session already completed",
        );
    }
    {
        let received = session.received.read().await;
        if received.iter().any(|r| !*r) {
            return json_error(
                StatusCode::BAD_REQUEST,
                "incomplete",
                "not all chunks uploaded",
            );
        }
    }
    let ttl = ttl_to_duration(&session.ttl_code).as_secs();
    let expires = session.created + ttl;
    let permit = match state.upload_sem.clone().acquire_owned().await {
        Ok(p) => p,
        Err(_) => {
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "upload_capacity",
                "server busy handling uploads",
            );
        }
    };
    let storage_name = session.storage_name.clone();
    let final_path = state.upload_dir.join(&storage_name);
    let mut tmp_path = final_path.clone();
    tmp_path.set_extension("part");
    let mut file = match fs::File::create(&tmp_path).await {
        Ok(f) => f,
        Err(err) => {
            drop(permit);
            tracing::error!(?err, ?tmp_path, "failed to create assembled file");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "final_create",
                "failed to assemble upload",
            );
        }
    };
    let mut hasher = Sha256::new();
    for idx in 0..session.total_chunks {
        let chunk_path = session.storage_dir.join(format!("{:06}.chunk", idx));
        let mut chunk_file = match fs::File::open(&chunk_path).await {
            Ok(f) => f,
            Err(err) => {
                drop(permit);
                let _ = fs::remove_file(&tmp_path).await;
                tracing::error!(?err, ?chunk_path, "missing chunk during assembly");
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "chunk_missing",
                    "missing chunk during assembly",
                );
            }
        };
        let mut buf = vec![0u8; 8192];
        loop {
            match chunk_file.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if file.write_all(&buf[..n]).await.is_err() {
                        drop(permit);
                        let _ = fs::remove_file(&tmp_path).await;
                        return json_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "write",
                            "failed writing assembled file",
                        );
                    }
                    hasher.update(&buf[..n]);
                }
                Err(err) => {
                    drop(permit);
                    let _ = fs::remove_file(&tmp_path).await;
                    tracing::error!(?err, ?chunk_path, "failed reading chunk");
                    return json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "chunk_read",
                        "failed reading chunk data",
                    );
                }
            }
        }
    }
    if file.flush().await.is_err() {
        drop(permit);
        let _ = fs::remove_file(&tmp_path).await;
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "flush",
            "failed finalizing upload",
        );
    }
    if fs::rename(&tmp_path, &final_path).await.is_err() {
        drop(permit);
        let _ = fs::remove_file(&tmp_path).await;
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "move",
            "failed finalizing upload",
        );
    }
    drop(permit);

    let digest = format!("{:x}", hasher.finalize());
    let expected_hash = req
        .hash
        .as_ref()
        .or_else(|| session.hash.as_ref())
        .map(|s| s.as_str());
    if let Some(exp) = expected_hash {
        if exp != digest {
            let _ = fs::remove_file(&final_path).await;
            state.remove_chunk_session(&path.id).await;
            return json_error(
                StatusCode::BAD_REQUEST,
                "hash_mismatch",
                "uploaded file hash mismatch",
            );
        }
    }
    if let Some((existing, meta)) = find_duplicate_by_hash(&state, &digest) {
        let _ = fs::remove_file(&final_path).await;
        state.remove_chunk_session(&path.id).await;
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "duplicate": true,
                "file": existing,
                "meta": meta,
            })),
        )
            .into_response();
    }

    let meta = FileMeta {
        owner: session.owner.clone(),
        expires,
        original: session.original_name.clone(),
        created: now_secs(),
        hash: digest.clone(),
    };
    session.mark_completed();
    if let Err(err) = state
        .persist_chunk_session(&path.id, session.as_ref())
        .await
    {
        tracing::warn!(?err, session_id = %path.id, "failed to persist completed chunk session before cleanup");
    }
    state.owners.insert(storage_name.clone(), meta);
    state.persist_owners().await;
    state.remove_chunk_session(&path.id).await;
    spawn_integrity_check(state.clone());

    Json(UploadResponse {
        files: vec![storage_name],
        truncated: false,
        remaining: 0,
    })
    .into_response()
}

#[axum::debug_handler]
pub async fn cancel_chunk_upload_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    Path(path): Path<ChunkCompletePath>,
) -> Response {
    if state.is_banned(&real_client_ip(&headers, &addr)).await {
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let ip = real_client_ip(&headers, &addr);
    let Some(entry) = state.chunk_sessions.get(&path.id) else {
        return json_error(
            StatusCode::NOT_FOUND,
            "chunk_session",
            "upload session not found",
        );
    };
    if entry.value().owner != ip {
        return json_error(
            StatusCode::FORBIDDEN,
            "not_owner",
            "upload session not owned by ip",
        );
    }
    drop(entry);
    state.remove_chunk_session(&path.id).await;
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(axum::body::Body::empty())
        .unwrap()
}

#[axum::debug_handler]
pub async fn checkhash_handler(
    State(state): State<AppState>,
    AxumQuery(query): AxumQuery<CheckHashQuery>,
) -> Response {
    let exists = state
        .owners
        .iter()
        .any(|entry| entry.value().hash == query.hash);
    Json(json!({ "exists": exists })).into_response()
}

#[axum::debug_handler]
pub async fn upload_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    if state.is_banned(&real_client_ip(&headers, &addr)).await {
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let ip = real_client_ip(&headers, &addr);
    let sem = state.upload_sem.clone();
    let _permit = match sem.try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "busy",
                "server is busy, try again later",
            );
        }
    };

    let mut ttl_code = "24h".to_string();
    let mut files_to_process = Vec::new();
    let mut pending_files = Vec::new();
    let mut forbidden_error: Option<String> = None;

    while let Some(field) = multipart.next_field().await.unwrap() {
        let name = if let Some(name) = field.name() {
            name.to_string()
        } else {
            continue;
        };
        if name == "ttl" {
            if let Ok(data) = field.bytes().await {
                if let Ok(s) = std::str::from_utf8(&data) {
                    ttl_code = s.to_string();
                }
            }
            continue;
        }
        if name.starts_with("file") {
            let original_name = field.file_name().map(|s| s.to_string());
            let content_type = field.content_type().map(|m| m.to_string());
            if let Ok(data) = field.bytes().await {
                if !data.is_empty() {
                    pending_files.push((original_name, content_type, data));
                }
            }
        }
    }

    let mut has_forbidden = false;
    for (original_name, _content_type, data) in &pending_files {
        if let Some(orig) = original_name {
            if is_forbidden_extension(orig) {
                tracing::warn!(?original_name, "Upload rejected: forbidden file extension");
                forbidden_error = Some("File type not allowed (forbidden extension)".to_string());
                has_forbidden = true;
                break;
            }
        }
        let is_forbidden_content = if let Some(kind) = infer::get(&data) {
            let ext = kind.extension();
            FORBIDDEN_EXTENSIONS.contains(&ext)
        } else {
            false
        };
        if is_forbidden_content {
            tracing::warn!(
                ?original_name,
                "Upload rejected: forbidden file content detected by infer"
            );
            forbidden_error = Some("File type not allowed (forbidden content)".to_string());
            has_forbidden = true;
            break;
        }
    }

    for (original_name, _content_type, data) in pending_files {
        files_to_process.push((original_name, data));
    }

    if has_forbidden {
        let msg = forbidden_error.unwrap_or_else(|| "File type not allowed".to_string());
        return json_error(
            StatusCode::BAD_REQUEST,
            "bad_filetype",
            match msg.as_str() {
                "File type not allowed (forbidden content)" => {
                    "File type not allowed (forbidden content)"
                }
                "File type not allowed (forbidden MIME type)" => {
                    "File type not allowed (forbidden MIME type)"
                }
                _ => "File type not allowed",
            },
        );
    }

    if files_to_process.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "no_files",
            "no files were uploaded",
        );
    }

    let now = now_secs();
    let ttl = ttl_to_duration(&ttl_code).as_secs();
    let expires = now + ttl;
    let mut saved_files = Vec::new();
    let mut duplicate_info = None;

    for (original_name, data) in &files_to_process {
        if data.len() as u64 > max_file_bytes() {
            tracing::warn!(ip = %ip, ?original_name, size = data.len(), "Upload rejected: file too large");
            continue;
        }
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let hash = format!("{:x}", hasher.finalize());
        if let Some(entry) = state.owners.iter().find(|entry| entry.value().hash == hash) {
            tracing::info!(ip = %ip, ?original_name, file = %entry.key(), "Duplicate upload detected");
            duplicate_info = Some(json!({
                "duplicate": true,
                "file": entry.key(),
                "meta": entry.value()
            }));
            continue;
        }
        let storage_name = make_storage_name(original_name.as_deref());
        if is_forbidden_extension(&storage_name) {
            tracing::warn!(ip = %ip, ?original_name, file = %storage_name, "Upload rejected: forbidden extension");
            continue;
        }
        let path = state.upload_dir.join(&storage_name);
        if fs::write(&path, data).await.is_ok() {
            let meta = FileMeta {
                hash: hash.clone(),
                created: now,
                expires,
                owner: ip.clone(),
                original: original_name.clone().unwrap_or_default(),
            };
            state.owners.insert(storage_name.clone(), meta);
            tracing::info!(ip = %ip, file = %storage_name, size = data.len(), "File uploaded successfully");
            saved_files.push(storage_name);
        } else {
            tracing::error!(ip = %ip, file = %storage_name, "Failed to write uploaded file");
        }
    }

    state.persist_owners().await;
    spawn_integrity_check(state.clone());

    if let Some(dup) = duplicate_info {
        return (StatusCode::CONFLICT, Json(dup)).into_response();
    }

    let truncated = saved_files.len() < files_to_process.len();
    let remaining = files_to_process.len() - saved_files.len();

    (
        StatusCode::OK,
        Json(UploadResponse {
            files: saved_files,
            truncated,
            remaining,
        }),
    )
        .into_response()
}

#[axum::debug_handler]
pub async fn list_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
) -> Response {
    if state.is_banned(&real_client_ip(&headers, &addr)).await {
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    cleanup_expired(&state).await;
    let client_ip = real_client_ip(&headers, &addr);
    let reconcile_report = verify_user_entries_with_report(&state, &client_ip).await;
    cleanup_expired(&state).await;
    check_storage_integrity(&state).await;
    let mut files: Vec<(String, u64, String, u64, u64)> = state
        .owners
        .iter()
        .filter_map(|entry| {
            let m = entry.value();
            if m.owner == client_ip {
                let set = m.created;
                let total = if m.expires > set { m.expires - set } else { 0 };
                Some((
                    entry.key().clone(),
                    m.expires,
                    m.original.clone(),
                    total,
                    set,
                ))
            } else {
                None
            }
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let only_names: Vec<String> = files
        .iter()
        .map(|(n, _, _, _, _)| qualify_path(&state, &format!("f/{}", n)))
        .collect();
    let metas: Vec<FileMetaEntry> = files
        .into_iter()
        .map(|(n, e, o, t, s)| FileMetaEntry {
            file: qualify_path(&state, &format!("f/{}", n)),
            expires: e,
            original: o,
            total: Some(t),
            set: Some(s),
        })
        .collect();
    let body = Json(ListResponse {
        files: only_names,
        metas,
        reconcile: reconcile_report,
    });
    let mut resp = body.into_response();
    resp.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

pub async fn simple_list_handler() -> Response {
    (StatusCode::NOT_IMPLEMENTED, "Not implemented").into_response()
}

#[axum::debug_handler]
pub async fn simple_upload_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let ip = real_client_ip(&headers, &addr);
    let mut ttl_code = "3d".to_string();
    let mut files_to_process = Vec::new();
    let mut forbidden_error: Option<String> = None;
    let mut has_forbidden = false;

    while let Some(field) = multipart.next_field().await.unwrap() {
        let name = if let Some(name) = field.name() {
            name.to_string()
        } else {
            continue;
        };

        if name == "ttl" {
            if let Ok(data) = field.bytes().await {
                if let Ok(s) = std::str::from_utf8(&data) {
                    ttl_code = s.to_string();
                }
            }
            continue;
        }

        if name == "file" || name.starts_with("file") {
            let original_name = field.file_name().map(|s| s.to_string());
            let content_type = field.content_type().map(|m| m.to_string());
            if let Ok(data) = field.bytes().await {
                if !data.is_empty() {
                    let forbidden_mimes: Vec<mime::Mime> = FORBIDDEN_EXTENSIONS
                        .iter()
                        .flat_map(|ext| {
                            mime_guess::from_ext(ext)
                                .iter()
                                .filter_map(|m| m.essence_str().parse().ok())
                        })
                        .collect();
                    let mime_type = if let Some(ref ct) = content_type {
                        ct.parse::<mime::Mime>().ok()
                    } else if let Some(ref orig) = original_name {
                        mime_guess::from_path(orig)
                            .first_raw()
                            .and_then(|m| m.parse().ok())
                    } else {
                        None
                    };
                    let is_forbidden_content = if let Some(kind) = infer::get(&data) {
                        let ext = kind.extension();
                        FORBIDDEN_EXTENSIONS.contains(&ext)
                    } else {
                        false
                    };
                    if is_forbidden_content {
                        tracing::warn!(
                            ?original_name,
                            "Simple upload rejected: forbidden file content detected by infer"
                        );
                        forbidden_error =
                            Some("File type not allowed (forbidden content)".to_string());
                        has_forbidden = true;
                        break;
                    }
                    if let Some(mime) = &mime_type {
                        if forbidden_mimes.iter().any(|forb| forb == mime) {
                            tracing::warn!(
                                ?original_name,
                                mime = %mime,
                                "Simple upload rejected: forbidden MIME type"
                            );
                            forbidden_error =
                                Some("File type not allowed (forbidden MIME type)".to_string());
                            has_forbidden = true;
                            break;
                        }
                    }
                    files_to_process.push((original_name, data));
                }
            }
        }
    }

    if has_forbidden {
        let msg = forbidden_error.unwrap_or_else(|| "File type not allowed".to_string());
        return json_error(
            StatusCode::BAD_REQUEST,
            "bad_filetype",
            match msg.as_str() {
                "File type not allowed (forbidden content)" => {
                    "File type not allowed (forbidden content)"
                }
                "File type not allowed (forbidden MIME type)" => {
                    "File type not allowed (forbidden MIME type)"
                }
                _ => "File type not allowed",
            },
        );
    }

    if files_to_process.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "no_files",
            "no files were uploaded",
        );
    }

    let expires = now_secs() + ttl_to_duration(&ttl_code).as_secs();
    let mut saved_files = Vec::new();

    for (original_name, data) in &files_to_process {
        let now = now_secs();
        if data.len() as u64 > max_file_bytes() {
            tracing::warn!(ip = %ip, ?original_name, size = data.len(), "Simple upload rejected: file too large");
            continue;
        }
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let hash = format!("{:x}", hasher.finalize());
        let storage_name = make_storage_name(original_name.as_deref());
        if is_forbidden_extension(&storage_name) {
            tracing::warn!(ip = %ip, ?original_name, file = %storage_name, "Simple upload rejected: forbidden extension");
            continue;
        }
        let path = state.upload_dir.join(&storage_name);
        if fs::write(&path, data).await.is_ok() {
            let meta = FileMeta {
                owner: ip.clone(),
                expires,
                original: original_name.clone().unwrap_or_default(),
                created: now,
                hash: hash.clone(),
            };
            state.owners.insert(storage_name.clone(), meta);
            tracing::info!(ip = %ip, file = %storage_name, size = data.len(), "Simple file uploaded successfully");
            saved_files.push(storage_name);
        } else {
            tracing::error!(ip = %ip, file = %storage_name, "Failed to write simple uploaded file");
        }
    }

    state.persist_owners().await;
    spawn_integrity_check(state.clone());

    let truncated = saved_files.len() < files_to_process.len();
    let msg = if saved_files.is_empty() {
        "No files uploaded."
    } else if truncated {
        "Some files were too large or invalid and were skipped."
    } else {
        "Upload successful!"
    };
    let url = format!("/simple?m={}", urlencoding::encode(msg));
    (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, url)]).into_response()
}
