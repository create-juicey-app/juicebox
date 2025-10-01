use axum::extract::{ConnectInfo, Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::net::SocketAddr as ClientAddr;
use tokio::fs;

use crate::state::{AppState, cleanup_expired};
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
    if file.contains('/') || file.contains("..") || file.contains('\\') {
        return json_error(StatusCode::BAD_REQUEST, "bad_file", "invalid file name");
    }
    cleanup_expired(&state).await;
    match state.owners.get(&file) {
        Some(meta) if meta.value().owner == ip => {}
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
    println!("[DEBUG] handle_simple_delete: called with f='{}'", f);
    let ip = real_client_ip(&headers, &addr);
    println!("[DEBUG] handle_simple_delete: real_client_ip='{}'", ip);
    let fname = f.trim();
    if fname.is_empty() || fname.contains('/') || fname.contains("..") || fname.contains('\\') {
        println!(
            "[DEBUG] handle_simple_delete: invalid file name '{}', returning error",
            fname
        );
        let url = format!("/simple?m={}", urlencoding::encode("Invalid file name."));
        return (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, url)]).into_response();
    }
    let can_delete = match state.owners.get(fname) {
        Some(meta) if meta.value().owner == ip => true,
        _ => false,
    };
    if can_delete {
        println!(
            "[DEBUG] handle_simple_delete: found file '{}' owned by '{}', deleting",
            fname, ip
        );
        state.owners.remove(fname);
        let path = state.upload_dir.join(fname);
        let _ = fs::remove_file(&path).await;
        println!(
            "[DEBUG] handle_simple_delete: file '{}' removed from disk (if existed)",
            fname
        );
        state.persist_owners().await;
        println!("[DEBUG] handle_simple_delete: owners persisted");
        let url = format!(
            "/simple?m={}",
            urlencoding::encode("File Deleted Successfully.")
        );
        println!("[DEBUG] handle_simple_delete: returning success redirect");
        (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, url)]).into_response()
    } else {
        println!(
            "[DEBUG] handle_simple_delete: file '{}' not found or not owned by '{}', returning error",
            fname, ip
        );
        let url = format!(
            "/simple?m={}",
            urlencoding::encode("File not found or not owned by you.")
        );
        (StatusCode::SEE_OTHER, [(axum::http::header::LOCATION, url)]).into_response()
    }
}
