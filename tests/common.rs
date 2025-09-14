mod common {
}

use std::{collections::HashMap, sync::Arc, time::SystemTime};
use tokio::sync::{RwLock, Semaphore};
use juicebox::state::{AppState, FileMeta, ReportRecord};
use juicebox::util::UPLOAD_CONCURRENCY;
use tempfile::TempDir;

pub fn setup_test_app() -> (AppState, TempDir) {
    let temp_dir = tempfile::tempdir().unwrap();
    let base_path = temp_dir.path();

    let static_dir = Arc::new(base_path.join("public"));
    let upload_dir = Arc::new(base_path.join("files"));
    let data_dir = Arc::new(base_path.join("data"));
    let metadata_path = Arc::new(data_dir.join("file_owners.json"));
    let reports_path = Arc::new(data_dir.join("reports.json"));
    let admin_sessions_path = Arc::new(data_dir.join("admin_sessions.json"));

    std::fs::create_dir_all(&*static_dir).unwrap();
    std::fs::create_dir_all(&*upload_dir).unwrap();
    std::fs::create_dir_all(&*data_dir).unwrap();

    let state = AppState {
        upload_dir,
        static_dir,
        metadata_path,
        owners: Arc::new(RwLock::new(HashMap::<String, FileMeta>::new())),
        upload_sem: Arc::new(Semaphore::new(UPLOAD_CONCURRENCY)),
        production: false,
        last_meta_mtime: Arc::new(RwLock::new(SystemTime::UNIX_EPOCH)),
        reports_path,
        reports: Arc::new(RwLock::new(Vec::<ReportRecord>::new())),
        admin_sessions_path,
        admin_sessions: Arc::new(RwLock::new(HashMap::<String,u64>::new())),
    };

    (state, temp_dir)
}