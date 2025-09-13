use std::{sync::Arc, path::PathBuf, time::SystemTime, collections::HashMap};
use tokio::sync::{RwLock, Semaphore};
use serde::{Serialize, Deserialize};
use tokio::fs; use tokio::io::AsyncWriteExt;
use crate::util::{now_secs, ttl_to_duration};

#[derive(Serialize, Deserialize, Clone, Debug)] pub struct FileMeta { pub owner: String, pub expires: u64 }
#[derive(Serialize, Deserialize, Clone, Debug)] pub struct ReportRecord { pub file: String, pub reason: String, pub details: String, pub ip: String, pub time: u64 }

#[derive(Clone)] pub struct AppState {
    pub upload_dir: Arc<PathBuf>, pub static_dir: Arc<PathBuf>, pub metadata_path: Arc<PathBuf>,
    pub owners: Arc<RwLock<HashMap<String, FileMeta>>>, pub upload_sem: Arc<Semaphore>, pub production: bool,
    pub last_meta_mtime: Arc<RwLock<SystemTime>>, pub reports_path: Arc<PathBuf>, pub reports: Arc<RwLock<Vec<ReportRecord>>>,
}

impl AppState {
    pub async fn persist_owners(&self) { let owners=self.owners.read().await; if let Ok(json)=serde_json::to_vec(&*owners) { let tmp=self.metadata_path.with_extension("tmp"); if let Ok(mut f)=fs::File::create(&tmp).await { if f.write_all(&json).await.is_ok() { let _=f.sync_all().await; let _=fs::rename(&tmp,&*self.metadata_path).await; if let Ok(md)=fs::metadata(&*self.metadata_path).await { if let Ok(modified)=md.modified() { let mut lm=self.last_meta_mtime.write().await; *lm=modified; } } } } } }
    pub async fn persist_reports(&self) { let reports=self.reports.read().await; if let Ok(json)=serde_json::to_vec(&*reports) { let tmp=self.reports_path.with_extension("tmp"); if let Ok(mut f)=fs::File::create(&tmp).await { if f.write_all(&json).await.is_ok() { let _=f.sync_all().await; let _=fs::rename(&tmp,&*self.reports_path).await; } } } }
}

pub async fn check_storage_integrity(state: &AppState) { let mut to_remove=Vec::new(); { let owners=state.owners.read().await; for (fname,_meta) in owners.iter() { if !state.upload_dir.join(fname).exists() { to_remove.push(fname.clone()); } } } if to_remove.is_empty(){return;} { let mut owners=state.owners.write().await; for f in &to_remove { owners.remove(f); } } state.persist_owners().await; }
pub fn spawn_integrity_check(state: AppState) { tokio::spawn(async move { check_storage_integrity(&state).await; }); }

pub async fn cleanup_expired(state: &AppState) { let now=now_secs(); let mut to_delete=Vec::new(); { let owners=state.owners.read().await; for (file, meta) in owners.iter() { if meta.expires <= now { to_delete.push(file.clone()); } } } if to_delete.is_empty(){return;} { let mut owners=state.owners.write().await; for f in &to_delete { owners.remove(f); } } for f in &to_delete { let _=fs::remove_file(state.upload_dir.join(f)).await; } state.persist_owners().await; }

pub async fn reload_metadata_if_changed(state: &AppState) { let meta_res=fs::metadata(&*state.metadata_path).await; let md=match meta_res { Ok(m)=>m, Err(_)=>return }; let modified=match md.modified(){Ok(t)=>t,Err(_)=>return}; let need_reload = { let lm=state.last_meta_mtime.read().await; modified>*lm }; if !need_reload { return; } if let Ok(bytes)=fs::read(&*state.metadata_path).await { if let Ok(map)=serde_json::from_slice::<HashMap<String, FileMeta>>(&bytes) { let mut owners=state.owners.write().await; owners.clear(); owners.extend(map.into_iter()); } else if let Ok(old)=serde_json::from_slice::<HashMap<String,String>>(&bytes) { let mut owners=state.owners.write().await; owners.clear(); let default_exp= now_secs() + ttl_to_duration("3d").as_secs(); owners.extend(old.into_iter().map(|(k,v)|(k, FileMeta{ owner:v, expires: default_exp }))); } let mut lm=state.last_meta_mtime.write().await; *lm=modified; } }

pub async fn verify_user_entries(state: &AppState, ip: &str) { if let Ok(bytes)=fs::read(&*state.metadata_path).await { if let Ok(disk_map)=serde_json::from_slice::<HashMap<String, FileMeta>>(&bytes) { let mut to_remove=Vec::new(); let mut to_update=Vec::new(); let mut to_add=Vec::new(); { let owners=state.owners.read().await; for (fname, meta_mem) in owners.iter() { if meta_mem.owner==ip { match disk_map.get(fname) { Some(meta_disk)=>{ if meta_disk.owner!=meta_mem.owner || meta_disk.expires!=meta_mem.expires { to_update.push((fname.clone(), meta_disk.clone())); } }, None=>to_remove.push(fname.clone()) } } } for (fname, meta_disk) in disk_map.iter() { if meta_disk.owner==ip && !owners.contains_key(fname) { to_add.push((fname.clone(), meta_disk.clone())); } } } if !(to_remove.is_empty() && to_update.is_empty() && to_add.is_empty()) { let mut owners=state.owners.write().await; for f in to_remove { owners.remove(&f); } for (f,m) in to_update { owners.insert(f,m); } for (f,m) in to_add { owners.insert(f,m); } } } } }

#[derive(serde::Serialize)] pub struct ReconcileReport { pub added: Vec<String>, pub removed: Vec<String>, pub updated: Vec<String> }

pub async fn verify_user_entries_with_report(state: &AppState, ip: &str) -> Option<ReconcileReport> { if let Ok(bytes)=fs::read(&*state.metadata_path).await { if let Ok(disk_map)=serde_json::from_slice::<HashMap<String, FileMeta>>(&bytes) { let mut to_remove=Vec::new(); let mut to_update=Vec::new(); let mut to_add=Vec::new(); { let owners=state.owners.read().await; for (fname, meta_mem) in owners.iter() { if meta_mem.owner==ip { match disk_map.get(fname) { Some(meta_disk)=>{ if meta_disk.owner!=meta_mem.owner || meta_disk.expires!=meta_mem.expires { to_update.push((fname.clone(), meta_disk.clone())); } }, None=>to_remove.push(fname.clone()) } } } for (fname, meta_disk) in disk_map.iter() { if meta_disk.owner==ip && !owners.contains_key(fname) { to_add.push((fname.clone(), meta_disk.clone())); } } } if to_remove.is_empty() && to_update.is_empty() && to_add.is_empty() { return None; } { let mut owners=state.owners.write().await; for f in &to_remove { owners.remove(f); } for (f,m) in &to_update { owners.insert(f.clone(), m.clone()); } for (f,m) in &to_add { owners.insert(f.clone(), m.clone()); } } return Some(ReconcileReport { added: to_add.into_iter().map(|(f,_)| f).collect(), removed: to_remove, updated: to_update.into_iter().map(|(f,_)| f).collect() }); } } None }
