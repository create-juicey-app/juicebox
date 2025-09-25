use std::{collections::HashMap, path::PathBuf, sync::Arc, net::SocketAddr, time::{Duration, SystemTime}};
use dashmap::DashMap;
use tokio::fs; use tokio::sync::{RwLock, Semaphore};
use axum::{Router, middleware};
use tower_http::compression::CompressionLayer;
use juicebox::state::{AppState, FileMeta, ReportRecord, cleanup_expired};
use juicebox::util::{ttl_to_duration, now_secs, PROD_HOST, UPLOAD_CONCURRENCY};
use juicebox::handlers::{build_router, add_security_headers, enforce_host, add_cache_headers};
use juicebox::handlers::ban_gate;
use juicebox::rate_limit::build_rate_limiter;
use tera::Tera;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // load env (non-fatal)
    let _ = dotenvy::dotenv();
    let production = std::env::var("APP_ENV").map(|v| v.eq_ignore_ascii_case("production")).unwrap_or(false);

    let static_dir = Arc::new(PathBuf::from("./public"));
    let upload_dir = Arc::new(PathBuf::from("./files"));
    let data_dir = Arc::new(PathBuf::from("./data"));
    let metadata_path = Arc::new(data_dir.join("file_owners.json"));
    let reports_path = Arc::new(data_dir.join("reports.json"));
    let admin_sessions_path = Arc::new(data_dir.join("admin_sessions.json"));
    let admin_key_path = Arc::new(data_dir.join("admin_key.json"));
    let bans_path = Arc::new(data_dir.join("ip_bans.json"));

    // try create data dir earlier (already done above)
    fs::create_dir_all(&*static_dir).await?;
    fs::create_dir_all(&*upload_dir).await?;
    fs::create_dir_all(&*data_dir).await?;
    // ensure bans file presence
    let _ = fs::OpenOptions::new().create(true).append(true).open(&*bans_path).await;

    let owners_map: HashMap<String, FileMeta> = match fs::read(&*metadata_path).await {
        Ok(data) => {
            if let Ok(old_map) = serde_json::from_slice::<HashMap<String,String>>(&data) {
                old_map.into_iter().map(|(k,v)| (k, FileMeta {
                    owner: v,
                    expires: now_secs() + ttl_to_duration("3d").as_secs(),
                    original: String::new(),
                    created: now_secs(),
                    hash: String::new(), // legacy files have no hash, so set empty
                })).collect()
            } else { serde_json::from_slice(&data).unwrap_or_default() }
        }
        Err(_) => HashMap::new(),
    };


    let reports_vec: Vec<ReportRecord> = match fs::read(&*reports_path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let admin_sessions_map: HashMap<String,u64> = match fs::read(&*admin_sessions_path).await { Ok(bytes)=>serde_json::from_slice(&bytes).unwrap_or_default(), Err(_)=>HashMap::new() };
    let bans_vec: Vec<juicebox::state::IpBan> = match fs::read(&*bans_path).await { Ok(bytes)=>serde_json::from_slice(&bytes).unwrap_or_default(), Err(_)=>Vec::new() };

    let initial_mtime = fs::metadata(&*metadata_path).await
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
    };

    // Load or create admin key after state so helper can use now_secs etc
    let key_file = state.load_or_create_admin_key(&admin_key_path).await?;
    {
        let mut k = state.admin_key.write().await; *k = key_file.key.clone();
    }

    // periodic cleanup task
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(600));
        loop {
            interval.tick().await;
            cleanup_expired(&cleanup_state).await;
            cleanup_state.cleanup_admin_sessions().await;
        }
    });

    // setup email worker if config present
    if state.mailgun_api_key.is_some() && state.mailgun_domain.is_some() && state.report_email_to.is_some() && state.report_email_from.is_some() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<juicebox::handlers::ReportRecordEmail>(100);
        state.email_tx = Some(tx);
        let api_key = state.mailgun_api_key.clone().unwrap();
        let domain = state.mailgun_domain.clone().unwrap();
        let to_addr = state.report_email_to.clone().unwrap();
        let from_addr = state.report_email_from.clone().unwrap();
        println!("mail: enabled (domain={domain}, to={to_addr})");
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            while let Some(ev) = rx.recv().await {
                let subj = format!("[JuiceBox] Report: {} ({})", ev.file, ev.reason);
                let expires_human = if ev.expires>0 { format!("{}s", ev.expires.saturating_sub(ev.time)) } else { "n/a".into() };
                let mut html = String::new();
                html.push_str("<html><body style=\"font-family:system-ui,Arial,sans-serif;background:#0f141b;color:#e8edf2;padding:16px;\">");
                html.push_str("<div style=\"background:#18222d;border:1px solid #2b3746;border-radius:12px;padding:18px 20px;max-width:640px;margin:auto;\">");
                html.push_str(&format!("<h2 style=\"margin:0 0 12px;font-size:18px;\">New Content Report</h2>"));
                html.push_str("<table style=\"width:100%;border-collapse:collapse;font-size:13px;margin-bottom:14px;\">");
                let row = |k:&str,v:&str| format!("<tr><td style=\"padding:4px 6px;border:1px solid #273341;background:#121b24;font-weight:600;\">{}</td><td style=\"padding:4px 6px;border:1px solid #273341;\">{}</td></tr>", k, htmlescape::encode_minimal(v));
                html.push_str(&row("File ID", &ev.file));
                html.push_str(&row("Reason", &ev.reason));
                html.push_str(&row("Reporter IP", &ev.ip));
                html.push_str(&row("Owner IP", &ev.owner_ip));
                html.push_str(&row("Original Name", &ev.original_name));
                html.push_str(&row("Size (bytes)", &ev.size.to_string()));
                html.push_str(&row("Report Time", &format!("{} ({})", ev.time, ev.iso_time)));
                html.push_str(&row("Expires At (epoch)", &ev.expires.to_string()));
                html.push_str(&row("Remaining TTL (approx)", &expires_human));
                html.push_str(&row("Reports for File", &ev.total_reports_for_file.to_string()));
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
                let ban_link = if !ev.owner_ip.is_empty() { format!("https://{}/admin/ban?ip={}", PROD_HOST, ev.owner_ip) } else { String::new() };
                let has_ban = !ban_link.is_empty();
                html.push_str("<div style=\"display:inline-flex;flex-wrap:nowrap;margin-top:6px;\">");

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

                let text = format!("Report: file={} reason={} reporter_ip={} owner_ip={} size={} details={}", ev.file, ev.reason, ev.ip, ev.owner_ip, ev.size, if ev.details.is_empty(){"(none)"} else {ev.details.as_str()});
                let form = [
                    ("from", from_addr.as_str()),
                    ("to", to_addr.as_str()),
                    ("subject", subj.as_str()),
                    ("text", text.as_str()),
                    ("html", html.as_str())
                ];
                let url = format!("https://api.eu.mailgun.net/v3/{}/messages", domain);
                match client.post(&url)
                    .basic_auth("api", Some(&api_key))
                    .form(&form)
                    .send().await {
                    Ok(resp) => {
                        if !resp.status().is_success() {
                            let status = resp.status();
                            let body_txt = resp.text().await.unwrap_or_default();
                            eprintln!("mail: failed status={status} body={body_txt}");
                        } else { println!("mail: sent report file={} reason={} owner_ip={} reporter_ip={}", ev.file, ev.reason, ev.owner_ip, ev.ip); }
                    },
                    Err(e) => eprintln!("mail: error sending: {e}"),
                }
            }
        });
    } else {
        println!("mail: disabled (missing env vars)");
    }

    let rate_layer = build_rate_limiter();
    let router = build_router(state.clone());
    let mut app: Router = router
        .layer(CompressionLayer::new())
        .layer(middleware::from_fn(add_cache_headers))
        .layer(middleware::from_fn(add_security_headers))
        .layer(middleware::from_fn_with_state(state.clone(), ban_gate))
        .layer(rate_layer)
        .layer(axum::extract::DefaultBodyLimit::max(juicebox::util::max_file_bytes() as usize));
    if state.production { app = app.layer(middleware::from_fn(enforce_host)); }

    let addr: SocketAddr = ([0,0,0,0], 1200).into();
    println!("listening on {addr} (prod host: {}), admin key loaded (expires {})", PROD_HOST, key_file.expires);
    axum_server::bind(addr)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await?;
    Ok(())
}
