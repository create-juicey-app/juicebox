mod util;
mod state;
mod rate_limit;
mod handlers;

use std::{collections::HashMap, path::PathBuf, sync::Arc, net::SocketAddr, time::{Duration, SystemTime}};
use tokio::fs; use tokio::sync::{RwLock, Semaphore};
use axum::{Router, middleware};
use crate::state::{AppState, FileMeta, ReportRecord, cleanup_expired};
use crate::util::{ttl_to_duration, now_secs, PROD_HOST, UPLOAD_CONCURRENCY};
use crate::handlers::{build_router, add_security_headers, enforce_host};
use crate::rate_limit::build_rate_limiter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let production = std::env::var("APP_ENV").map(|v| v.eq_ignore_ascii_case("production")).unwrap_or(false);

    let static_dir = Arc::new(PathBuf::from("./public"));
    let upload_dir = Arc::new(PathBuf::from("./files"));
    let data_dir = Arc::new(PathBuf::from("./data"));
    let metadata_path = Arc::new(data_dir.join("file_owners.json"));
    let reports_path = Arc::new(data_dir.join("reports.json"));

    fs::create_dir_all(&*static_dir).await?;
    fs::create_dir_all(&*upload_dir).await?;
    fs::create_dir_all(&*data_dir).await?;

    let owners_map: HashMap<String, FileMeta> = match fs::read(&*metadata_path).await {
        Ok(data) => {
            if let Ok(old_map) = serde_json::from_slice::<HashMap<String,String>>(&data) {
                old_map.into_iter().map(|(k,v)| (k, FileMeta { owner: v, expires: now_secs() + ttl_to_duration("3d").as_secs() })).collect()
            } else { serde_json::from_slice(&data).unwrap_or_default() }
        }
        Err(_) => HashMap::new(),
    };

    let reports_vec: Vec<ReportRecord> = match fs::read(&*reports_path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    let initial_mtime = fs::metadata(&*metadata_path).await
        .ok()
        .and_then(|m| m.modified().ok())
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let state = AppState {
        upload_dir,
        static_dir,
        metadata_path,
        owners: Arc::new(RwLock::new(owners_map)),
        upload_sem: Arc::new(Semaphore::new(UPLOAD_CONCURRENCY)),
        production,
        last_meta_mtime: Arc::new(RwLock::new(initial_mtime)),
        reports_path,
        reports: Arc::new(RwLock::new(reports_vec)),
    };

    // periodic cleanup task
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(600));
        loop {
            interval.tick().await;
            cleanup_expired(&cleanup_state).await;
        }
    });

    let rate_layer = build_rate_limiter();
    let router = build_router(state.clone());
    let mut app: Router = router
        .layer(middleware::from_fn(add_security_headers))
        .layer(rate_layer)
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024 * 512));
    if state.production { app = app.layer(middleware::from_fn(enforce_host)); }

    let addr: SocketAddr = ([0,0,0,0], 1200).into();
    println!("listening on {addr} (prod host: {})", PROD_HOST);
    axum_server::bind(addr)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await?;
    Ok(())
}
