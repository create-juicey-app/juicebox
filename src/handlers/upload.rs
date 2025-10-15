use axum::Json;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Multipart, Path, Query as AxumQuery, State};
use axum::http::header::{ALLOW, CACHE_CONTROL};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use infer;
use mime_guess::mime;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::net::SocketAddr as ClientAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, trace, warn};

use crate::state::{
    AppState, ChunkSession, FileMeta, ReconcileReport, check_storage_integrity, cleanup_expired,
    spawn_integrity_check, verify_user_entries_with_report,
};
use crate::util::{
    FORBIDDEN_EXTENSIONS, MAX_ACTIVE_FILES_PER_IP, is_forbidden_extension, json_error,
    make_storage_name, max_file_bytes, new_id, now_secs, qualify_path, real_client_ip,
    ttl_to_duration,
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
    #[serde(default)]
    pub limit_reached: bool,
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

#[derive(Serialize)]
pub struct ChunkStatusResponse {
    pub total_chunks: u32,
    pub assembled_chunks: u32,
    pub completed: bool,
}

#[axum::debug_handler]
pub async fn init_chunk_options_handler() -> Response {
    no_content_with_allow("POST, OPTIONS")
}

#[axum::debug_handler]
pub async fn chunk_part_options_handler(Path(_): Path<ChunkPathParams>) -> Response {
    no_content_with_allow("PUT, OPTIONS")
}

#[axum::debug_handler]
pub async fn chunk_complete_options_handler(Path(_): Path<ChunkCompletePath>) -> Response {
    no_content_with_allow("POST, OPTIONS")
}

#[axum::debug_handler]
pub async fn chunk_cancel_options_handler(Path(_): Path<ChunkCompletePath>) -> Response {
    no_content_with_allow("DELETE, OPTIONS")
}

fn no_content_with_allow(methods: &str) -> Response {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(methods) {
        headers.insert(ALLOW, value);
    }
    (StatusCode::NO_CONTENT, headers).into_response()
}

#[axum::debug_handler]
pub async fn upload_head_handler() -> Response {
    no_content_with_allow("POST, HEAD, OPTIONS")
}

#[axum::debug_handler]
pub async fn upload_options_handler() -> Response {
    no_content_with_allow("POST, HEAD, OPTIONS")
}

fn file_limit_response() -> Response {
    let message = format!(
        "Active file limit reached. Delete an existing upload to free one of the {MAX_ACTIVE_FILES_PER_IP} slots."
    );
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "code": "file_limit",
            "message": message,
        })),
    )
        .into_response()
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
    let client_ip = real_client_ip(&headers, &addr);
    trace!(%client_ip, "chunk upload init request received");
    if state.is_banned(&client_ip).await {
        warn!(%client_ip, "chunk upload init rejected: banned ip");
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let owner_hash = if let Some(hash) = state.hash_ip_to_string(&client_ip) {
        hash
    } else {
        warn!(%client_ip, "chunk upload init failed: unable to hash ip");
        return json_error(
            StatusCode::FORBIDDEN,
            "invalid_ip",
            "unable to fingerprint client",
        );
    };
    if req.size == 0 {
        warn!(%client_ip, "chunk upload init rejected: empty size");
        return json_error(StatusCode::BAD_REQUEST, "empty", "file size required");
    }
    if req.size > max_file_bytes() {
        warn!(
            %client_ip,
            requested = req.size,
            limit = max_file_bytes(),
            "chunk upload init rejected: size over limit"
        );
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "file exceeds configured max size",
        );
    }
    cleanup_expired(&state).await;
    let now = now_secs();
    let ttl_code = req.ttl.clone().unwrap_or_else(|| "24h".to_string());
    let ttl = ttl_to_duration(&ttl_code).as_secs();
    let expires = now + ttl;

    let (chunk_size, total_chunks) =
        if let Some(layout) = compute_chunk_layout(req.size, req.chunk_size) {
            layout
        } else {
            warn!(
                %client_ip,
                requested_size = req.size,
                requested_chunk = ?req.chunk_size,
                "chunk upload init rejected: unable to compute chunk layout"
            );
            return json_error(
                StatusCode::BAD_REQUEST,
                "chunk_layout",
                "unable to compute chunk layout",
            );
        };

    if let Some(hash) = req.hash.as_ref() {
        if let Some((file, meta)) = find_duplicate_by_hash(&state, hash) {
            info!(%client_ip, file = %file, "chunk upload init detected duplicate hash");
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

    if state.remaining_file_slots(owner_hash.as_str(), now) == 0 {
        warn!(owner_hash = %owner_hash, "chunk upload rejected: active file limit reached");
        return file_limit_response();
    }

    let session_id = new_id();
    let storage_name = make_storage_name(Some(&req.filename));
    let storage_dir_path = state.chunk_dir.join(&session_id);
    if let Err(err) = fs::create_dir_all(&storage_dir_path).await {
        error!(?err, session_id = %session_id, dir = ?storage_dir_path, "failed to create chunk directory");
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "chunk_dir",
            "failed to initialize chunk upload",
        );
    }

    let session = Arc::new(ChunkSession {
        owner_hash: owner_hash.clone(),
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
        assembled_chunks: AtomicU32::new(0),
    });
    state
        .chunk_sessions
        .insert(session_id.clone(), session.clone());
    let post_insert_now = now_secs();
    let reserved_after = state.reserved_file_slots(session.owner_hash.as_str(), post_insert_now);
    if reserved_after > MAX_ACTIVE_FILES_PER_IP {
        state.remove_chunk_session(&session_id).await;
        warn!(owner_hash = %session.owner_hash, "chunk upload rejected after init: active file limit reached");
        return file_limit_response();
    }
    if let Err(err) = state
        .persist_chunk_session(&session_id, session.as_ref())
        .await
    {
        error!(?err, session_id = %session_id, "failed to persist chunk session metadata");
        state.remove_chunk_session(&session_id).await;
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "chunk_dir",
            "failed to initialize chunk upload",
        );
    }

    info!(
        %client_ip,
        owner_hash = %owner_hash,
        session_id = %session_id,
        filename = %req.filename,
        size = req.size,
        chunk_size,
        total_chunks,
        "chunk upload session initialized"
    );

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
    let client_ip = real_client_ip(&headers, &addr);
    trace!(%client_ip, session_id = %params.id, index = params.index, size = body.len(), "chunk upload part received");
    if state.is_banned(&client_ip).await {
        warn!(%client_ip, session_id = %params.id, "chunk upload part rejected: banned ip");
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let owner_hash = if let Some(hash) = state.hash_ip_to_string(&client_ip) {
        hash
    } else {
        warn!(%client_ip, "chunk upload part rejected: unable to hash ip");
        return json_error(
            StatusCode::FORBIDDEN,
            "invalid_ip",
            "unable to fingerprint client",
        );
    };
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
    if session.owner_hash != owner_hash {
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
        warn!(session_id = %params.id, owner_hash = %owner_hash, ?expected, got = body.len(), "chunk upload part rejected: length mismatch");
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
        error!(?err, session_id = %params.id, chunk_index = params.index, path = ?chunk_path, "failed to persist chunk");
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
        warn!(?err, session_id = %params.id, "failed to persist chunk session after part");
    }
    info!(
        session_id = %params.id,
        owner_hash = %owner_hash,
        chunk_index = params.index,
        "chunk upload part stored"
    );
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(axum::body::Body::empty())
        .unwrap()
}

#[axum::debug_handler]
#[tracing::instrument(
    name = "upload.chunk.complete",
    skip(state, headers, req),
    fields(
        client_ip = tracing::field::Empty,
        session = %path.id,
        owner_hash = tracing::field::Empty,
        storage = tracing::field::Empty,
        expected_hash = tracing::field::Empty
    )
)]
pub async fn complete_chunk_upload_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    Path(path): Path<ChunkCompletePath>,
    Json(req): Json<ChunkCompleteRequest>,
) -> Response {
    let client_ip = real_client_ip(&headers, &addr);
    tracing::Span::current().record("client_ip", tracing::field::display(&client_ip));
    trace!(%client_ip, session_id = %path.id, "chunk completion requested");
    if state.is_banned(&client_ip).await {
        warn!(%client_ip, session_id = %path.id, "chunk completion rejected: banned ip");
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let owner_hash = if let Some(hash) = state.hash_ip_to_string(&client_ip) {
        hash
    } else {
        warn!(%client_ip, session_id = %path.id, "chunk completion rejected: unable to hash ip");
        return json_error(
            StatusCode::FORBIDDEN,
            "invalid_ip",
            "unable to fingerprint client",
        );
    };
    tracing::Span::current().record("owner_hash", tracing::field::display(&owner_hash));
    let Some(session_entry) = state.chunk_sessions.get(&path.id) else {
        return json_error(
            StatusCode::NOT_FOUND,
            "chunk_session",
            "upload session not found",
        );
    };
    let session = session_entry.value().clone();
    drop(session_entry);
    if session.owner_hash != owner_hash {
        warn!(session_id = %path.id, owner_hash = %owner_hash, "chunk completion rejected: ownership mismatch");
        return json_error(
            StatusCode::FORBIDDEN,
            "not_owner",
            "upload session not owned by ip",
        );
    }
    if session.is_completed() {
        debug!(session_id = %path.id, "chunk completion called on already completed session");
        return json_error(
            StatusCode::BAD_REQUEST,
            "completed",
            "upload session already completed",
        );
    }
    {
        let received = session.received.read().await;
        if received.iter().any(|r| !*r) {
            warn!(session_id = %path.id, "chunk completion rejected: missing chunks");
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
    tracing::Span::current().record("storage", tracing::field::display(&storage_name));
    let final_path = state.upload_dir.join(&storage_name);
    let mut tmp_path = final_path.clone();
    tmp_path.set_extension("part");
    let start = tokio::time::Instant::now();
    let mut file = match fs::File::create(&tmp_path).await {
        Ok(f) => f,
        Err(err) => {
            drop(permit);
            error!(?err, ?tmp_path, session_id = %path.id, "failed to create assembled file");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "final_create",
                "failed to assemble upload",
            );
        }
    };
    session.assembled_chunks.store(0, Ordering::Relaxed);
    let mut hasher = Sha256::new();
    let mut chunk_buf = Vec::with_capacity(session.chunk_size as usize);
    let open_elapsed = start.elapsed();
    debug!(session = %path.id, elapsed_ms = open_elapsed.as_millis(), "chunk completion: file create ready");
    for idx in 0..session.total_chunks {
        let chunk_start = tokio::time::Instant::now();
        let chunk_path = session.storage_dir.join(format!("{:06}.chunk", idx));
        let mut chunk_file = match fs::File::open(&chunk_path).await {
            Ok(f) => f,
            Err(err) => {
                drop(permit);
                let _ = fs::remove_file(&tmp_path).await;
                error!(?err, ?chunk_path, session_id = %path.id, chunk = idx, "missing chunk during assembly");
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "chunk_missing",
                    "missing chunk during assembly",
                );
            }
        };
        let expected_len = expected_chunk_len(&session, idx);
        chunk_buf.resize(expected_len as usize, 0);
        if let Err(err) = chunk_file.read_exact(&mut chunk_buf).await {
            drop(permit);
            let _ = fs::remove_file(&tmp_path).await;
            let code = if err.kind() == std::io::ErrorKind::UnexpectedEof {
                error!(
                    actual = chunk_buf.len(),
                    expected = expected_len,
                    ?chunk_path,
                    session_id = %path.id,
                    chunk = idx,
                    "chunk length mismatch during assembly"
                );
                json_error(
                    StatusCode::BAD_REQUEST,
                    "chunk_size",
                    "chunk length mismatch",
                )
            } else {
                error!(?err, ?chunk_path, session_id = %path.id, chunk = idx, "failed reading chunk");
                json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "chunk_read",
                    "failed reading chunk data",
                )
            };
            return code;
        }
        if let Err(err) = file.write_all(&chunk_buf).await {
            drop(permit);
            let _ = fs::remove_file(&tmp_path).await;
            error!(?err, ?chunk_path, session_id = %path.id, chunk = idx, "failed writing assembled file");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "write",
                "failed writing assembled file",
            );
        }
        hasher.update(&chunk_buf);
        session.assembled_chunks.store(
            std::cmp::min(idx + 1, session.total_chunks),
            Ordering::Relaxed,
        );
        let chunk_elapsed = chunk_start.elapsed();
        if chunk_elapsed.as_millis() >= 25 {
            warn!(session = %path.id, chunk = idx, elapsed_ms = chunk_elapsed.as_millis(), "chunk completion: slow chunk assembly");
        }
    }
    let assemble_elapsed = start.elapsed();
    debug!(session = %path.id, elapsed_ms = assemble_elapsed.as_millis(), "chunk completion: chunks assembled");
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
    session
        .assembled_chunks
        .store(session.total_chunks, Ordering::Relaxed);
    let finalize_elapsed = start.elapsed();
    debug!(session = %path.id, elapsed_ms = finalize_elapsed.as_millis(), "chunk completion: file moved");

    let digest = format!("{:x}", hasher.finalize());
    let expected_hash = req
        .hash
        .as_ref()
        .or_else(|| session.hash.as_ref())
        .map(|s| s.as_str());
    if let Some(exp) = expected_hash {
        tracing::Span::current().record("expected_hash", tracing::field::display(exp));
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
        owner_hash: session.owner_hash.clone(),
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
        warn!(?err, session_id = %path.id, "failed to persist completed chunk session before cleanup");
    }
    state.owners.insert(storage_name.clone(), meta);
    let persist_start = tokio::time::Instant::now();
    state.spawn_persist_owners();
    let persist_latency = persist_start.elapsed();
    debug!(session = %path.id, elapsed_us = persist_latency.as_micros(), "chunk completion: spawned owner persist");
    let cleanup_start = tokio::time::Instant::now();
    state.remove_chunk_session(&path.id).await;
    let cleanup_elapsed = cleanup_start.elapsed();
    if cleanup_elapsed.as_millis() >= 50 {
        warn!(session = %path.id, elapsed_ms = cleanup_elapsed.as_millis(), "chunk completion: slow cleanup");
    } else {
        debug!(session = %path.id, elapsed_ms = cleanup_elapsed.as_millis(), "chunk completion: cleanup complete");
    }
    info!(
        %client_ip,
        session_id = %path.id,
        storage = %storage_name,
        size = session.total_bytes,
        hash = %digest,
        total_ms = start.elapsed().as_millis(),
        "chunk completion finished"
    );
    spawn_integrity_check(state.clone());

    Json(UploadResponse {
        files: vec![storage_name],
        truncated: false,
        remaining: 0,
        limit_reached: false,
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
    let client_ip = real_client_ip(&headers, &addr);
    trace!(%client_ip, session_id = %path.id, "chunk cancel requested");
    if state.is_banned(&client_ip).await {
        warn!(%client_ip, session_id = %path.id, "chunk cancel rejected: banned ip");
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let owner_hash = if let Some(hash) = state.hash_ip_to_string(&client_ip) {
        hash
    } else {
        warn!(%client_ip, session_id = %path.id, "chunk cancel rejected: unable to hash ip");
        return json_error(
            StatusCode::FORBIDDEN,
            "invalid_ip",
            "unable to fingerprint client",
        );
    };
    let Some(entry) = state.chunk_sessions.get(&path.id) else {
        return json_error(
            StatusCode::NOT_FOUND,
            "chunk_session",
            "upload session not found",
        );
    };
    if entry.value().owner_hash != owner_hash {
        return json_error(
            StatusCode::FORBIDDEN,
            "not_owner",
            "upload session not owned by ip",
        );
    }
    drop(entry);
    state.remove_chunk_session(&path.id).await;
    info!(%client_ip, session_id = %path.id, "chunk session cancelled");
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(axum::body::Body::empty())
        .unwrap()
}

#[axum::debug_handler]
#[tracing::instrument(
    name = "upload.chunk.status",
    skip(state, headers),
    fields(
        client_ip = tracing::field::Empty,
        session = %path.id,
        owner_hash = tracing::field::Empty
    )
)]
pub async fn chunk_status_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    Path(path): Path<ChunkCompletePath>,
) -> Response {
    let client_ip = real_client_ip(&headers, &addr);
    tracing::Span::current().record("client_ip", tracing::field::display(&client_ip));
    trace!(%client_ip, session_id = %path.id, "chunk status requested");
    if state.is_banned(&client_ip).await {
        warn!(%client_ip, session_id = %path.id, "chunk status rejected: banned ip");
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let owner_hash = if let Some(hash) = state.hash_ip_to_string(&client_ip) {
        hash
    } else {
        warn!(%client_ip, session_id = %path.id, "chunk status rejected: unable to hash ip");
        return json_error(
            StatusCode::FORBIDDEN,
            "invalid_ip",
            "unable to fingerprint client",
        );
    };
    tracing::Span::current().record("owner_hash", tracing::field::display(&owner_hash));
    let Some(session_entry) = state.chunk_sessions.get(&path.id) else {
        return json_error(
            StatusCode::NOT_FOUND,
            "chunk_session",
            "upload session not found",
        );
    };
    let session = session_entry.value().clone();
    drop(session_entry);
    if session.owner_hash != owner_hash {
        return json_error(
            StatusCode::FORBIDDEN,
            "not_owner",
            "upload session not owned by ip",
        );
    }
    let total = session.total_chunks;
    let assembled = session.assembled_chunks.load(Ordering::Relaxed).min(total);
    debug!(%client_ip, session_id = %path.id, total, assembled, completed = session.is_completed(), "chunk status returned");
    Json(ChunkStatusResponse {
        total_chunks: total,
        assembled_chunks: assembled,
        completed: session.is_completed() && assembled >= total,
    })
    .into_response()
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
    debug!(hash = %query.hash, exists, "hash check performed");
    Json(json!({ "exists": exists })).into_response()
}

#[axum::debug_handler]
#[tracing::instrument(
    name = "upload.multipart",
    skip(state, headers, multipart),
    fields(client_ip = tracing::field::Empty)
)]
pub async fn upload_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let client_ip = real_client_ip(&headers, &addr);
    tracing::Span::current().record("client_ip", tracing::field::display(&client_ip));
    trace!(%client_ip, "multipart upload request received");
    if state.is_banned(&client_ip).await {
        warn!(%client_ip, "upload rejected: banned ip");
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let owner_hash = if let Some(hash) = state.hash_ip_to_string(&client_ip) {
        hash
    } else {
        warn!(%client_ip, "upload rejected: unable to hash ip");
        return json_error(
            StatusCode::FORBIDDEN,
            "invalid_ip",
            "unable to fingerprint client",
        );
    };
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

    cleanup_expired(&state).await;
    let now = now_secs();
    let mut slots_remaining = state.remaining_file_slots(owner_hash.as_str(), now);
    if slots_remaining == 0 {
        tracing::warn!(owner_hash = %owner_hash, "Upload rejected: active file limit reached");
        return file_limit_response();
    }
    let ttl = ttl_to_duration(&ttl_code).as_secs();
    let expires = now + ttl;
    let mut saved_files = Vec::new();
    let mut duplicate_info = None;
    let mut limit_reached = false;

    for (original_name, data) in &files_to_process {
        if slots_remaining == 0 {
            limit_reached = true;
            break;
        }
        if data.len() as u64 > max_file_bytes() {
            tracing::warn!(owner_hash = %owner_hash, ?original_name, size = data.len(), "Upload rejected: file too large");
            continue;
        }
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let hash = format!("{:x}", hasher.finalize());
        if let Some(entry) = state.owners.iter().find(|entry| entry.value().hash == hash) {
            tracing::info!(owner_hash = %owner_hash, ?original_name, file = %entry.key(), "Duplicate upload detected");
            duplicate_info = Some(json!({
                "duplicate": true,
                "file": entry.key(),
                "meta": entry.value()
            }));
            continue;
        }
        let storage_name = make_storage_name(original_name.as_deref());
        if is_forbidden_extension(&storage_name) {
            tracing::warn!(owner_hash = %owner_hash, ?original_name, file = %storage_name, "Upload rejected: forbidden extension");
            continue;
        }
        let path = state.upload_dir.join(&storage_name);
        if fs::write(&path, data).await.is_ok() {
            let meta = FileMeta {
                hash: hash.clone(),
                created: now,
                expires,
                owner_hash: owner_hash.clone(),
                original: original_name.clone().unwrap_or_default(),
            };
            state.owners.insert(storage_name.clone(), meta);
            let check_now = now_secs();
            let total_reserved = state.reserved_file_slots(owner_hash.as_str(), check_now);
            if total_reserved > MAX_ACTIVE_FILES_PER_IP {
                state.owners.remove(&storage_name);
                let _ = fs::remove_file(&path).await;
                tracing::warn!(
                    owner_hash = %owner_hash,
                    file = %storage_name,
                    "Upload rejected: active file limit reached (post-write)",
                );
                return file_limit_response();
            }
            tracing::info!(owner_hash = %owner_hash, file = %storage_name, size = data.len(), "File uploaded successfully");
            saved_files.push(storage_name.clone());
            slots_remaining = slots_remaining.saturating_sub(1);
        } else {
            tracing::error!(owner_hash = %owner_hash, file = %storage_name, "Failed to write uploaded file");
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
            limit_reached,
        }),
    )
        .into_response()
}

#[axum::debug_handler]
#[tracing::instrument(
    name = "files.list",
    skip(state, headers),
    fields(client_ip = tracing::field::Empty)
)]
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
    tracing::Span::current().record("client_ip", tracing::field::display(&client_ip));
    let Some(owner_hash) = state.hash_ip_to_string(&client_ip) else {
        return json_error(
            StatusCode::FORBIDDEN,
            "invalid_ip",
            "unable to fingerprint client",
        );
    };
    let reconcile_report = verify_user_entries_with_report(&state, &owner_hash).await;
    cleanup_expired(&state).await;
    check_storage_integrity(&state).await;
    let mut files: Vec<(String, u64, String, u64, u64)> = state
        .owners
        .iter()
        .filter_map(|entry| {
            let m = entry.value();
            if m.owner_hash == owner_hash {
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
#[tracing::instrument(
    name = "upload.simple",
    skip(state, headers, multipart),
    fields(client_ip = tracing::field::Empty)
)]
pub async fn simple_upload_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let ip = real_client_ip(&headers, &addr);
    tracing::Span::current().record("client_ip", tracing::field::display(&ip));
    trace!(%ip, "simple upload request received");
    if state.is_banned(&ip).await {
        warn!(%ip, "simple upload rejected: banned ip");
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let owner_hash = if let Some(hash) = state.hash_ip_to_string(&ip) {
        hash
    } else {
        warn!(%ip, "simple upload rejected: unable to hash ip");
        return json_error(
            StatusCode::FORBIDDEN,
            "invalid_ip",
            "unable to fingerprint client",
        );
    };
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

    cleanup_expired(&state).await;
    let now = now_secs();
    let mut slots_remaining = state.remaining_file_slots(owner_hash.as_str(), now);
    if slots_remaining == 0 {
        tracing::warn!(owner_hash = %owner_hash, "Simple upload rejected: active file limit reached");
        return file_limit_response();
    }

    let expires = now + ttl_to_duration(&ttl_code).as_secs();
    let mut saved_files: Vec<String> = Vec::new();
    let mut limit_reached = false;

    for (original_name, data) in &files_to_process {
        if slots_remaining == 0 {
            limit_reached = true;
            break;
        }
        let created = now_secs();
        if data.len() as u64 > max_file_bytes() {
            tracing::warn!(owner_hash = %owner_hash, ?original_name, size = data.len(), "Simple upload rejected: file too large");
            continue;
        }
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let hash = format!("{:x}", hasher.finalize());
        let storage_name = make_storage_name(original_name.as_deref());
        if is_forbidden_extension(&storage_name) {
            tracing::warn!(owner_hash = %owner_hash, ?original_name, file = %storage_name, "Simple upload rejected: forbidden extension");
            continue;
        }
        let path = state.upload_dir.join(&storage_name);
        if fs::write(&path, data).await.is_ok() {
            let meta = FileMeta {
                owner_hash: owner_hash.clone(),
                expires,
                original: original_name.clone().unwrap_or_default(),
                created,
                hash: hash.clone(),
            };
            state.owners.insert(storage_name.clone(), meta);
            let check_now = now_secs();
            let total_reserved = state.reserved_file_slots(owner_hash.as_str(), check_now);
            if total_reserved > MAX_ACTIVE_FILES_PER_IP {
                state.owners.remove(storage_name.as_str());
                let _ = fs::remove_file(&path).await;
                tracing::warn!(owner_hash = %owner_hash, file = %storage_name, "Simple upload rejected: active file limit reached (post-write)");
                limit_reached = true;
                break;
            }
            tracing::info!(owner_hash = %owner_hash, file = %storage_name, size = data.len(), "Simple file uploaded successfully");
            saved_files.push(storage_name.clone());
            slots_remaining = slots_remaining.saturating_sub(1);
        } else {
            tracing::error!(owner_hash = %owner_hash, file = %storage_name, "Failed to write simple uploaded file");
        }
    }

    if limit_reached && saved_files.is_empty() {
        state.persist_owners().await;
        spawn_integrity_check(state.clone());
        return file_limit_response();
    }

    state.persist_owners().await;
    spawn_integrity_check(state.clone());

    let truncated = saved_files.len() < files_to_process.len();
    let msg = if limit_reached {
        format!(
            "Some files were discarded because you reached the {} active file limit.",
            MAX_ACTIVE_FILES_PER_IP
        )
    } else if saved_files.is_empty() {
        "No files uploaded.".to_string()
    } else if truncated {
        "Some files were too large or invalid and were skipped.".to_string()
    } else {
        "Upload successful!".to_string()
    };
    let url = format!("/simple?m={}", urlencoding::encode(&msg));
    (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, url)]).into_response()
}
