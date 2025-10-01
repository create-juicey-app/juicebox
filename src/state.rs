use crate::util::{ADMIN_KEY_TTL, ADMIN_SESSION_TTL, new_id, now_secs, ttl_to_duration};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::SystemTime};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::{RwLock, Semaphore};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileMeta {
    pub owner: String,
    pub expires: u64,
    #[serde(default)]
    pub original: String,
    #[serde(default = "now_secs")]
    pub created: u64,
    pub hash: String,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ReportRecord {
    pub file: String,
    pub reason: String,
    pub details: String,
    pub ip: String,
    pub time: u64,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AdminKeyFile {
    pub key: String,
    pub expires: u64,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct IpBan {
    pub ip: String,
    pub reason: String,
    pub time: u64,
}

#[derive(Debug)]
pub struct ChunkSession {
    pub owner: String,
    pub original_name: String,
    pub storage_name: String,
    pub ttl_code: String,
    pub expires: u64,
    pub total_bytes: u64,
    pub chunk_size: u64,
    pub total_chunks: u32,
    pub hash: Option<String>,
    pub storage_dir: Arc<PathBuf>,
    pub created: u64,
    pub received: RwLock<Vec<bool>>,
    pub completed: AtomicBool,
    pub last_update: AtomicU64,
}

impl ChunkSession {
    pub fn touch(&self) {
        self.last_update.store(now_secs(), Ordering::Relaxed);
    }

    pub fn mark_completed(&self) {
        self.completed.store(true, Ordering::Relaxed);
        self.touch();
    }

    pub fn is_completed(&self) -> bool {
        self.completed.load(Ordering::Relaxed)
    }
}

#[derive(Clone)]
pub struct AppState {
    pub upload_dir: Arc<PathBuf>,
    pub static_dir: Arc<PathBuf>,
    pub metadata_path: Arc<PathBuf>,
    pub owners: Arc<DashMap<String, FileMeta>>,
    pub upload_sem: Arc<Semaphore>,
    pub production: bool,
    pub last_meta_mtime: Arc<RwLock<SystemTime>>,
    pub reports_path: Arc<PathBuf>,
    pub reports: Arc<RwLock<Vec<ReportRecord>>>,
    pub admin_sessions_path: Arc<PathBuf>,
    pub admin_sessions: Arc<RwLock<HashMap<String, u64>>>,
    pub admin_key_path: Arc<PathBuf>,
    pub admin_key: Arc<RwLock<String>>,
    pub bans_path: Arc<PathBuf>,
    pub bans: Arc<RwLock<Vec<IpBan>>>,
    // email notification config
    pub mailgun_api_key: Option<String>,
    pub mailgun_domain: Option<String>,
    pub report_email_to: Option<String>,
    pub report_email_from: Option<String>,
    pub email_tx: Option<tokio::sync::mpsc::Sender<crate::handlers::ReportRecordEmail>>, // channel to worker
    pub tera: std::sync::Arc<tera::Tera>,
    pub chunk_dir: Arc<PathBuf>,
    pub chunk_sessions: Arc<DashMap<String, Arc<ChunkSession>>>,
}

impl AppState {
    pub async fn persist_owners(&self) {
        // DashMap is not directly serializable, so collect to HashMap
        let owners: HashMap<String, FileMeta> = self
            .owners
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect();
        if let Ok(json) = serde_json::to_vec(&owners) {
            let tmp = self.metadata_path.with_extension("tmp");
            if let Ok(mut f) = fs::File::create(&tmp).await {
                if f.write_all(&json).await.is_ok() {
                    let _ = f.sync_all().await;
                    let _ = fs::rename(&tmp, &*self.metadata_path).await;
                    if let Ok(md) = fs::metadata(&*self.metadata_path).await {
                        if let Ok(modified) = md.modified() {
                            let mut lm = self.last_meta_mtime.write().await;
                            *lm = modified;
                        }
                    }
                }
            }
        }
    }
    pub async fn persist_reports(&self) {
        let reports = self.reports.read().await;
        if let Ok(json) = serde_json::to_vec(&*reports) {
            let tmp = self.reports_path.with_extension("tmp");
            if let Ok(mut f) = fs::File::create(&tmp).await {
                if f.write_all(&json).await.is_ok() {
                    let _ = f.sync_all().await;
                    let _ = fs::rename(&tmp, &*self.reports_path).await;
                }
            }
        }
    }
    pub async fn persist_admin_sessions(&self) {
        let map = self.admin_sessions.read().await;
        if let Ok(json) = serde_json::to_vec(&*map) {
            let tmp = self.admin_sessions_path.with_extension("tmp");
            if let Ok(mut f) = fs::File::create(&tmp).await {
                if f.write_all(&json).await.is_ok() {
                    let _ = f.sync_all().await;
                    let _ = fs::rename(&tmp, &*self.admin_sessions_path).await;
                }
            }
        }
    }
    pub async fn persist_bans(&self) {
        let bans = self.bans.read().await;
        if let Ok(json) = serde_json::to_vec(&*bans) {
            let tmp = self.bans_path.with_extension("tmp");
            if let Ok(mut f) = fs::File::create(&tmp).await {
                if f.write_all(&json).await.is_ok() {
                    let _ = f.sync_all().await;
                    let _ = fs::rename(&tmp, &*self.bans_path).await;
                }
            }
        }
    }
    pub async fn is_admin(&self, token: &str) -> bool {
        let map = self.admin_sessions.read().await;
        if let Some(exp) = map.get(token) {
            if *exp > now_secs() {
                return true;
            }
        }
        false
    }
    pub async fn create_admin_session(&self, token: String) {
        let mut map = self.admin_sessions.write().await;
        map.insert(token, now_secs() + ADMIN_SESSION_TTL);
    }
    pub async fn cleanup_admin_sessions(&self) {
        let mut map = self.admin_sessions.write().await;
        let now = now_secs();
        map.retain(|_, exp| *exp > now);
    }
    pub async fn load_or_create_admin_key(&self, path: &PathBuf) -> anyhow::Result<AdminKeyFile> {
        if let Ok(bytes) = fs::read(path).await {
            if let Ok(parsed) = serde_json::from_slice::<AdminKeyFile>(&bytes) {
                if parsed.expires > now_secs() && !parsed.key.is_empty() {
                    return Ok(parsed);
                }
            }
        }
        // Need to create / rotate
        let new = AdminKeyFile {
            key: new_id(),
            expires: now_secs() + ADMIN_KEY_TTL,
        };
        let json = serde_json::to_vec_pretty(&new)?;
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent).await;
        }
        fs::write(path, json).await?;
        Ok(new)
    }
    pub async fn is_banned(&self, ip: &str) -> bool {
        let bans = self.bans.read().await;
        bans.iter().any(|b| b.ip == ip)
    }
    pub async fn add_ban(&self, ip: String, reason: String) {
        let mut bans = self.bans.write().await;
        if !bans.iter().any(|b| b.ip == ip) {
            bans.push(IpBan {
                ip,
                reason,
                time: now_secs(),
            });
        }
    }
    pub async fn remove_ban(&self, ip: &str) {
        let mut bans = self.bans.write().await;
        bans.retain(|b| b.ip != ip);
    }

    pub async fn remove_chunk_session(&self, id: &str) {
        if let Some((_, session)) = self.chunk_sessions.remove(id) {
            let dir = session.storage_dir.clone();
            let _ = fs::remove_dir_all(&*dir).await;
        }
    }

    pub async fn cleanup_chunk_sessions(&self) {
        const STALE_GRACE: u64 = 30 * 60; // 30 minutes
        let now = now_secs();
        let mut expired_ids = Vec::new();
        for entry in self.chunk_sessions.iter() {
            let session = entry.value();
            let expired = session.expires <= now;
            let idle = session
                .last_update
                .load(Ordering::Relaxed)
                .saturating_add(STALE_GRACE)
                <= now;
            if expired || idle {
                expired_ids.push(entry.key().clone());
            }
        }
        for id in expired_ids {
            self.remove_chunk_session(&id).await;
        }
    }
}

pub async fn check_storage_integrity(state: &AppState) {
    let mut to_remove = Vec::new();
    for entry in state.owners.iter() {
        let fname = entry.key();
        if !state.upload_dir.join(fname).exists() {
            to_remove.push(fname.clone());
        }
    }
    if to_remove.is_empty() {
        return;
    }
    for f in &to_remove {
        state.owners.remove(f);
    }
    state.persist_owners().await;
}
pub fn spawn_integrity_check(state: AppState) {
    tokio::spawn(async move {
        check_storage_integrity(&state).await;
    });
}

pub async fn cleanup_expired(state: &AppState) {
    let now = now_secs();
    let mut to_delete = Vec::new();
    for entry in state.owners.iter() {
        let (file, meta) = (entry.key(), entry.value());
        if meta.expires <= now {
            to_delete.push(file.clone());
        }
    }
    if to_delete.is_empty() {
        return;
    }
    for f in &to_delete {
        state.owners.remove(f);
    }
    for f in &to_delete {
        let _ = fs::remove_file(state.upload_dir.join(f)).await;
    }
    state.persist_owners().await;
}

pub async fn reload_metadata_if_changed(state: &AppState) {
    let meta_res = fs::metadata(&*state.metadata_path).await;
    let md = match meta_res {
        Ok(m) => m,
        Err(_) => return,
    };
    let modified = match md.modified() {
        Ok(t) => t,
        Err(_) => return,
    };
    let need_reload = {
        let lm = state.last_meta_mtime.read().await;
        modified > *lm
    };
    if !need_reload {
        return;
    }
    if let Ok(bytes) = fs::read(&*state.metadata_path).await {
        if let Ok(map) = serde_json::from_slice::<HashMap<String, FileMeta>>(&bytes) {
            state.owners.clear();
            for (k, v) in map.into_iter() {
                state.owners.insert(k, v);
            }
        } else if let Ok(old) = serde_json::from_slice::<HashMap<String, String>>(&bytes) {
            state.owners.clear();
            let default_exp = now_secs() + ttl_to_duration("3d").as_secs();
            for (k, v) in old.into_iter() {
                state.owners.insert(
                    k,
                    FileMeta {
                        owner: v,
                        expires: default_exp,
                        original: String::new(),
                        created: now_secs(),
                        hash: String::new(),
                    },
                );
            }
        }
        let mut lm = state.last_meta_mtime.write().await;
        *lm = modified;
    }
}

// Simplified: delegate to the already tested reconcile implementation, ignore its report.
pub async fn verify_user_entries(state: &AppState, ip: &str) {
    let _ = verify_user_entries_with_report(state, ip).await;
} // Simplified: delegate to the already tested reconcile implementation, ignore its report.

#[derive(serde::Serialize)]
pub struct ReconcileReport {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub updated: Vec<String>,
}

pub async fn verify_user_entries_with_report(
    state: &AppState,
    ip: &str,
) -> Option<ReconcileReport> {
    if let Ok(bytes) = fs::read(&*state.metadata_path).await {
        if let Ok(disk_map) = serde_json::from_slice::<HashMap<String, FileMeta>>(&bytes) {
            let mut to_remove = Vec::new();
            let mut to_update = Vec::new();
            let mut to_add = Vec::new();
            for entry in state.owners.iter() {
                let (fname, meta_mem) = (entry.key(), entry.value());
                if meta_mem.owner == ip {
                    match disk_map.get(fname) {
                        Some(meta_disk) => {
                            if meta_disk.owner != meta_mem.owner
                                || meta_disk.expires != meta_mem.expires
                                || meta_disk.original != meta_mem.original
                            {
                                to_update.push((fname.clone(), meta_disk.clone()));
                            }
                        }
                        _none => to_remove.push(fname.clone()),
                    }
                }
            }
            for (fname, meta_disk) in disk_map.iter() {
                if meta_disk.owner == ip && state.owners.get(fname).is_none() {
                    to_add.push((fname.clone(), meta_disk.clone()));
                }
            }
            if to_remove.is_empty() && to_update.is_empty() && to_add.is_empty() {
                return None;
            }
            for f in &to_remove {
                state.owners.remove(f);
            }
            for (f, m) in &to_update {
                state.owners.insert(f.clone(), m.clone());
            }
            for (f, m) in &to_add {
                state.owners.insert(f.clone(), m.clone());
            }
            return Some(ReconcileReport {
                added: to_add.into_iter().map(|(f, _)| f).collect(),
                removed: to_remove,
                updated: to_update.into_iter().map(|(f, _)| f).collect(),
            });
        }
    }
    None
}
