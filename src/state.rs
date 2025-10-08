use crate::util::{
    hash_ip_addr, hash_ip_string, hash_network_from_cidr, hash_network_from_ip, new_id, now_secs,
    ttl_to_duration, IpVersion, ADMIN_KEY_TTL, ADMIN_SESSION_TTL,
};
use anyhow::Result;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::SystemTime};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, RwLock, Semaphore};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileMeta {
    #[serde(alias = "owner")]
    pub owner_hash: String,
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
    #[serde(alias = "ip")]
    pub reporter_hash: String,
    pub time: u64,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AdminKeyFile {
    pub key: String,
    pub expires: u64,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum BanSubject {
    Exact {
        hash: String,
    },
    Network {
        hash: String,
        prefix: u8,
        version: IpVersion,
    },
}

impl BanSubject {
    pub fn key(&self) -> &str {
        match self {
            BanSubject::Exact { hash } => hash,
            BanSubject::Network { hash, .. } => hash,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct IpBan {
    pub subject: BanSubject,
    #[serde(default)]
    pub label: Option<String>,
    pub reason: String,
    pub time: u64,
}

#[derive(Debug)]
pub struct ChunkSession {
    pub owner_hash: String,
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
    pub persist_lock: Mutex<()>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ChunkSessionRecord {
    #[serde(alias = "owner")]
    owner_hash: String,
    original_name: String,
    storage_name: String,
    ttl_code: String,
    expires: u64,
    total_bytes: u64,
    chunk_size: u64,
    total_chunks: u32,
    hash: Option<String>,
    created: u64,
    received: Vec<bool>,
    completed: bool,
    last_update: u64,
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

    async fn snapshot(&self) -> ChunkSessionRecord {
        let received = self.received.read().await.clone();
        ChunkSessionRecord {
            owner_hash: self.owner_hash.clone(),
            original_name: self.original_name.clone(),
            storage_name: self.storage_name.clone(),
            ttl_code: self.ttl_code.clone(),
            expires: self.expires,
            total_bytes: self.total_bytes,
            chunk_size: self.chunk_size,
            total_chunks: self.total_chunks,
            hash: self.hash.clone(),
            created: self.created,
            received,
            completed: self.completed.load(Ordering::Relaxed),
            last_update: self.last_update.load(Ordering::Relaxed),
        }
    }

    fn from_record(record: ChunkSessionRecord, dir: PathBuf) -> Self {
        Self {
            owner_hash: record.owner_hash,
            original_name: record.original_name,
            storage_name: record.storage_name,
            ttl_code: record.ttl_code,
            expires: record.expires,
            total_bytes: record.total_bytes,
            chunk_size: record.chunk_size,
            total_chunks: record.total_chunks,
            hash: record.hash,
            storage_dir: Arc::new(dir),
            created: record.created,
            received: RwLock::new(record.received),
            completed: AtomicBool::new(record.completed),
            last_update: AtomicU64::new(record.last_update),
            persist_lock: Mutex::new(()),
        }
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
    pub ip_hash_secret: Arc<Vec<u8>>,
}

impl AppState {
    fn ip_hash_secret_bytes(&self) -> &[u8] {
        self.ip_hash_secret.as_ref()
    }

    pub fn hash_ip(&self, ip: &str) -> Option<(IpVersion, String)> {
        hash_ip_string(self.ip_hash_secret_bytes(), ip)
    }

    pub fn hash_ip_to_string(&self, ip: &str) -> Option<String> {
        self.hash_ip(ip).map(|(_, hash)| hash)
    }

    pub fn hash_ip_addr(&self, addr: &IpAddr) -> (IpVersion, String) {
        hash_ip_addr(self.ip_hash_secret_bytes(), addr)
    }

    pub fn hash_network_for_ip(
        &self,
        addr: &IpAddr,
        prefix: u8,
    ) -> Option<(IpVersion, u8, String)> {
        hash_network_from_ip(self.ip_hash_secret_bytes(), addr, prefix)
    }

    pub fn hash_network_from_cidr(&self, cidr: &str) -> Option<(IpVersion, u8, String)> {
        hash_network_from_cidr(self.ip_hash_secret_bytes(), cidr)
    }

    pub fn ban_subject_from_input(&self, input: &str) -> Option<BanSubject> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Some((version, prefix, hash)) = self.hash_network_from_cidr(trimmed) {
            return Some(BanSubject::Network {
                hash,
                prefix,
                version,
            });
        }
        if let Ok(addr) = trimmed.parse::<IpAddr>() {
            let (_, hash) = self.hash_ip_addr(&addr);
            return Some(BanSubject::Exact { hash });
        }
        Some(BanSubject::Exact {
            hash: trimmed.to_string(),
        })
    }

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

    pub async fn persist_chunk_session(&self, _id: &str, session: &ChunkSession) -> Result<()> {
        let _guard = session.persist_lock.lock().await;
        let snapshot = session.snapshot().await;
        let json = serde_json::to_vec(&snapshot)?;
        let path = session.storage_dir.join("session.json");
        let tmp = path.with_extension("tmp");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let mut file = fs::File::create(&tmp).await?;
        file.write_all(&json).await?;
        file.sync_all().await?;
        fs::rename(&tmp, &path).await?;
        Ok(())
    }

    pub async fn load_chunk_sessions_from_disk(&self) -> Result<()> {
        let mut dirs = match fs::read_dir(&*self.chunk_dir).await {
            Ok(d) => d,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.into()),
        };
        while let Some(entry) = dirs.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            let meta_path = entry.path().join("session.json");
            let bytes = match fs::read(&meta_path).await {
                Ok(bytes) => bytes,
                Err(err) => {
                    tracing::warn!(?err, session_id = %id, "chunk session metadata missing; removing directory");
                    let _ = fs::remove_dir_all(entry.path()).await;
                    continue;
                }
            };
            match serde_json::from_slice::<ChunkSessionRecord>(&bytes) {
                Ok(record) => {
                    let session = Arc::new(ChunkSession::from_record(record, entry.path()));
                    self.chunk_sessions.insert(id, session);
                }
                Err(err) => {
                    tracing::warn!(?err, session_id = %id, "failed to parse chunk session metadata; removing directory");
                    let _ = fs::remove_dir_all(entry.path()).await;
                }
            }
        }
        Ok(())
    }

    pub async fn persist_all_chunk_sessions(&self) {
        let sessions: Vec<(String, Arc<ChunkSession>)> = self
            .chunk_sessions
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();
        for (id, session) in sessions {
            if let Err(err) = self.persist_chunk_session(&id, &session).await {
                tracing::warn!(?err, session_id = %id, "failed to persist chunk session");
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
        let parsed_ip = ip.parse::<IpAddr>().ok();
        let mut ip_hash = None;
        if let Some(addr) = parsed_ip.as_ref() {
            let (_, hash) = self.hash_ip_addr(addr);
            ip_hash = Some(hash);
        }
        let direct_hash_input = if parsed_ip.is_none() { Some(ip) } else { None };

        let bans = self.bans.read().await;
        for ban in bans.iter() {
            match &ban.subject {
                BanSubject::Exact { hash } => {
                    if ip_hash.as_ref().is_some_and(|candidate| candidate == hash) {
                        return true;
                    }
                    if direct_hash_input.is_some_and(|candidate| candidate == hash) {
                        return true;
                    }
                }
                BanSubject::Network {
                    hash,
                    prefix,
                    version,
                } => {
                    if let Some(addr) = parsed_ip.as_ref() {
                        if let Some((net_version, _, candidate)) =
                            self.hash_network_for_ip(addr, *prefix)
                        {
                            if net_version == *version && &candidate == hash {
                                return true;
                            }
                        }
                    }
                    if direct_hash_input.is_some_and(|candidate| candidate == hash) {
                        return true;
                    }
                }
            }
        }
        false
    }
    pub async fn add_ban(&self, mut ban: IpBan) {
        if ban.time == 0 {
            ban.time = now_secs();
        }
        let key = ban.subject.key().to_string();
        let mut bans = self.bans.write().await;
        if bans.iter().any(|b| b.subject.key() == key) {
            return;
        }
        bans.push(ban);
    }
    pub async fn remove_ban(&self, key: &str) {
        let mut bans = self.bans.write().await;
        bans.retain(|b| b.subject.key() != key);
    }

    pub async fn find_ban_for_input(&self, input: &str) -> Option<IpBan> {
        let parsed_ip = input.parse::<IpAddr>().ok();
        let ip_hash = parsed_ip.as_ref().map(|addr| self.hash_ip_addr(addr).1);
        let direct_value = if parsed_ip.is_none() {
            Some(input.to_string())
        } else {
            None
        };
        let bans = self.bans.read().await;
        for ban in bans.iter() {
            match &ban.subject {
                BanSubject::Exact { hash } => {
                    if ip_hash.as_ref().is_some_and(|candidate| candidate == hash) {
                        return Some(ban.clone());
                    }
                    if direct_value
                        .as_ref()
                        .is_some_and(|candidate| candidate == hash)
                    {
                        return Some(ban.clone());
                    }
                }
                BanSubject::Network {
                    hash,
                    prefix,
                    version,
                } => {
                    if let Some(addr) = parsed_ip.as_ref() {
                        if let Some((net_version, _, candidate)) =
                            self.hash_network_for_ip(addr, *prefix)
                        {
                            if net_version == *version && &candidate == hash {
                                return Some(ban.clone());
                            }
                        }
                    }
                    if direct_value
                        .as_ref()
                        .is_some_and(|candidate| candidate == hash)
                    {
                        return Some(ban.clone());
                    }
                }
            }
        }
        None
    }

    pub async fn remove_chunk_session(&self, id: &str) {
        if let Some((_, session)) = self.chunk_sessions.remove(id) {
            let dir = session.storage_dir.clone();
            if let Err(err) = fs::remove_dir_all(&*dir).await {
                tracing::warn!(?err, path = ?dir, "failed to remove chunk session directory");
            }
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
                        owner_hash: v,
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
pub async fn verify_user_entries(state: &AppState, owner_hash: &str) {
    let _ = verify_user_entries_with_report(state, owner_hash).await;
} // Simplified: delegate to the already tested reconcile implementation, ignore its report.

#[derive(serde::Serialize)]
pub struct ReconcileReport {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub updated: Vec<String>,
}

pub async fn verify_user_entries_with_report(
    state: &AppState,
    owner_hash: &str,
) -> Option<ReconcileReport> {
    if let Ok(bytes) = fs::read(&*state.metadata_path).await {
        if let Ok(disk_map) = serde_json::from_slice::<HashMap<String, FileMeta>>(&bytes) {
            let mut to_remove = Vec::new();
            let mut to_update = Vec::new();
            let mut to_add = Vec::new();
            for entry in state.owners.iter() {
                let (fname, meta_mem) = (entry.key(), entry.value());
                if meta_mem.owner_hash == owner_hash {
                    match disk_map.get(fname) {
                        Some(meta_disk) => {
                            if meta_disk.owner_hash != meta_mem.owner_hash
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
                if meta_disk.owner_hash == owner_hash && state.owners.get(fname).is_none() {
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
