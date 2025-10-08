mod common {}

use juicebox::state::{AppState, ReportRecord};
use juicebox::util::{hash_ip_string, UPLOAD_CONCURRENCY};
use std::{collections::HashMap, path::Path, sync::Arc, time::SystemTime};
use tempfile::TempDir;
use tokio::sync::{RwLock, Semaphore};

pub const PRIMARY_HASH_SECRET: [u8; 32] = [0x11; 32];
pub const SECONDARY_HASH_SECRET: [u8; 32] = [0x22; 32];

pub fn hash_fixture_ip(ip: &str) -> String {
    hash_ip_string(&PRIMARY_HASH_SECRET, ip)
        .map(|(_, hash)| hash)
        .expect("hash fixture ip")
}

pub fn setup_test_app() -> (AppState, TempDir) {
    let temp_dir = tempfile::tempdir().unwrap();
    let base_path = temp_dir.path();

    let static_dir = Arc::new(base_path.join("public"));
    let upload_dir = Arc::new(base_path.join("files"));
    let data_dir = Arc::new(base_path.join("data"));
    let metadata_path = Arc::new(data_dir.join("file_owners.json"));
    let reports_path = Arc::new(data_dir.join("reports.json"));
    let admin_sessions_path = Arc::new(data_dir.join("admin_sessions.json"));
    let chunk_dir = Arc::new(data_dir.join("chunks"));

    std::fs::create_dir_all(&*static_dir).unwrap();
    std::fs::create_dir_all(&*upload_dir).unwrap();
    std::fs::create_dir_all(&*data_dir).unwrap();
    std::fs::create_dir_all(&*chunk_dir).unwrap();

    let admin_key_path = Arc::new(data_dir.join("admin_key.json"));
    let bans_path = Arc::new(data_dir.join("ip_bans.json"));
    let admin_key = Arc::new(RwLock::new(String::from("test_admin_key")));
    let bans = Arc::new(RwLock::new(Vec::<juicebox::state::IpBan>::new()));
    let mailgun_api_key = Some("test_mailgun_api_key".to_string());
    let mailgun_domain = Some("test.mailgun.org".to_string());
    let report_email_to = Some("to@example.com".to_string());
    let report_email_from = Some("from@example.com".to_string());
    let (email_tx, _email_rx) =
        tokio::sync::mpsc::channel::<juicebox::handlers::ReportRecordEmail>(1);
    let email_tx = Some(email_tx);
    // Load templates from the actual templates directory for tests
    let tera = Arc::new(
        tera::Tera::new("templates/**/*.tera").expect("Failed to load templates for tests"),
    );
    let ip_hash_secret = Arc::new(PRIMARY_HASH_SECRET.to_vec());

    let state = AppState {
        upload_dir,
        static_dir,
        metadata_path: metadata_path.clone(),
        owners: Arc::new(dashmap::DashMap::new()),
        upload_sem: Arc::new(Semaphore::new(UPLOAD_CONCURRENCY)),
        production: false,
        last_meta_mtime: Arc::new(RwLock::new(SystemTime::UNIX_EPOCH)),
        reports_path,
        reports: Arc::new(RwLock::new(Vec::<ReportRecord>::new())),
        admin_sessions_path,
        admin_sessions: Arc::new(RwLock::new(HashMap::<String, u64>::new())),
        admin_key_path,
        admin_key,
        bans_path,
        bans,
        mailgun_api_key,
        mailgun_domain,
        report_email_to,
        report_email_from,
        email_tx,
        tera,
        chunk_dir,
        chunk_sessions: Arc::new(dashmap::DashMap::new()),
        ip_hash_secret,
    };

    (state, temp_dir)
}

#[allow(dead_code)]
pub fn recreate_state(base_path: &Path) -> AppState {
    let static_dir = Arc::new(base_path.join("public"));
    let upload_dir = Arc::new(base_path.join("files"));
    let data_dir = Arc::new(base_path.join("data"));
    let metadata_path = Arc::new(data_dir.join("file_owners.json"));
    let reports_path = Arc::new(data_dir.join("reports.json"));
    let admin_sessions_path = Arc::new(data_dir.join("admin_sessions.json"));
    let admin_key_path = Arc::new(data_dir.join("admin_key.json"));
    let bans_path = Arc::new(data_dir.join("ip_bans.json"));
    let chunk_dir = Arc::new(data_dir.join("chunks"));

    std::fs::create_dir_all(&*static_dir).unwrap();
    std::fs::create_dir_all(&*upload_dir).unwrap();
    std::fs::create_dir_all(&*data_dir).unwrap();
    std::fs::create_dir_all(&*chunk_dir).unwrap();

    let admin_key = Arc::new(RwLock::new(String::new()));
    let bans = Arc::new(RwLock::new(Vec::<juicebox::state::IpBan>::new()));
    let (email_tx, _email_rx) =
        tokio::sync::mpsc::channel::<juicebox::handlers::ReportRecordEmail>(1);
    let tera = Arc::new(
        tera::Tera::new("templates/**/*.tera").expect("Failed to load templates for tests"),
    );
    let ip_hash_secret = Arc::new(SECONDARY_HASH_SECRET.to_vec());

    AppState {
        upload_dir,
        static_dir,
        metadata_path: metadata_path.clone(),
        owners: Arc::new(dashmap::DashMap::new()),
        upload_sem: Arc::new(Semaphore::new(UPLOAD_CONCURRENCY)),
        production: false,
        last_meta_mtime: Arc::new(RwLock::new(SystemTime::UNIX_EPOCH)),
        reports_path,
        reports: Arc::new(RwLock::new(Vec::<ReportRecord>::new())),
        admin_sessions_path,
        admin_sessions: Arc::new(RwLock::new(HashMap::<String, u64>::new())),
        admin_key_path,
        admin_key,
        bans_path,
        bans,
        mailgun_api_key: None,
        mailgun_domain: None,
        report_email_to: None,
        report_email_from: None,
        email_tx: Some(email_tx),
        tera,
        chunk_dir,
        chunk_sessions: Arc::new(dashmap::DashMap::new()),
        ip_hash_secret,
    }
}
