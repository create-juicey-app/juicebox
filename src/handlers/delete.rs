use axum::extract::{ConnectInfo, Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::net::SocketAddr as ClientAddr;
use tokio::fs;
use tracing::debug;

use crate::state::{cleanup_expired, AppState};
use crate::util::{json_error, real_client_ip};

#[derive(Deserialize)]
pub struct SimpleDeleteForm {
    pub f: String,
}

#[axum::debug_handler]
pub async fn delete_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    Path(file): Path<String>,
) -> Response {
    let ip = real_client_ip(&headers, &addr);
    if state.is_banned(&ip).await {
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let Some((_, owner_hash)) = state.hash_ip(&ip) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    if file.contains('/') || file.contains("..") || file.contains('\\') {
        return json_error(StatusCode::BAD_REQUEST, "bad_file", "invalid file name");
    }
    cleanup_expired(&state).await;
    match state.owners.get(&file) {
        Some(meta) if meta.value().owner_hash == owner_hash => {}
        _ => return (StatusCode::NOT_FOUND, "not found").into_response(),
    }
    state.owners.remove(&file);
    let path = state.upload_dir.join(&file);
    let _ = fs::remove_file(&path).await;
    state.persist_owners().await;
    (StatusCode::NO_CONTENT, ()).into_response()
}

pub async fn simple_delete_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    Query(frm): Query<SimpleDeleteForm>,
) -> Response {
    handle_simple_delete(state, addr, headers, frm.f).await
}

#[axum::debug_handler]
pub async fn simple_delete_post_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<ClientAddr>,
    headers: HeaderMap,
    Form(frm): Form<SimpleDeleteForm>,
) -> Response {
    handle_simple_delete(state, addr, headers, frm.f).await
}

async fn handle_simple_delete(
    state: AppState,
    addr: ClientAddr,
    headers: HeaderMap,
    f: String,
) -> Response {
    let ip = real_client_ip(&headers, &addr);
    let Some((_, owner_hash)) = state.hash_ip(&ip) else {
        let url = format!("/simple?m={}", urlencoding::encode("File not found or not owned by you."));
        return (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, url)]).into_response();
    };
    let fname = f.trim();
    if fname.is_empty() || fname.contains('/') || fname.contains("..") || fname.contains('\\') {
        debug!(file = fname, "simple delete rejected: invalid name");
        let url = format!("/simple?m={}", urlencoding::encode("Invalid file name."));
        return (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, url)]).into_response();
    }
    let can_delete = match state.owners.get(fname) {
        Some(meta) if meta.value().owner_hash == owner_hash => true,
        _ => false,
    };
    if can_delete {
        debug!(file = fname, owner_hash = %owner_hash, "simple delete: removing owned file");
        state.owners.remove(fname);
        let path = state.upload_dir.join(fname);
        let _ = fs::remove_file(&path).await;
        state.persist_owners().await;
        let url = format!(
            "/simple?m={}",
            urlencoding::encode("File Deleted Successfully.")
        );
        (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, url)]).into_response()
    } else {
        debug!(file = fname, owner_hash = %owner_hash, "simple delete: no ownership match");
        let url = format!(
            "/simple?m={}",
            urlencoding::encode("File not found or not owned by you.")
        );
        (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, url)]).into_response()
    }
}
