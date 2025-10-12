use axum::extract::{ConnectInfo, Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::net::SocketAddr as ClientAddr;
use tokio::fs;
use tracing::{debug, info, warn};
use std::env;

use crate::state::{AppState, cleanup_expired};
use crate::util::{json_error, real_client_ip, PROD_HOST};

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
    // attempt to purge Cloudflare cache for this file in the background
    let file_clone = file.clone();
    tokio::spawn(async move {
        if let Err(e) = purge_cloudflare_file(&file_clone).await {
            warn!(file = %file_clone, error = %e, "cloudflare purge failed");
        } else {
            info!(file = %file_clone, "cloudflare purge requested");
        }
    });
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
        let url = format!(
            "/simple?m={}",
            urlencoding::encode("File not found or not owned by you.")
        );
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
        // background purge for Cloudflare
        let fname_clone = fname.to_string();
        tokio::spawn(async move {
            if let Err(e) = purge_cloudflare_file(&fname_clone).await {
                warn!(file = %fname_clone, error = %e, "cloudflare purge failed");
            } else {
                info!(file = %fname_clone, "cloudflare purge requested");
            }
        });
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

async fn purge_cloudflare_file(fname: &str) -> Result<(), anyhow::Error> {
    // Expect CLOUDFLARE_ZONE_ID and CLOUDFLARE_API_TOKEN to be set in env; if not, no-op.
    let zone_id = match env::var("CLOUDFLARE_ZONE_ID") {
        Ok(v) if !v.is_empty() => v,
        _ => return Ok(()),
    };
    let api_token = match env::var("CLOUDFLARE_API_TOKEN") {
        Ok(v) if !v.is_empty() => v,
        _ => return Ok(()),
    };
    // Build the fully-qualified file URL to purge
    let encoded = urlencoding::encode(fname);
    let file_url = format!("https://{}/f/{}", PROD_HOST, encoded);

    let client = reqwest::Client::new();
    let api_url = format!(
        "https://api.cloudflare.com/client/v4/zones/{}/purge_cache",
        zone_id
    );
    let body_json = serde_json::json!({"files": [file_url]});
    let body_bytes = serde_json::to_vec(&body_json).map_err(|e| anyhow::anyhow!(e))?;
    let resp = client
        .post(&api_url)
        .bearer_auth(api_token)
        .header("Content-Type", "application/json")
        .body(body_bytes)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let txt = resp.text().await.unwrap_or_else(|_| "<no body>".into());
        return Err(anyhow::anyhow!("cloudflare purge failed: {} {}", status, txt));
    }
    Ok(())
}
