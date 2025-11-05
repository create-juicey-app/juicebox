use anyhow::anyhow;
use axum::http::{Request, Response};
use axum::{Router, extract::MatchedPath, middleware};
use axum_server::Handle;
use dashmap::DashMap;
use juicebox::handlers::ban_gate;
use juicebox::handlers::{add_cache_headers, add_security_headers, build_router};
use juicebox::rate_limit::{RateLimiterInner, build_rate_limiter};
use juicebox::state::{
    AppState, BanSubject, FileMeta, IpBan, ReportRecord, TelemetryState, cleanup_expired,
};
use juicebox::util::{
    IpVersion, PROD_HOST, UPLOAD_CONCURRENCY, hash_ip_string, hash_network_from_cidr,
    looks_like_hash, now_secs, ttl_to_duration,
};
use sentry::integrations::tracing::{self as sentry_tracing_integration, EventFilter};
use sentry::{ClientInitGuard, SessionMode};
use sentry_tower::{NewSentryLayer, SentryHttpLayer};
use serde::Deserialize;
use std::{
    borrow::Cow,
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
use tower_http::{compression::CompressionLayer, trace::TraceLayer};
use tracing::Instrument;
use tracing::field::Empty;
use tracing::{Level, debug, error, info, info_span, warn};
use tracing_log::LogTracer;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt};

#[tracing::instrument(skip(secret))]
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

#[tracing::instrument(skip(secret))]
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

#[tracing::instrument(skip(secret))]
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
                BanSubject::Network {
                    hash,
                    prefix,
                    version,
                } => {
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
                    let (version, prefix, final_hash) =
                        if let Some((ver, pre, new_hash)) = from_cidr {
                            migrated = true;
                            (ver, pre, new_hash)
                        } else if let (Some(ip), Some(pre)) = (ip.as_ref(), prefix) {
                            let cidr_string = format!("{}/{}", ip, pre);
                            if let Some((ver, pre, new_hash)) =
                                hash_network_from_cidr(secret, &cidr_string)
                            {
                                migrated = true;
                                (ver, pre, new_hash)
                            } else {
                                let ver = version.unwrap_or_else(|| {
                                    if ip.contains(':') {
                                        IpVersion::V6
                                    } else {
                                        IpVersion::V4
                                    }
                                });
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
                            } else if let Some((ver2, pre2, new_hash)) =
                                hash_network_from_cidr(secret, &format!("{}/{}", existing, pre))
                            {
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

fn decode_hash_secret(raw: &str) -> anyhow::Result<Vec<u8>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("IP_HASH_SECRET may not be empty"));
    }
    // fuck you
    if trimmed.len() % 2 == 0 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        let mut buf = Vec::with_capacity(trimmed.len() / 2);
        for chunk in trimmed.as_bytes().chunks(2) {
            let hi = (chunk[0] as char)
                .to_digit(16)
                .ok_or_else(|| anyhow!("IP_HASH_SECRET contains invalid hex"))?;
            let lo = (chunk[1] as char)
                .to_digit(16)
                .ok_or_else(|| anyhow!("IP_HASH_SECRET contains invalid hex"))?;
            buf.push(((hi << 4) | lo) as u8);
        }
        return Ok(buf);
    }
    Ok(trimmed.as_bytes().to_vec())
}

fn load_hash_secret_from_env() -> anyhow::Result<Vec<u8>> {
    let raw = std::env::var("IP_HASH_SECRET")
        .map_err(|_| anyhow!("IP_HASH_SECRET environment variable is required"))?;
    let bytes = decode_hash_secret(&raw)?;
    if bytes.len() < 16 {
        return Err(anyhow!(
            "IP_HASH_SECRET must be at least 16 bytes after decoding"
        ));
    }
    Ok(bytes)
}

const SENTRY_FLUSH_TIMEOUT_SECS: u64 = 2;

fn read_trimmed_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolve_dir_path(root: Option<&Path>, env_key: &str, default_relative: &str) -> PathBuf {
    if let Some(value) = read_trimmed_env(env_key) {
        let candidate = PathBuf::from(&value);
        if candidate.is_absolute() || root.is_none() {
            return candidate;
        }
        if let Some(root) = root {
            return root.join(candidate);
        }
    }
    if let Some(root) = root {
        if default_relative.is_empty() {
            return root.to_path_buf();
        }
        return root.join(default_relative);
    }
    PathBuf::from(default_relative)
}

fn resolve_sentry_release() -> Cow<'static, str> {
    if let Some(release) = read_trimmed_env("SENTRY_RELEASE") {
        return Cow::Owned(release);
    }
    const COMMIT_ENV_VARS: [&str; 7] = [
        "SOURCE_VERSION",
        "GIT_COMMIT",
        "GIT_SHA",
        "GITHUB_SHA",
        "VERCEL_GIT_COMMIT_SHA",
        "COMMIT_SHA",
        "REVISION",
    ];

    if let Some(commit_raw) = COMMIT_ENV_VARS.iter().find_map(|key| read_trimmed_env(key)) {
        let normalized: String = commit_raw
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
            .collect();
        let candidate = if normalized.is_empty() {
            commit_raw.clone()
        } else {
            normalized
        };
        let short_commit: String = candidate.chars().take(12).collect();
        if !short_commit.is_empty() {
            let release = format!("{}+{}", env!("CARGO_PKG_VERSION"), short_commit);
            return Cow::Owned(release);
        }
    }

    if let Some(release) = sentry::release_name!() {
        return release;
    }

    Cow::Borrowed(env!("CARGO_PKG_VERSION"))
}

struct SentryRuntime {
    guard: ClientInitGuard,
    release: String,
    environment: String,
    traces_sample_rate: f32,
    error_sample_rate: f32,
    trace_propagation_targets: Vec<String>,
    session_mode: SessionMode,
    auto_session_tracking: bool,
}

// Only enable Sentry if SENTRY_DSN is explicitly provided. Do not fall back to an
// embedded default DSN for security reasons.
fn resolve_sentry_dsn(_production: bool) -> Option<String> {
    match std::env::var("SENTRY_DSN") {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty()
                || trimmed.eq_ignore_ascii_case("disabled")
                || trimmed.eq_ignore_ascii_case("off")
                || trimmed.eq_ignore_ascii_case("false")
            {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Err(_) => None,
    }
}

fn resolve_sentry_environment(production: bool) -> String {
    std::env::var("SENTRY_ENV")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            if production {
                "production".to_string()
            } else {
                "development".to_string()
            }
        })
}

fn resolve_sentry_traces_sample_rate(_production: bool) -> f32 {
    std::env::var("SENTRY_TRACES_SAMPLE_RATE")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .and_then(|value| value.parse::<f32>().ok())
        .map(|value| value.clamp(0.0, 1.0))
        // Default to sampling everything (1.0) when SENTRY is enabled in production
        // unless explicitly overridden. In development default to 1.0 as well to
        // ensure we capture as many errors as possible during testing.
        .unwrap_or(1.0)
}

fn resolve_sentry_error_sample_rate(_production: bool) -> f32 {
    std::env::var("SENTRY_SAMPLE_RATE")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .and_then(|value| value.parse::<f32>().ok())
        .map(|value| value.clamp(0.0, 1.0))
        .unwrap_or(1.0)
}

fn resolve_sentry_trace_targets() -> Vec<String> {
    std::env::var("SENTRY_TRACE_PROPAGATION_TARGETS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|entry| entry.trim())
                .filter(|entry| !entry.is_empty())
                .map(|entry| entry.to_string())
                .collect::<Vec<_>>()
        })
        .filter(|targets| !targets.is_empty())
        .unwrap_or_else(|| vec!["^/".to_string()])
}

fn init_sentry(
    production: bool,
    dsn: Option<String>,
    release: String,
    environment: String,
    traces_sample_rate: f32,
    error_sample_rate: f32,
    trace_propagation_targets: Vec<String>,
) -> Option<SentryRuntime> {
    let dsn = dsn?;
    // Stronger defaults: attach stack traces and capture PII only when explicitly
    // running in production and SENTRY_DSN is set. traces_sample_rate controls
    // how many transactions are sampled. We keep session_mode request-based and
    // enable auto session tracking.
    let session_mode = SessionMode::Request;
    let auto_session_tracking = true;
    let release_for_scope = release.clone();
    let environment_for_scope = environment.clone();
    let mut opts = sentry::ClientOptions::default();
    opts.release = Some(release.clone().into());
    opts.environment = Some(environment.clone().into());
    // Attach stacktraces to errors to provide richer context in Sentry.
    opts.attach_stacktrace = true;
    opts.enable_logs = true;
    // Respect production flag for sending PII; only enable if production.
    opts.send_default_pii = production;
    opts.traces_sample_rate = traces_sample_rate;
    opts.sample_rate = error_sample_rate;
    let guard = sentry::init((dsn.clone(), opts));
    let trace_targets_for_scope = trace_propagation_targets.clone();
    sentry::configure_scope(|scope| {
        scope.set_tag("service", "juicebox-backend");
        scope.set_tag("runtime", "rust");
        scope.set_tag("environment", &environment_for_scope);
        scope.set_extra("release", release_for_scope.clone().into());
        scope.set_extra("traces_sample_rate", traces_sample_rate.into());
        scope.set_extra("session_mode", format!("{:?}", session_mode).into());
        scope.set_extra("auto_session_tracking", auto_session_tracking.into());
        scope.set_extra(
            "trace_propagation_targets",
            format!("{trace_targets_for_scope:?}").into(),
        );
        scope.set_extra("error_sample_rate", error_sample_rate.into());
    });
    Some(SentryRuntime {
        guard,
        release,
        environment,
        traces_sample_rate,
        error_sample_rate,
        trace_propagation_targets,
        session_mode,
        auto_session_tracking,
    })
}

fn init_tracing_subscriber() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(
            "info,juicebox=debug,juicebox::handlers=debug,hyper=warn,hyper_util=warn,reqwest=warn",
        )
    });
    let sentry_layer =
        sentry_tracing_integration::layer().event_filter(|metadata| match *metadata.level() {
            Level::TRACE | Level::DEBUG => EventFilter::Log,
            Level::INFO => EventFilter::Breadcrumb | EventFilter::Log,
            Level::WARN => EventFilter::Event | EventFilter::Breadcrumb | EventFilter::Log,
            Level::ERROR => EventFilter::Event | EventFilter::Log,
        });
    let subscriber = tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer())
        .with(sentry_layer);
    if let Err(err) = tracing::subscriber::set_global_default(subscriber) {
        eprintln!("failed to set tracing subscriber: {err}");
    }
}

fn should_trigger_sentry_verify_panic() -> bool {
    std::env::var("SENTRY_VERIFY_PANIC")
        .map(|value| {
            let trimmed = value.trim();
            trimmed.eq_ignore_ascii_case("1")
                || trimmed.eq_ignore_ascii_case("true")
                || trimmed.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let production = std::env::var("APP_ENV")
        .map(|v| v.eq_ignore_ascii_case("production"))
        .unwrap_or(false);

    let release = resolve_sentry_release().into_owned();
    let environment = resolve_sentry_environment(production);
    let traces_sample_rate = resolve_sentry_traces_sample_rate(production);
    let error_sample_rate = resolve_sentry_error_sample_rate(production);
    let trace_propagation_targets = resolve_sentry_trace_targets();
    let sentry_dsn = resolve_sentry_dsn(production);
    debug!(
        release = %release,
        environment = %environment,
        traces_sample_rate,
        error_sample_rate,
        trace_propagation_targets = ?trace_propagation_targets,
        sentry_dsn_present = sentry_dsn.is_some(),
        "resolved sentry configuration"
    );
    let sentry_runtime = init_sentry(
        production,
        sentry_dsn.clone(),
        release.clone(),
        environment.clone(),
        traces_sample_rate,
        error_sample_rate,
        trace_propagation_targets.clone(),
    );
    if let Err(err) = LogTracer::builder()
        .with_max_level(log::LevelFilter::Trace)
        .init()
    {
        eprintln!("failed to initialize log tracer: {err}");
    }
    init_tracing_subscriber();
    info!(
        production,
        pid = std::process::id(),
        "starting juicebox backend"
    );

    if let Some(ref sentry_info) = sentry_runtime {
        info!(
            release = %sentry_info.release,
            environment = %sentry_info.environment,
            error_sample_rate = sentry_info.error_sample_rate,
            traces_sample_rate = sentry_info.traces_sample_rate,
            session_mode = ?sentry_info.session_mode,
            auto_session_tracking = sentry_info.auto_session_tracking,
            trace_propagation_targets = ?sentry_info.trace_propagation_targets,
            "Sentry telemetry enabled"
        );
    } else {
        info!(
            release = %release,
            environment = %environment,
            traces_sample_rate,
            error_sample_rate,
            trace_propagation_targets = ?trace_propagation_targets,
            "Sentry telemetry disabled"
        );
    }

    let telemetry_state = TelemetryState {
        sentry_dsn: sentry_dsn.clone(),
        release,
        environment,
        traces_sample_rate,
        error_sample_rate,
        trace_propagation_targets,
    };
    debug!(
        release = %telemetry_state.release,
        environment = %telemetry_state.environment,
        traces_sample_rate = telemetry_state.traces_sample_rate,
        error_sample_rate = telemetry_state.error_sample_rate,
        trace_propagation_targets = ?telemetry_state.trace_propagation_targets,
        sentry_dsn_present = telemetry_state.sentry_dsn.is_some(),
        "telemetry state prepared"
    );

    if should_trigger_sentry_verify_panic() {
        warn!("SENTRY_VERIFY_PANIC enabled; panicking to verify telemetry");
        panic!("SENTRY_VERIFY_PANIC triggered");
    }

    let storage_root = read_trimmed_env("JUICEBOX_STORAGE_ROOT").map(PathBuf::from);
    let static_dir = Arc::new(resolve_dir_path(None, "JUICEBOX_PUBLIC_DIR", "public"));
    let data_dir = Arc::new(resolve_dir_path(
        storage_root.as_deref(),
        "JUICEBOX_DATA_DIR",
        "data",
    ));
    let upload_dir = Arc::new(resolve_dir_path(
        storage_root.as_deref(),
        "JUICEBOX_UPLOAD_DIR",
        "files",
    ));
    let metadata_path = Arc::new(data_dir.join("file_owners.json"));
    let reports_path = Arc::new(data_dir.join("reports.json"));
    let admin_sessions_path = Arc::new(data_dir.join("admin_sessions.json"));
    let admin_key_path = Arc::new(data_dir.join("admin_key.json"));
    let bans_path = Arc::new(data_dir.join("ip_bans.json"));
    let chunk_dir = Arc::new(
        read_trimmed_env("JUICEBOX_CHUNK_DIR")
            .map(|value| {
                let candidate = PathBuf::from(&value);
                if candidate.is_absolute() {
                    data_dir.join(candidate)
                } else {
                    data_dir.join("chunks")
                }
            })
            .unwrap_or_else(|| data_dir.join("chunks")),
    );

    // try create data dir earlier (already done above)
    fs::create_dir_all(&*static_dir).await?;
    fs::create_dir_all(&*upload_dir).await?;
    fs::create_dir_all(&*data_dir).await?;
    fs::create_dir_all(&*chunk_dir).await?;
    debug!(
        static_dir = ?static_dir,
        upload_dir = ?upload_dir,
        data_dir = ?data_dir,
        chunk_dir = ?chunk_dir,
        "ensured storage directories exist"
    );
    let ip_hash_secret = Arc::new(load_hash_secret_from_env()?);
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
    info!(
        owners = owners_map.len(),
        migrated = owners_migrated,
        "loaded owner metadata"
    );
    info!(
        reports = reports_vec.len(),
        migrated = reports_migrated,
        "loaded reports metadata"
    );
    info!(
        bans = bans_vec.len(),
        migrated = bans_migrated,
        "loaded ban metadata"
    );
    debug!(
        admin_sessions = admin_sessions_map.len(),
        "loaded admin sessions"
    );

    let initial_mtime = fs::metadata(&*metadata_path)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    debug!(?initial_mtime, "metadata last modified timestamp loaded");

    // gather email config early
    let mailgun_api_key = std::env::var("MAILGUN_API_KEY").ok();
    let mailgun_domain = std::env::var("MAILGUN_DOMAIN").ok();
    let report_email_to = std::env::var("REPORT_EMAIL_TO").ok();
    let report_email_from = std::env::var("REPORT_EMAIL_FROM").ok();
    debug!(
        mail_configured = mailgun_api_key.is_some()
            && mailgun_domain.is_some()
            && report_email_to.is_some()
            && report_email_from.is_some(),
        "email notification configuration evaluated"
    );

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
        owners_persist_lock: Arc::new(tokio::sync::Mutex::new(())),
        telemetry: Arc::new(telemetry_state.clone()),
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
        warn!(?err, "failed to restore chunk upload sessions from disk");
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
    let cleanup_handle = tokio::spawn(
        async move {
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
        }
        .instrument(info_span!("maintenance.cleanup")),
    );

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
                let canonical = PROD_HOST.as_str();
                let file_link = format!("https://{}/f/{}", canonical, ev.file);
                let admin_files = format!("https://{}/admin/files", canonical);
                let admin_reports = format!("https://{}/admin/reports", canonical);
                let ban_link = if !ev.owner_hash.is_empty() {
                    format!("https://{}/admin/ban?ip={}", canonical, ev.owner_hash)
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
        }
        .instrument(info_span!("mailgun.dispatcher")));
        email_handle = Some(handle);
    } else {
        println!("mail: disabled (missing env vars)");
    }

    let router = build_router(state.clone());
    let app: Router = router
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<_>| {
                    let matched_path = request
                        .extensions()
                        .get::<MatchedPath>()
                        .map(|p| p.as_str())
                        .unwrap_or("<unmatched>");
                    tracing::info_span!(
                        "http.server.request",
                        method = %request.method(),
                        matched_path,
                        uri = %request.uri(),
                        http.status_code = Empty,
                        latency_ms = Empty
                    )
                })
                .on_request(|request: &Request<_>, span: &tracing::Span| {
                    tracing::info!(
                        parent: span,
                        method = %request.method(),
                        uri = %request.uri(),
                        "HTTP request received"
                    );
                })
                .on_response(
                    |response: &Response<_>, latency: Duration, span: &tracing::Span| {
                        span.record("http.status_code", response.status().as_u16() as i64);
                        span.record("latency_ms", latency.as_millis() as i64);
                        tracing::info!(
                            parent: span,
                            status = %response.status(),
                            latency_ms = latency.as_millis(),
                            "HTTP response dispatched"
                        );
                    },
                ),
        )
        .layer(NewSentryLayer::new_from_top())
        .layer(SentryHttpLayer::new().enable_transaction())
        .layer(CompressionLayer::new())
        .layer(middleware::from_fn(add_cache_headers))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            add_security_headers,
        ))
        .layer(middleware::from_fn_with_state(state.clone(), ban_gate))
        .layer(rate_layer.clone())
        .layer(axum::extract::DefaultBodyLimit::max(
            juicebox::util::max_file_bytes() as usize,
        ));

    let addr: SocketAddr = ([0, 0, 0, 0], 1200).into();
    println!(
        "listening on {addr} (prod host: {}), admin key loaded (expires {})",
        PROD_HOST.as_str(),
        key_file.expires
    );
    let shutdown_state = state.clone();
    let shutdown_notify_clone = shutdown_notify.clone();
    let shutdown_rate = rate_handle.clone();
    let shutdown_handle = Handle::new();
    let shutdown_cancel = Arc::new(Notify::new());
    let shutdown_task = tokio::spawn(
        wait_for_shutdown(
            shutdown_state,
            shutdown_notify_clone,
            shutdown_rate.clone(),
            shutdown_handle.clone(),
            shutdown_cancel.clone(),
        )
        .instrument(info_span!("graceful_shutdown")),
    );

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
            warn!(?err, "shutdown task terminated unexpectedly");
            false
        }
    };
    if let Err(err) = cleanup_handle.await {
        warn!(?err, "cleanup task terminated unexpectedly");
    }
    if let Some(handle) = email_handle {
        match handle.await {
            Ok(_) => {}
            Err(err) => warn!(?err, "email task terminated unexpectedly"),
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
    if let Some(sentry_info) = sentry_runtime {
        sentry_info
            .guard
            .close(Some(Duration::from_secs(SENTRY_FLUSH_TIMEOUT_SECS)));
    }
    Ok(())
}

#[tracing::instrument(skip(state, notify, rate, handle, cancel))]
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
    info!("shutdown signal received; commencing graceful shutdown");
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

#[tracing::instrument(skip_all)]
async fn listen_for_shutdown() {
    let ctrl_c = async {
        if let Err(err) = ctrl_c().await {
            error!(?err, "failed to install ctrl+c handler"); // this shouldn't happen.
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(err) => error!(?err, "failed to install SIGTERM handler"), //this shouldn't happen either
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
