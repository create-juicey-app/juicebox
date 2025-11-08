use crate::util::{
    ADMIN_KEY_TTL, ADMIN_SESSION_TTL, IpVersion, MAX_ACTIVE_FILES_PER_IP, hash_ip_addr,
    hash_ip_string, hash_network_from_cidr, hash_network_from_ip, new_id, now_secs,
};
use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::SystemTime};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, RwLock, Semaphore};
use tracing::{debug, error, info, trace, warn};

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

#[async_trait]
pub trait KvStore: Send + Sync {
    async fn replace_hash(&self, key: &str, entries: &[(String, String)]) -> Result<()>;
    async fn load_hash(&self, key: &str) -> Result<Vec<(String, String)>>;
    async fn replace_list(&self, key: &str, values: &[String]) -> Result<()>;
    async fn load_list(&self, key: &str) -> Result<Vec<String>>;
}

pub struct RedisStore {
    prefix: String,
    manager: Arc<Mutex<ConnectionManager>>,
}

impl RedisStore {
    pub fn new(prefix: String, manager: Arc<Mutex<ConnectionManager>>) -> Self {
        Self { prefix, manager }
    }

    fn key(&self, suffix: &str) -> String {
        format!("{}:{suffix}", self.prefix)
    }
}

#[async_trait]
impl KvStore for RedisStore {
    async fn replace_hash(&self, key: &str, entries: &[(String, String)]) -> Result<()> {
        let redis_key = self.key(key);
        let mut conn = self.manager.lock().await;
        redis::cmd("DEL")
            .arg(&redis_key)
            .query_async::<_, ()>(&mut *conn)
            .await?;
        if !entries.is_empty() {
            let mut cmd = redis::cmd("HSET");
            cmd.arg(&redis_key);
            for (field, value) in entries {
                cmd.arg(field).arg(value);
            }
            cmd.query_async::<_, ()>(&mut *conn).await?;
        }
        Ok(())
    }

    async fn load_hash(&self, key: &str) -> Result<Vec<(String, String)>> {
        let redis_key = self.key(key);
        let mut conn = self.manager.lock().await;
        let entries: Vec<(String, String)> = conn.hgetall(&redis_key).await?;
        Ok(entries)
    }

    async fn replace_list(&self, key: &str, values: &[String]) -> Result<()> {
        let redis_key = self.key(key);
        let mut conn = self.manager.lock().await;
        redis::cmd("DEL")
            .arg(&redis_key)
            .query_async::<_, ()>(&mut *conn)
            .await?;
        if !values.is_empty() {
            let mut pipe = redis::pipe();
            pipe.atomic();
            for value in values {
                pipe.cmd("RPUSH").arg(&redis_key).arg(value);
            }
            pipe.query_async::<_, ()>(&mut *conn).await?;
        }
        Ok(())
    }

    async fn load_list(&self, key: &str) -> Result<Vec<String>> {
        let redis_key = self.key(key);
        let mut conn = self.manager.lock().await;
        let entries: Vec<String> = conn.lrange(&redis_key, 0, -1).await?;
        Ok(entries)
    }
}

#[derive(Default)]
pub struct MemoryStore {
    prefix: String,
    hashes: Mutex<HashMap<String, HashMap<String, String>>>,
    lists: Mutex<HashMap<String, Vec<String>>>,
}

impl MemoryStore {
    pub fn new(prefix: String) -> Self {
        Self {
            prefix,
            hashes: Mutex::new(HashMap::new()),
            lists: Mutex::new(HashMap::new()),
        }
    }

    fn key(&self, suffix: &str) -> String {
        format!("{}:{suffix}", self.prefix)
    }
}

#[async_trait]
impl KvStore for MemoryStore {
    async fn replace_hash(&self, key: &str, entries: &[(String, String)]) -> Result<()> {
        let redis_key = self.key(key);
        let mut hashes = self.hashes.lock().await;
        let mut map = HashMap::new();
        for (field, value) in entries {
            map.insert(field.clone(), value.clone());
        }
        hashes.insert(redis_key, map);
        Ok(())
    }

    async fn load_hash(&self, key: &str) -> Result<Vec<(String, String)>> {
        let redis_key = self.key(key);
        let hashes = self.hashes.lock().await;
        let entries = hashes
            .get(&redis_key)
            .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        Ok(entries)
    }

    async fn replace_list(&self, key: &str, values: &[String]) -> Result<()> {
        let redis_key = self.key(key);
        let mut lists = self.lists.lock().await;
        lists.insert(redis_key, values.to_vec());
        Ok(())
    }

    async fn load_list(&self, key: &str) -> Result<Vec<String>> {
        let redis_key = self.key(key);
        let lists = self.lists.lock().await;
        Ok(lists.get(&redis_key).cloned().unwrap_or_default())
    }
}

#[derive(Clone, Debug)]
pub struct TelemetryState {
    pub sentry_dsn: Option<String>,
    pub release: String,
    pub environment: String,
    pub traces_sample_rate: f32,
    pub error_sample_rate: f32,
    pub trace_propagation_targets: Vec<String>,
}

impl TelemetryState {
    #[inline]
    pub fn sentry_enabled(&self) -> bool {
        self.sentry_dsn
            .as_ref()
            .map(|dsn| !dsn.trim().is_empty())
            .unwrap_or(false)
    }

    #[inline]
    pub fn sentry_connect_origin(&self) -> Option<String> {
        let dsn = self.sentry_dsn.as_ref()?.trim();
        if dsn.is_empty() {
            return None;
        }
        let (scheme, rest) = dsn.split_once("://")?;
        let host_part = rest.split('@').nth(1).unwrap_or(rest);
        let host = host_part.split('/').next().unwrap_or(host_part).trim();
        if host.is_empty() {
            return None;
        }
        Some(format!("{}://{}", scheme, host))
    }
}

#[cfg(test)]
mod telemetry_tests {
    use super::TelemetryState;

    #[test]
    fn parses_standard_sentry_dsn() {
        let state = TelemetryState {
            sentry_dsn: Some("https://123@example.ingest.sentry.io/4500000000000000".into()),
            release: "rel".into(),
            environment: "env".into(),
            traces_sample_rate: 1.0,
            error_sample_rate: 1.0,
            trace_propagation_targets: Vec::new(),
        };
        assert_eq!(
            state.sentry_connect_origin().as_deref(),
            Some("https://example.ingest.sentry.io")
        );
    }

    #[test]
    fn returns_none_for_empty_dsn() {
        let state = TelemetryState {
            sentry_dsn: Some(" ".into()),
            release: String::new(),
            environment: String::new(),
            traces_sample_rate: 0.0,
            error_sample_rate: 0.0,
            trace_propagation_targets: Vec::new(),
        };
        assert!(state.sentry_connect_origin().is_none());
    }

    #[test]
    fn handles_missing_credentials() {
        let state = TelemetryState {
            sentry_dsn: Some("https://o123.ingest.sentry.io/1".into()),
            release: String::new(),
            environment: String::new(),
            traces_sample_rate: 0.0,
            error_sample_rate: 0.0,
            trace_propagation_targets: Vec::new(),
        };
        assert_eq!(
            state.sentry_connect_origin().as_deref(),
            Some("https://o123.ingest.sentry.io")
        );
    }
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
    pub assembled_chunks: AtomicU32,
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
    #[serde(default)]
    assembled_chunks: u32,
}

impl ChunkSession {
    pub fn touch(&self) {
        self.last_update.store(now_secs(), Ordering::Relaxed);
    }

    pub fn mark_completed(&self) {
        self.completed.store(true, Ordering::Relaxed);
        self.assembled_chunks
            .store(self.total_chunks, Ordering::Relaxed);
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
            assembled_chunks: self.assembled_chunks.load(Ordering::Relaxed),
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
            assembled_chunks: AtomicU32::new(record.assembled_chunks),
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
    pub owners_persist_lock: Arc<Mutex<()>>,
    pub telemetry: Arc<TelemetryState>,
    pub kv: Arc<dyn KvStore>,
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

    #[tracing::instrument(level = "trace", skip(self))]
    pub fn active_file_count(&self, owner_hash: &str, now: u64) -> usize {
        let count = self
            .owners
            .iter()
            .filter(|entry| {
                let meta = entry.value();
                meta.owner_hash.as_str() == owner_hash && meta.expires > now
            })
            .count();
        trace!(owner_hash, count, "active file count computed");
        count
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub fn pending_chunk_count(&self, owner_hash: &str) -> usize {
        let count = self
            .chunk_sessions
            .iter()
            .filter(|entry| {
                let session = entry.value();
                session.owner_hash.as_str() == owner_hash && !session.is_completed()
            })
            .count();
        trace!(owner_hash, count, "pending chunk count computed");
        count
    }

    #[tracing::instrument(level = "debug", skip(self))]
    pub fn reserved_file_slots(&self, owner_hash: &str, now: u64) -> usize {
        let reserved =
            self.active_file_count(owner_hash, now) + self.pending_chunk_count(owner_hash);
        debug!(owner_hash, reserved, "reserved file slots computed");
        reserved
    }

    #[tracing::instrument(level = "debug", skip(self))]
    pub fn remaining_file_slots(&self, owner_hash: &str, now: u64) -> usize {
        let remaining =
            MAX_ACTIVE_FILES_PER_IP.saturating_sub(self.reserved_file_slots(owner_hash, now));
        debug!(owner_hash, remaining, "remaining file slots computed");
        remaining
    }

    #[tracing::instrument(level = "debug", skip(self))]
    async fn persist_owners_inner(&self) {
        let owners: HashMap<String, FileMeta> = self
            .owners
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect();
        let mut encoded = Vec::with_capacity(owners.len());
        for (key, meta) in owners.iter() {
            match serde_json::to_string(meta) {
                Ok(value) => encoded.push((key.clone(), value)),
                Err(err) => {
                    error!(?err, file = key, "failed to serialize file meta for redis");
                    return;
                }
            }
        }
        if let Err(err) = self.kv.replace_hash("owners", &encoded).await {
            error!(?err, "failed to persist owners metadata to key-value store");
            return;
        }
        debug!(
            count = encoded.len(),
            "persisted owners metadata to key-value store"
        );
    }

    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn persist_owners(&self) {
        let _guard = self.owners_persist_lock.lock().await;
        self.persist_owners_inner().await;
    }

    #[tracing::instrument(level = "debug", skip(self))]
    pub fn spawn_persist_owners(&self) {
        let state = self.clone();
        tokio::spawn(async move {
            state.persist_owners().await;
        });
    }
    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn persist_reports(&self) {
        let reports = self.reports.read().await.clone();
        let mut encoded = Vec::with_capacity(reports.len());
        for report in reports.iter() {
            match serde_json::to_string(report) {
                Ok(value) => encoded.push(value),
                Err(err) => {
                    error!(?err, "failed to serialize report for redis");
                    return;
                }
            }
        }
        if let Err(err) = self.kv.replace_list("reports", &encoded).await {
            error!(?err, "failed to persist reports to key-value store");
            return;
        }
        debug!(
            count = encoded.len(),
            "persisted reports to key-value store"
        );
    }
    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn persist_admin_sessions(&self) {
        let map_snapshot = self.admin_sessions.read().await.clone();
        let entries: Vec<(String, String)> = map_snapshot
            .iter()
            .map(|(token, exp)| (token.clone(), exp.to_string()))
            .collect();
        if let Err(err) = self.kv.replace_hash("admin_sessions", &entries).await {
            error!(?err, "failed to persist admin sessions to key-value store");
            return;
        }
        debug!(
            count = map_snapshot.len(),
            "persisted admin sessions to key-value store"
        );
    }
    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn persist_bans(&self) {
        let bans_snapshot = self.bans.read().await.clone();
        let mut encoded = Vec::with_capacity(bans_snapshot.len());
        for ban in bans_snapshot.iter() {
            match serde_json::to_string(ban) {
                Ok(value) => encoded.push((ban.subject.key().to_string(), value)),
                Err(err) => {
                    error!(
                        ?err,
                        key = ban.subject.key(),
                        "failed to serialize ban for redis"
                    );
                    return;
                }
            }
        }
        if let Err(err) = self.kv.replace_hash("bans", &encoded).await {
            error!(?err, "failed to persist bans to key-value store");
            return;
        }
        debug!(count = encoded.len(), "persisted bans to key-value store");
    }

    #[tracing::instrument(level = "debug", skip(self, session))]
    pub async fn persist_chunk_session(&self, id: &str, session: &ChunkSession) -> Result<()> {
        let _guard = session.persist_lock.lock().await;
        let snapshot = session.snapshot().await;
        let json = serde_json::to_vec(&snapshot).map_err(|err| {
            error!(
                ?err,
                session_id = id,
                "failed to serialize chunk session snapshot"
            );
            err
        })?;
        let path = session.storage_dir.join("session.json");
        let tmp = path.with_extension("tmp");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.map_err(|err| {
                error!(?err, session_id = id, dir = ?parent, "failed to create chunk session directory");
                err
            })?;
        }
        let mut file = fs::File::create(&tmp).await.map_err(|err| {
            error!(?err, session_id = id, path = ?tmp, "failed to create chunk session temp file");
            err
        })?;
        file.write_all(&json).await.map_err(|err| {
            error!(?err, session_id = id, path = ?tmp, "failed to write chunk session temp file");
            err
        })?;
        if let Err(err) = file.sync_all().await {
            warn!(?err, session_id = id, path = ?tmp, "failed to sync chunk session temp file");
        }
        fs::rename(&tmp, &path).await.map_err(|err| {
            error!(?err, session_id = id, from = ?tmp, to = ?path, "failed to replace chunk session metadata");
            err
        })?;
        debug!(session_id = id, "persisted chunk session metadata");
        Ok(())
    }

    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn load_chunk_sessions_from_disk(&self) -> Result<()> {
        let mut dirs = match fs::read_dir(&*self.chunk_dir).await {
            Ok(d) => d,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                debug!(dir = ?self.chunk_dir, "chunk session directory missing; skipping load");
                return Ok(());
            }
            Err(err) => {
                error!(?err, dir = ?self.chunk_dir, "failed to read chunk sessions directory");
                return Err(err.into());
            }
        };
        let mut loaded = 0usize;
        while let Some(entry) = dirs.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            let meta_path = entry.path().join("session.json");
            let bytes = match fs::read(&meta_path).await {
                Ok(bytes) => bytes,
                Err(err) => {
                    warn!(?err, session_id = %id, path = ?meta_path, "chunk session metadata missing; removing directory");
                    let _ = fs::remove_dir_all(entry.path()).await;
                    continue;
                }
            };
            match serde_json::from_slice::<ChunkSessionRecord>(&bytes) {
                Ok(record) => {
                    let session = Arc::new(ChunkSession::from_record(record, entry.path()));
                    self.chunk_sessions.insert(id, session);
                    loaded += 1;
                }
                Err(err) => {
                    warn!(?err, session_id = %id, "failed to parse chunk session metadata; removing directory");
                    let _ = fs::remove_dir_all(entry.path()).await;
                }
            }
        }
        debug!(loaded, "restored chunk sessions from disk");
        Ok(())
    }

    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn persist_all_chunk_sessions(&self) {
        let sessions: Vec<(String, Arc<ChunkSession>)> = self
            .chunk_sessions
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();
        let count = sessions.len();
        for (id, session) in sessions {
            if let Err(err) = self.persist_chunk_session(&id, &session).await {
                warn!(?err, session_id = %id, "failed to persist chunk session");
            }
        }
        debug!(count, "persisted chunk sessions snapshot");
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub async fn is_admin(&self, token: &str) -> bool {
        let map = self.admin_sessions.read().await;
        if let Some(exp) = map.get(token) {
            if *exp > now_secs() {
                trace!("admin session valid");
                return true;
            }
        }
        trace!("admin session missing or expired");
        false
    }

    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn create_admin_session(&self, token: String) {
        let mut map = self.admin_sessions.write().await;
        map.insert(token, now_secs() + ADMIN_SESSION_TTL);
        debug!(count = map.len(), "created admin session");
    }

    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn cleanup_admin_sessions(&self) {
        let mut map = self.admin_sessions.write().await;
        let now = now_secs();
        map.retain(|_, exp| *exp > now);
        debug!(remaining = map.len(), "cleaned up admin sessions");
    }
    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn load_or_create_admin_key(&self, path: &PathBuf) -> anyhow::Result<AdminKeyFile> {
        if let Ok(bytes) = fs::read(path).await {
            match serde_json::from_slice::<AdminKeyFile>(&bytes) {
                Ok(parsed) if parsed.expires > now_secs() && !parsed.key.is_empty() => {
                    debug!("loaded existing admin key");
                    return Ok(parsed);
                }
                Ok(_) => {
                    warn!("existing admin key expired or empty; rotating");
                }
                Err(err) => {
                    warn!(?err, "failed to parse admin key file; rotating");
                }
            }
        }
        // Need to create / rotate
        let new = AdminKeyFile {
            key: new_id(),
            expires: now_secs() + ADMIN_KEY_TTL,
        };
        let json = serde_json::to_vec_pretty(&new).map_err(|err| {
            error!(?err, "failed to serialize admin key");
            err
        })?;
        if let Some(parent) = path.parent() {
            if let Err(err) = fs::create_dir_all(parent).await {
                error!(?err, dir = ?parent, "failed to create admin key directory");
            }
        }
        fs::write(path, json).await.map_err(|err| {
            error!(?err, path = ?path, "failed to write admin key file");
            err
        })?;
        info!(expires = new.expires, "generated new admin key");
        Ok(new)
    }

    #[tracing::instrument(level = "debug", skip(self))]
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
        if parsed_ip.is_none() {
            trace!(ip, "ban lookup completed (raw hash)");
        }
        false
    }

    #[tracing::instrument(level = "info", skip(self, ban))]
    pub async fn add_ban(&self, mut ban: IpBan) {
        if ban.time == 0 {
            ban.time = now_secs();
        }
        let key = ban.subject.key().to_string();
        let mut bans = self.bans.write().await;
        if bans.iter().any(|b| b.subject.key() == key) {
            warn!(ban_key = key, "ban already exists; skipping");
            return;
        }
        bans.push(ban);
        info!(ban_key = key, total = bans.len(), "ban added");
    }

    #[tracing::instrument(level = "info", skip(self))]
    pub async fn remove_ban(&self, key: &str) {
        let mut bans = self.bans.write().await;
        bans.retain(|b| b.subject.key() != key);
        info!(ban_key = key, remaining = bans.len(), "ban removed");
    }

    #[tracing::instrument(level = "debug", skip(self))]
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

    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn remove_chunk_session(&self, id: &str) {
        if let Some((_, session)) = self.chunk_sessions.remove(id) {
            let dir = (*session.storage_dir).clone();
            tokio::spawn(async move {
                if let Err(err) = fs::remove_dir_all(&dir).await {
                    warn!(?err, path = ?dir, "failed to remove chunk session directory");
                }
            });
            debug!(session_id = id, "removed chunk session from memory");
        } else {
            trace!(session_id = id, "attempted to remove missing chunk session");
        }
    }

    #[tracing::instrument(level = "debug", skip(self))]
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
        for id in expired_ids.iter() {
            self.remove_chunk_session(id).await;
        }
        let removed = expired_ids.len();
        if removed > 0 {
            info!(removed, "cleaned up stale chunk sessions");
        } else {
            trace!("no stale chunk sessions found");
        }
    }
}

#[tracing::instrument(level = "debug", skip(state))]
pub async fn check_storage_integrity(state: &AppState) {
    let mut to_remove = Vec::new();
    for entry in state.owners.iter() {
        let fname = entry.key();
        if !state.upload_dir.join(fname).exists() {
            to_remove.push(fname.clone());
        }
    }
    if to_remove.is_empty() {
        trace!("storage integrity verified (no missing files)");
        return;
    }
    for f in &to_remove {
        state.owners.remove(f);
    }
    state.persist_owners().await;
    warn!(
        removed = to_remove.len(),
        "removed orphaned metadata entries"
    );
}

#[tracing::instrument(level = "trace", skip(state))]
pub fn spawn_integrity_check(state: AppState) {
    tokio::spawn(async move {
        check_storage_integrity(&state).await;
    });
}

#[tracing::instrument(level = "debug", skip(state))]
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
        trace!("no expired files found");
        return;
    }
    for f in &to_delete {
        state.owners.remove(f);
    }
    for f in &to_delete {
        if let Err(err) = fs::remove_file(state.upload_dir.join(f)).await {
            warn!(?err, file = f, "failed to remove expired file from disk");
        }
    }
    state.persist_owners().await;
    info!(removed = to_delete.len(), "cleanup expired files completed");
}

#[tracing::instrument(level = "debug", skip(state))]
pub async fn reload_metadata_if_changed(state: &AppState) {
    let entries: Vec<(String, String)> = match state.kv.load_hash("owners").await {
        Ok(items) => items,
        Err(err) => {
            error!(?err, "failed to reload owners from key-value store");
            return;
        }
    };
    state.owners.clear();
    let mut restored = 0usize;
    for (file, payload) in entries {
        match serde_json::from_str::<FileMeta>(&payload) {
            Ok(meta) => {
                state.owners.insert(file, meta);
                restored += 1;
            }
            Err(err) => {
                warn!(
                    ?err,
                    file, "failed to deserialize file meta from key-value store"
                );
            }
        }
    }
    debug!(restored, "reloaded owners metadata from key-value store");
}

// Simplified: delegate to the already tested reconcile implementation, ignore its report.
#[tracing::instrument(level = "debug", skip(state))]
pub async fn verify_user_entries(state: &AppState, owner_hash: &str) {
    let _ = verify_user_entries_with_report(state, owner_hash).await;
} // Simplified: delegate to the already tested reconcile implementation, ignore its report.

#[derive(serde::Serialize)]
pub struct ReconcileReport {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub updated: Vec<String>,
}

#[tracing::instrument(level = "debug", skip(state))]
pub async fn verify_user_entries_with_report(
    state: &AppState,
    owner_hash: &str,
) -> Option<ReconcileReport> {
    let entries: Vec<(String, String)> = match state.kv.load_hash("owners").await {
        Ok(items) => items,
        Err(err) => {
            error!(
                ?err,
                "failed to fetch owners from key-value store for reconciliation"
            );
            return None;
        }
    };
    let mut disk_map = HashMap::with_capacity(entries.len());
    for (fname, payload) in entries {
        if let Ok(meta) = serde_json::from_str::<FileMeta>(&payload) {
            disk_map.insert(fname, meta);
        }
    }

    let mut missing_in_store = Vec::new();
    let mut memory_preferred = Vec::new();
    let mut store_applied = Vec::new();
    let mut store_applied_payloads = Vec::new();
    for entry in state.owners.iter() {
        let (fname, meta_mem) = (entry.key(), entry.value());
        if meta_mem.owner_hash != owner_hash {
            continue;
        }
        match disk_map.get(fname) {
            Some(meta_disk) => {
                if meta_disk.owner_hash != meta_mem.owner_hash
                    || meta_disk.expires != meta_mem.expires
                    || meta_disk.original != meta_mem.original
                    || meta_disk.hash != meta_mem.hash
                {
                    if meta_disk.created >= meta_mem.created {
                        store_applied.push(fname.clone());
                        store_applied_payloads.push((fname.clone(), meta_disk.clone()));
                    } else {
                        memory_preferred.push(fname.clone());
                    }
                }
            }
            None => missing_in_store.push(fname.clone()),
        }
    }

    for (fname, meta) in &store_applied_payloads {
        state.owners.insert(fname.clone(), meta.clone());
    }

    let mut restored = Vec::new();
    let mut stale_store = Vec::new();
    for (fname, meta_disk) in disk_map.iter() {
        if meta_disk.owner_hash != owner_hash {
            continue;
        }
        if state.owners.get(fname).is_none() {
            let path = state.upload_dir.join(fname);
            if tokio::fs::metadata(&path).await.is_ok() {
                state.owners.insert(fname.clone(), meta_disk.clone());
                restored.push(fname.clone());
            } else {
                stale_store.push(fname.clone());
            }
        }
    }

    if missing_in_store.is_empty()
        && memory_preferred.is_empty()
        && store_applied.is_empty()
        && restored.is_empty()
        && stale_store.is_empty()
    {
        trace!(owner_hash, "reconciliation found no divergences");
        return None;
    }

    if !missing_in_store.is_empty() || !memory_preferred.is_empty() || !stale_store.is_empty() {
        state.persist_owners().await;
    }

    let report = ReconcileReport {
        added: restored.clone(),
        removed: stale_store.clone(),
        updated: missing_in_store
            .into_iter()
            .chain(memory_preferred.into_iter())
            .chain(store_applied.into_iter())
            .collect(),
    };
    info!(
        owner_hash,
        added = report.added.len(),
        removed = report.removed.len(),
        updated = report.updated.len(),
        "reconciled user entries"
    );
    Some(report)
}
