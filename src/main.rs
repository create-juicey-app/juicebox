use axum::{Router, middleware};
use axum_server::Handle;
use dashmap::DashMap;
use juicebox::handlers::ban_gate;
use juicebox::handlers::{add_cache_headers, add_security_headers, build_router, enforce_host};
use juicebox::rate_limit::{RateLimiterInner, build_rate_limiter};
use juicebox::state::{AppState, BanSubject, FileMeta, IpBan, ReportRecord, cleanup_expired};
use juicebox::util::{IpVersion, PROD_HOST, UPLOAD_CONCURRENCY, hash_ip_string, hash_network_from_cidr, looks_like_hash, now_secs, ttl_to_duration};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::Deserialize;
use std::{
    collections::HashMap,
    io::ErrorKind,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use tera::Tera;
use tokio::fs;
use tokio::signal::ctrl_c;
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Notify, RwLock, Semaphore};
use tower_http::compression::CompressionLayer;

async fn load_owners_with_migration(
    path: &PathBuf,
    secret: &[u8],
) -> anyhow::Result<(HashMap<String, FileMeta>, bool)> {
    let data = match fs::read(path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok((HashMap::new(), false)),
        Err(err) => return Err(err.into()),
    };
    if data.is_empty() {
        return Ok((HashMap::new(), false));
    }
    if let Ok(mut map) = serde_json::from_slice::<HashMap<String, FileMeta>>(&data) {
        let mut changed = false;
        for meta in map.values_mut() {
            if !looks_like_hash(&meta.owner_hash) {
                if let Some((_, hash)) = hash_ip_string(secret, &meta.owner_hash) {
                    meta.owner_hash = hash;
                    changed = true;
                }
            }
        }
        return Ok((map, changed));
    }
    if let Ok(old_map) = serde_json::from_slice::<HashMap<String, String>>(&data) {
        let default_exp = now_secs() + ttl_to_duration("3d").as_secs();
        let mut map = HashMap::new();
        let mut changed = false;
        for (file, owner) in old_map {
            let owner_hash = if let Some((_, hash)) = hash_ip_string(secret, &owner) {
                changed = true;
                hash
            } else {
                owner
            };
            map.insert(
                file,
                FileMeta {
                    owner_hash,
                    expires: default_exp,
                    original: String::new(),
                    created: now_secs(),
                    hash: String::new(),
                },
            );
        }
        return Ok((map, changed));
    }
    Ok((HashMap::new(), false))
}

#[derive(Deserialize)]
struct LegacyReportRecord {
    file: String,
    reason: String,
    #[serde(default)]
    details: String,
    #[serde(alias = "reporter_hash")]
    ip: String,
    time: u64,
}

async fn load_reports_with_migration(
    path: &PathBuf,
    secret: &[u8],
) -> anyhow::Result<(Vec<ReportRecord>, bool)> {
    let data = match fs::read(path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok((Vec::new(), false)),
        Err(err) => return Err(err.into()),
    };
    if data.is_empty() {
        return Ok((Vec::new(), false));
    }
    if let Ok(mut reports) = serde_json::from_slice::<Vec<ReportRecord>>(&data) {
        let mut changed = false;
        for report in reports.iter_mut() {
            if !looks_like_hash(&report.reporter_hash) {
                if let Some((_, hash)) = hash_ip_string(secret, &report.reporter_hash) {
                    report.reporter_hash = hash;
                    changed = true;
                }
            }
        }
        return Ok((reports, changed));
    }
    if let Ok(raw_reports) = serde_json::from_slice::<Vec<LegacyReportRecord>>(&data) {
        let mut reports = Vec::with_capacity(raw_reports.len());
        let mut changed = false;
        for raw in raw_reports {
            let (reporter_hash, migrated) = if looks_like_hash(&raw.ip) {
                (raw.ip, false)
            } else if let Some((_, hash)) = hash_ip_string(secret, &raw.ip) {
                (hash, true)
            } else {
                (raw.ip, false)
            };
            if migrated {
                changed = true;
            }
            reports.push(ReportRecord {
                file: raw.file,
                reason: raw.reason,
                details: raw.details,
                reporter_hash,
                time: raw.time,
            });
        }
        return Ok((reports, changed));
    }
    Ok((Vec::new(), false))
}

#[derive(Deserialize)]
struct LegacyIpBan {
    subject: LegacyBanSubject,
    #[serde(default)]
    label: Option<String>,
    reason: String,
    time: u64,
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum LegacyBanSubject {
    Exact {
        #[serde(default)]
        hash: Option<String>,
        #[serde(default)]
        ip: Option<String>,
    },
    Network {
        #[serde(default)]
        hash: Option<String>,
        #[serde(default)]
        cidr: Option<String>,
        #[serde(default)]
        ip: Option<String>,
        #[serde(default)]
        prefix: Option<u8>,
        #[serde(default)]
        version: Option<IpVersion>,
    },
}

async fn load_bans_with_migration(
    path: &PathBuf,
    secret: &[u8],
) -> anyhow::Result<(Vec<IpBan>, bool)> {
    let data = match fs::read(path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok((Vec::new(), false)),
        Err(err) => return Err(err.into()),
    };
    if data.is_empty() {
        return Ok((Vec::new(), false));
    }
    if let Ok(mut bans) = serde_json::from_slice::<Vec<IpBan>>(&data) {
        let mut changed = false;
        for ban in bans.iter_mut() {
            match &mut ban.subject {
                BanSubject::Exact { hash } => {
                    if !looks_like_hash(hash) {
                        if let Some((_, new_hash)) = hash_ip_string(secret, hash) {
                            *hash = new_hash;
                            changed = true;
                        }
                    }
                }
                BanSubject::Network { hash, prefix, version } => {
                    if !looks_like_hash(hash) {
                        let cidr = format!("{}/{}", hash, prefix);
                        if let Some((ver, pre, new_hash)) = hash_network_from_cidr(secret, &cidr) {
                            *version = ver;
                            *prefix = pre;
                            *hash = new_hash;
                            changed = true;
                        }
                    }
                }
            }
        }
        return Ok((bans, changed));
    }
    if let Ok(raw_bans) = serde_json::from_slice::<Vec<LegacyIpBan>>(&data) {
        let mut bans = Vec::with_capacity(raw_bans.len());
        let mut changed = false;
        for raw in raw_bans {
            let subject = match raw.subject {
                LegacyBanSubject::Exact { hash, ip } => {
                    let value = hash.or(ip).unwrap_or_default();
                    let (final_hash, migrated) = if looks_like_hash(&value) {
                        (value, false)
                    } else if let Some((_, new_hash)) = hash_ip_string(secret, &value) {
                        (new_hash, true)
                    } else {
                        (value, false)
                    };
                    if migrated {
                        changed = true;
                    }
                    BanSubject::Exact { hash: final_hash }
                }
                LegacyBanSubject::Network {
                    hash,
                    cidr,
                    ip,
                    prefix,
                    version,
                } => {
                    let mut migrated = false;
                    let from_cidr = cidr
                        .as_ref()
                        .and_then(|c| hash_network_from_cidr(secret, c));
                    let (version, prefix, final_hash) = if let Some((ver, pre, new_hash)) = from_cidr {
                        migrated = true;
                        (ver, pre, new_hash)
                    } else if let (Some(ip), Some(pre)) = (ip.as_ref(), prefix) {
                        let cidr_string = format!("{}/{}", ip, pre);
                        if let Some((ver, pre, new_hash)) = hash_network_from_cidr(secret, &cidr_string)
                        {
                            migrated = true;
                            (ver, pre, new_hash)
                        } else {
                            let ver = version.unwrap_or_else(|| if ip.contains(':') { IpVersion::V6 } else { IpVersion::V4 });
                            (ver, pre, hash.clone().unwrap_or_else(|| ip.clone()))
                        }
                    } else if let Some(existing) = hash {
                        let ver = version.unwrap_or(IpVersion::V4);
                        let pre = prefix.unwrap_or(match ver {
                            IpVersion::V4 => 32,
                            IpVersion::V6 => 128,
                        });
                        if looks_like_hash(&existing) {
                            (ver, pre, existing)
                        } else if let Some((ver2, pre2, new_hash)) = hash_network_from_cidr(
                            secret,
                            &format!("{}/{}", existing, pre),
                        ) {
                            migrated = true;
                            (ver2, pre2, new_hash)
                        } else {
                            (ver, pre, existing)
                        }
                    } else {
                        (IpVersion::V4, 32, String::new())
                    };
                    if migrated {
                        changed = true;
                    }
                    BanSubject::Network {
                        hash: final_hash,
                        prefix,
                        version,
                    }
                }
            };
            bans.push(IpBan {
                subject,
                label: raw.label,
                reason: raw.reason,
                time: raw.time,
            });
        }
        return Ok((bans, changed));
    }
    Ok((Vec::new(), false))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing subscriber for logging
    tracing_subscriber::fmt::init();
    // load env (non-fatal)
    let _ = dotenvy::dotenv();
    let production = std::env::var("APP_ENV")
        .map(|v| v.eq_ignore_ascii_case("production"))
        .unwrap_or(false);

    let static_dir = Arc::new(PathBuf::from("./public"));
    let upload_dir = Arc::new(PathBuf::from("./files"));
    let data_dir = Arc::new(PathBuf::from("./data"));
    let metadata_path = Arc::new(data_dir.join("file_owners.json"));
    let reports_path = Arc::new(data_dir.join("reports.json"));
    let admin_sessions_path = Arc::new(data_dir.join("admin_sessions.json"));
    let admin_key_path = Arc::new(data_dir.join("admin_key.json"));
    let bans_path = Arc::new(data_dir.join("ip_bans.json"));
    let chunk_dir = Arc::new(data_dir.join("chunks"));
    let ip_hash_secret_path = data_dir.join("ip_hash_secret.bin");

    // try create data dir earlier (already done above)
    fs::create_dir_all(&*static_dir).await?;
    fs::create_dir_all(&*upload_dir).await?;
    fs::create_dir_all(&*data_dir).await?;
    fs::create_dir_all(&*chunk_dir).await?;
    if fs::metadata(&ip_hash_secret_path).await.is_err() {
        let mut buf = [0u8; 32];
        OsRng.fill_bytes(&mut buf);
        fs::write(&ip_hash_secret_path, &buf).await?;
    }
    let ip_hash_secret = if let Ok(bytes) = fs::read(&ip_hash_secret_path).await {
        if !bytes.is_empty() {
            bytes
        } else {
            let mut buf = [0u8; 32];
            OsRng.fill_bytes(&mut buf);
            fs::write(&ip_hash_secret_path, &buf).await?;
            buf.to_vec()
        }
    } else {
        let mut buf = [0u8; 32];
        OsRng.fill_bytes(&mut buf);
        fs::write(&ip_hash_secret_path, &buf).await?;
        buf.to_vec()
    };
    let ip_hash_secret = Arc::new(ip_hash_secret);
    // ensure bans file presence
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&*bans_path)
        .await;

    let (owners_map, owners_migrated) =
        load_owners_with_migration(metadata_path.as_ref(), &ip_hash_secret).await?;
    let (reports_vec, reports_migrated) =
        load_reports_with_migration(reports_path.as_ref(), &ip_hash_secret).await?;
    let admin_sessions_map: HashMap<String, u64> = match fs::read(&*admin_sessions_path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => HashMap::new(),
    };
    let (bans_vec, bans_migrated) =
        load_bans_with_migration(bans_path.as_ref(), &ip_hash_secret).await?;

    let initial_mtime = fs::metadata(&*metadata_path)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .unwrap_or(SystemTime::UNIX_EPOCH);

    // gather email config early
    let mailgun_api_key = std::env::var("MAILGUN_API_KEY").ok();
    let mailgun_domain = std::env::var("MAILGUN_DOMAIN").ok();
    let report_email_to = std::env::var("REPORT_EMAIL_TO").ok();
    let report_email_from = std::env::var("REPORT_EMAIL_FROM").ok();

    // Initialize Tera
    let tera = match Tera::new("templates/**/*.tera") {
        Ok(t) => std::sync::Arc::new(t),
        Err(e) => panic!("Failed to initialize Tera: {}", e),
    };
    let mut state = AppState {
        upload_dir,
        static_dir,
        owners: Arc::new(DashMap::from_iter(owners_map)),
        metadata_path: metadata_path.clone(),
        upload_sem: Arc::new(Semaphore::new(UPLOAD_CONCURRENCY)),
        production,
        last_meta_mtime: Arc::new(RwLock::new(initial_mtime)),
        reports_path,
        reports: Arc::new(RwLock::new(reports_vec)),
        admin_sessions_path,
        admin_sessions: Arc::new(RwLock::new(admin_sessions_map)),
        admin_key_path: admin_key_path.clone(),
        admin_key: Arc::new(RwLock::new(String::new())),
        bans_path: bans_path.clone(),
        bans: Arc::new(RwLock::new(bans_vec)),
        mailgun_api_key,
        mailgun_domain,
        report_email_to,
        report_email_from,
        email_tx: None,
        tera,
        chunk_dir,
        chunk_sessions: Arc::new(DashMap::new()),
        ip_hash_secret: ip_hash_secret.clone(),
    };

    if owners_migrated {
        state.persist_owners().await;
    }
    if reports_migrated {
        state.persist_reports().await;
    }
    if bans_migrated {
        state.persist_bans().await;
    }

    if let Err(err) = state.load_chunk_sessions_from_disk().await {
        tracing::warn!(?err, "failed to restore chunk upload sessions from disk");
    }

    // Load or create admin key after state so helper can use now_secs etc
    let key_file = state.load_or_create_admin_key(&admin_key_path).await?;
    {
        let mut k = state.admin_key.write().await;
        *k = key_file.key.clone();
    }

    let shutdown_notify = Arc::new(Notify::new());
    let (rate_layer, rate_handle) = build_rate_limiter();

    // periodic cleanup task
    let cleanup_state = state.clone();
    let cleanup_shutdown = shutdown_notify.clone();
    let cleanup_rate = rate_handle.clone();
    let cleanup_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(600));
        loop {
            tokio::select! {
                _ = cleanup_shutdown.notified() => {
                    break;
                }
                _ = interval.tick() => {
                    cleanup_expired(&cleanup_state).await;
                    cleanup_state.cleanup_admin_sessions().await;
                    cleanup_state.cleanup_chunk_sessions().await;
                    cleanup_rate.prune_idle(Duration::from_secs(1800)).await;
                }
            }
        }
    });

    // setup email worker if config present
    let mut email_handle = None;
    if state.mailgun_api_key.is_some()
        && state.mailgun_domain.is_some()
        && state.report_email_to.is_some()
        && state.report_email_from.is_some()
    {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<juicebox::handlers::ReportRecordEmail>(100);
        state.email_tx = Some(tx);
        let api_key = state.mailgun_api_key.clone().unwrap();
        let domain = state.mailgun_domain.clone().unwrap();
        let to_addr = state.report_email_to.clone().unwrap();
        let from_addr = state.report_email_from.clone().unwrap();
        println!("mail: enabled (domain={domain}, to={to_addr})");
        let email_shutdown = shutdown_notify.clone();
        let handle = tokio::spawn(async move {
            let client = reqwest::Client::new();
            loop {
                tokio::select! {
                    _ = email_shutdown.notified() => {
                        break;
                    }
                    maybe_ev = rx.recv() => {
                        let Some(ev) = maybe_ev else { break; };
                let subj = format!("[JuiceBox] Report: {} ({})", ev.file, ev.reason);
                let expires_human = if ev.expires > 0 {
                    format!("{}s", ev.expires.saturating_sub(ev.time))
                } else {
                    "n/a".into()
                };
                let mut html = String::new();
                html.push_str("<html><body style=\"font-family:system-ui,Arial,sans-serif;background:#0f141b;color:#e8edf2;padding:16px;\">");
                html.push_str("<div style=\"background:#18222d;border:1px solid #2b3746;border-radius:12px;padding:18px 20px;max-width:640px;margin:auto;\">");
                html.push_str(
                    "<h2 style=\"margin:0 0 12px;font-size:18px;\">New Content Report</h2>"
                );
                html.push_str("<table style=\"width:100%;border-collapse:collapse;font-size:13px;margin-bottom:14px;\">");
                let row = |k: &str, v: &str| {
                    format!(
                        "<tr><td style=\"padding:4px 6px;border:1px solid #273341;background:#121b24;font-weight:600;\">{}</td><td style=\"padding:4px 6px;border:1px solid #273341;\">{}</td></tr>",
                        k,
                        htmlescape::encode_minimal(v)
                    )
                };
                html.push_str(&row("File ID", &ev.file));
                html.push_str(&row("Reason", &ev.reason));
                html.push_str(&row("Reporter Hash IP", &ev.reporter_hash));
                html.push_str(&row("Owner Hash IP", &ev.owner_hash));
                html.push_str(&row("Original Name", &ev.original_name));
                html.push_str(&row("Size (bytes)", &ev.size.to_string()));
                html.push_str(&row(
                    "Report Time",
                    &format!("{} ({})", ev.time, ev.iso_time),
                ));
                html.push_str(&row("Expires At (epoch)", &ev.expires.to_string()));
                html.push_str(&row("Remaining TTL (approx)", &expires_human));
                html.push_str(&row(
                    "Reports for File",
                    &ev.total_reports_for_file.to_string(),
                ));
                html.push_str(&row("Total Reports (all)", &ev.total_reports.to_string()));
                html.push_str("</table>");
                if !ev.details.is_empty() {
                    html.push_str("<div style=\"margin:10px 0 14px;font-size:12px;line-height:1.4;\"><strong style=\"display:block;margin-bottom:4px;\">Details</strong><pre style=\"white-space:pre-wrap;background:#121b24;border:1px solid #273341;padding:8px 10px;border-radius:8px;font:12px/1.4 ui-monospace,monospace;\">");
                    html.push_str(&htmlescape::encode_minimal(&ev.details));
                    html.push_str("</pre></div>");
                }
                let file_link = format!("https://{}/f/{}", PROD_HOST, ev.file);
                let admin_files = format!("https://{}/admin/files", PROD_HOST);
                let admin_reports = format!("https://{}/admin/reports", PROD_HOST);
                let ban_link = if !ev.owner_hash.is_empty() {
                    format!("https://{}/admin/ban?ip={}", PROD_HOST, ev.owner_hash)
                } else {
                    String::new()
                };
                let has_ban = !ban_link.is_empty();
                html.push_str(
                    "<div style=\"display:inline-flex;flex-wrap:nowrap;margin-top:6px;\">",
                );

                // First (left rounded)
                html.push_str(&format!(
                    "<a href=\"{}\" style=\"background:#ff9800;color:#111;padding:8px 12px;font-size:12px;text-decoration:none;font-weight:600;border-radius:8px 0 0 8px;\">Open File</a>",
                    file_link
                ));

                // Middle (square)
                html.push_str(&format!(
                    "<a href=\"{}\" style=\"background:#40618a;color:#fff;padding:8px 12px;font-size:12px;text-decoration:none;font-weight:600;border-radius:0;\">Manage Files</a>",
                    admin_files
                ));

                if has_ban {
                    // Middle (square)
                    html.push_str(&format!(
                        "<a href=\"{}\" style=\"background:#3d8f6e;color:#fff;padding:8px 12px;font-size:12px;text-decoration:none;font-weight:600;border-radius:0;\">View Reports</a>",
                        admin_reports
                    ));
                    // Last (right rounded)
                    html.push_str(&format!(
                        "<a href=\"{}\" style=\"background:#ff3d00;color:#fff;padding:8px 12px;font-size:12px;text-decoration:none;font-weight:600;border-radius:0 8px 8px 0;\">Ban Owner IP</a>",
                        ban_link
                    ));
                } else {
                    // Last (right rounded because no ban button)
                    html.push_str(&format!(
                        "<a href=\"{}\" style=\"background:#3d8f6e;color:#fff;padding:8px 12px;font-size:12px;text-decoration:none;font-weight:600;border-radius:0 8px 8px 0;\">View Reports</a>",
                        admin_reports
                    ));
                }
                html.push_str("</div>");
                html.push_str("<p style=\"margin-top:16px;font-size:10px;opacity:.55;\">Automated notification. Use admin dashboard to delete report or file. Do not forward externally.</p>");
                html.push_str("</div></body></html>");

                let text = format!(
                    "Report: file={} reason={} reporter_ip={} owner_ip={} size={} details={}",
                    ev.file,
                    ev.reason,
                    ev.reporter_hash,
                    ev.owner_hash,
                    ev.size,
                    if ev.details.is_empty() {
                        "(none)"
                    } else {
                        ev.details.as_str()
                    }
                );
                let form = [
                    ("from", from_addr.as_str()),
                    ("to", to_addr.as_str()),
                    ("subject", subj.as_str()),
                    ("text", text.as_str()),
                    ("html", html.as_str()),
                ];
                let url = format!("https://api.eu.mailgun.net/v3/{}/messages", domain);
                match client
                    .post(&url)
                    .basic_auth("api", Some(&api_key))
                    .form(&form)
                    .send()
                    .await
                {
                    Ok(resp) => {
                        if !resp.status().is_success() {
                            let status = resp.status();
                            let body_txt = resp.text().await.unwrap_or_default();
                            eprintln!("mail: failed status={status} body={body_txt}");
                        } else {
                            println!(
                                "mail: sent report file={} reason={} owner_hash={} reporter_hash={}",
                                ev.file, ev.reason, ev.owner_hash, ev.reporter_hash
                            );
                        }
                    }
                    Err(e) => eprintln!("mail: error sending: {e}"),
                }
                    }
                }
            }
        });
        email_handle = Some(handle);
    } else {
        println!("mail: disabled (missing env vars)");
    }

    let router = build_router(state.clone());
    let mut app: Router = router
        .layer(CompressionLayer::new())
        .layer(middleware::from_fn(add_cache_headers))
        .layer(middleware::from_fn(add_security_headers))
        .layer(middleware::from_fn_with_state(state.clone(), ban_gate))
        .layer(rate_layer.clone())
        .layer(axum::extract::DefaultBodyLimit::max(
            juicebox::util::max_file_bytes() as usize,
        ));
    if state.production {
        app = app.layer(middleware::from_fn(enforce_host));
    }

    let addr: SocketAddr = ([0, 0, 0, 0], 1200).into();
    println!(
        "listening on {addr} (prod host: {}), admin key loaded (expires {})",
        PROD_HOST, key_file.expires
    );
    let shutdown_state = state.clone();
    let shutdown_notify_clone = shutdown_notify.clone();
    let shutdown_rate = rate_handle.clone();
    let shutdown_handle = Handle::new();
    let shutdown_cancel = Arc::new(Notify::new());
    let shutdown_task = tokio::spawn(wait_for_shutdown(
        shutdown_state,
        shutdown_notify_clone,
        shutdown_rate.clone(),
        shutdown_handle.clone(),
        shutdown_cancel.clone(),
    ));

    let server = axum_server::bind(addr)
        .handle(shutdown_handle.clone())
        .serve(app.into_make_service_with_connect_info::<SocketAddr>());

    let server_result = server.await;
    shutdown_handle.shutdown();
    shutdown_notify.notify_waiters();
    shutdown_cancel.notify_waiters();
    let shutdown_handled = match shutdown_task.await {
        Ok(result) => result,
        Err(err) => {
            tracing::warn!(?err, "shutdown task terminated unexpectedly");
            false
        }
    };
    if let Err(err) = cleanup_handle.await {
        tracing::warn!(?err, "cleanup task terminated unexpectedly");
    }
    if let Some(handle) = email_handle {
        match handle.await {
            Ok(_) => {}
            Err(err) => tracing::warn!(?err, "email task terminated unexpectedly"),
        }
    }
    if !shutdown_handled {
        state.cleanup_admin_sessions().await;
        state.persist_admin_sessions().await;
        state.persist_reports().await;
        state.persist_bans().await;
        state.persist_owners().await;
        state.persist_all_chunk_sessions().await;
        rate_handle.prune_idle(Duration::from_secs(0)).await;
    }
    server_result?;
    Ok(())
}

async fn wait_for_shutdown(
    state: AppState,
    notify: Arc<Notify>,
    rate: RateLimiterInner,
    handle: Handle,
    cancel: Arc<Notify>,
) -> bool {
    let triggered = tokio::select! {
        _ = listen_for_shutdown() => true,
        _ = cancel.notified() => false,
    };
    if !triggered {
        return false;
    }
    tracing::info!("shutdown signal received; commencing graceful shutdown");
    notify.notify_waiters();
    handle.shutdown();
    state.cleanup_admin_sessions().await;
    state.persist_admin_sessions().await;
    state.persist_reports().await;
    state.persist_bans().await;
    state.persist_owners().await;
    state.persist_all_chunk_sessions().await;
    rate.prune_idle(Duration::from_secs(0)).await;
    true
}

async fn listen_for_shutdown() {
    let ctrl_c = async {
        if let Err(err) = ctrl_c().await {
            tracing::error!(?err, "failed to install ctrl+c handler");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(err) => tracing::error!(?err, "failed to install SIGTERM handler"),
        }
    };
    #[cfg(not(unix))]
    let terminate = async {
        std::future::pending::<()>().await;
    };
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
