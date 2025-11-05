use axum::Json;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use tera::Context;
use tokio::fs;
use tracing::{debug, error, trace, warn};

use crate::state::{AppState, BanSubject};
use crate::util::{
    IpVersion, MAX_ACTIVE_FILES_PER_IP, extract_client_ip, format_bytes, headers_trusted,
    max_file_bytes, now_secs, qualify_path, real_client_ip,
};

#[derive(Deserialize)]
pub struct SimpleQuery {
    pub m: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LangQuery {
    pub lang: Option<String>,
    pub m: Option<String>,
    pub deleted: Option<String>,
}

async fn apply_manifest_assets(state: &AppState, ctx: &mut Context) {
    let manifest_path = state.static_dir.join("dist/manifest.json");
    match fs::read_to_string(&manifest_path).await {
        Ok(manifest_str) => match serde_json::from_str::<HashMap<String, String>>(&manifest_str) {
            Ok(manifest_map) => {
                if let Some(app_bundle) = manifest_map.get("app") {
                    ctx.insert("app_bundle", app_bundle);
                }
                if let Some(css_bundle) = manifest_map.get("css") {
                    ctx.insert("css_bundle", css_bundle);
                }
                trace!(path = ?manifest_path, "applied manifest assets");
            }
            Err(err) => warn!(?err, path = ?manifest_path, "failed to parse asset manifest"),
        },
        Err(err) => debug!(?err, path = ?manifest_path, "asset manifest not available"),
    }
}

#[tracing::instrument(name = "web.root", skip(state), fields(lang = %query.lang.as_deref().unwrap_or("en")))]
pub async fn root_handler(
    State(state): State<AppState>,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");
    trace!(lang, "rendering root page");
    let t_map = load_translation_map(lang).await;
    let mut ctx = Context::new();
    ctx.insert("lang", lang);
    ctx.insert("t", &t_map);
    ctx.insert("max_file_bytes", &max_file_bytes());
    ctx.insert("max_file_size_str", &format_bytes(max_file_bytes()));
    apply_manifest_assets(&state, &mut ctx).await;
    let tera = &state.tera;
    match tera.render("index.html.tera", &ctx) {
        Ok(rendered) => {
            debug!(lang, "rendered root page");
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "text/html")],
                rendered,
            )
                .into_response()
        }
        Err(e) => {
            error!(?e, "failed to render index template");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("template error: {}", e),
            )
                .into_response()
        }
    }
}

pub async fn debug_ip_handler(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let edge = addr.ip().to_string();
    let cf = headers
        .get("CF-Connecting-IP")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    let xff = headers
        .get("X-Forwarded-For")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    debug!(edge, cf, xff, "debug ip request");
    Json(json!({"edge": edge, "cf": cf, "xff": xff})).into_response()
}

pub async fn trusted_handler(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let edge = addr.ip().to_string();
    let trusted = headers_trusted(&headers, Some(addr.ip()));
    if trusted {
        debug!(edge, "headers trusted for request");
        Json(json!({"trusted": true, "message": "HEADERS TRUSTED"})).into_response()
    } else {
        warn!(edge, "headers not trusted; falling back to edge ip");
        // return simple HTML with silly red message when untrusted
        let body = format!(
            "<html><body><h1 style=\"color:red\">ENDPOINT IS UNTRUSTED AND THE EDGE IP WILL BE USED</h1><p>edge: {}</p><p>cf: {}</p><p>xff: {}</p></body></html>",
            edge,
            headers
                .get("CF-Connecting-IP")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-"),
            headers
                .get("X-Forwarded-For")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-")
        );
        (
            axum::http::StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/html"),
            )],
            body,
        )
            .into_response()
    }
}

pub async fn visitor_debug_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    const MAX_FILE_PREVIEW: usize = 20;

    let edge_ip = addr.ip().to_string();
    let real_ip = real_client_ip(&headers, &addr);
    let extracted_ip = extract_client_ip(&headers, Some(addr.ip()));
    let trusted = headers_trusted(&headers, Some(addr.ip()));
    trace!(
        edge_ip,
        real_ip, extracted_ip, trusted, "visitor debug requested"
    );

    let version_label = |version: IpVersion| match version {
        IpVersion::V4 => "v4",
        IpVersion::V6 => "v6",
    };

    let edge_hash = state
        .hash_ip(&edge_ip)
        .map(|(version, hash)| json!({ "version": version_label(version), "value": hash }));

    let real_hash_tuple = state.hash_ip(&real_ip);
    let real_hash = real_hash_tuple
        .as_ref()
        .map(|(version, hash)| json!({ "version": version_label(*version), "value": hash }));
    let owner_hash = real_hash_tuple.as_ref().map(|(_, hash)| hash.clone());

    let extracted_hash = if extracted_ip == real_ip {
        real_hash.clone()
    } else {
        state
            .hash_ip(&extracted_ip)
            .map(|(version, hash)| json!({ "version": version_label(version), "value": hash }))
    };

    let forwarded = json!({
        "cf_connecting_ip": headers
            .get("CF-Connecting-IP")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        "true_client_ip": headers
            .get("True-Client-IP")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        "x_real_ip": headers
            .get("X-Real-IP")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        "x_forwarded_for": headers
            .get("X-Forwarded-For")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
    });

    let mut header_dump: HashMap<String, Vec<String>> = HashMap::new();
    for (name, value) in headers.iter() {
        header_dump
            .entry(name.to_string())
            .or_insert_with(Vec::new)
            .push(
                value
                    .to_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|_| format!("{:?}", value)),
            );
    }

    let now = now_secs();
    let mut owned_files = Vec::new();
    let mut owned_total = 0usize;
    if let Some(owner_hash_value) = owner_hash.as_ref() {
        for entry in state.owners.iter() {
            if entry.value().owner_hash == *owner_hash_value {
                owned_total += 1;
                if owned_files.len() < MAX_FILE_PREVIEW {
                    owned_files.push(json!({
                        "file": entry.key().clone(),
                        "original": entry.value().original,
                        "created": entry.value().created,
                        "expires": entry.value().expires,
                        "seconds_until_expiry": entry.value().expires.saturating_sub(now),
                        "content_hash": entry.value().hash,
                    }));
                }
            }
        }
    }
    let files_truncated = owned_total > owned_files.len();

    let mut ban_source = real_ip.clone();
    let mut ban_detail = state.find_ban_for_input(&ban_source).await;
    if ban_detail.is_none() && extracted_ip != ban_source {
        ban_source = extracted_ip.clone();
        ban_detail = state.find_ban_for_input(&ban_source).await;
    }
    let ban_json = ban_detail.map(|ban| {
        let subject = match ban.subject {
            BanSubject::Exact { hash } => {
                json!({ "mode": "exact", "hash": hash })
            }
            BanSubject::Network {
                hash,
                prefix,
                version,
            } => {
                json!({
                    "mode": "network",
                    "hash": hash,
                    "prefix": prefix,
                    "version": version_label(version),
                })
            }
        };
        json!({
            "reason": ban.reason,
            "label": ban.label,
            "time": ban.time,
            "subject": subject,
        })
    });

    let payload = json!({
        "timestamp": now,
        "production": state.production,
        "edge_ip": edge_ip,
        "client": {
            "real_ip": real_ip,
            "extracted_ip": extracted_ip,
            "headers_trusted": trusted,
            "forwarded": forwarded,
            "hash": real_hash,
            "extracted_hash": extracted_hash,
            "edge_hash": edge_hash,
        },
        "owner": {
            "hash": owner_hash,
            "active_count": owned_total,
            "active_limit": MAX_ACTIVE_FILES_PER_IP,
            "files_preview": owned_files,
            "files_truncated": files_truncated,
        },
        "ban": ban_json,
        "ban_lookup_ip": ban_source,
        "headers": header_dump,
    });

    debug!(
        edge_ip,
        real_ip,
        owned_total,
        banned = ban_json.is_some(),
        "visitor debug response prepared"
    );
    Json(payload).into_response()
}

pub async fn faq_handler(
    State(state): State<AppState>,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");
    trace!(lang, "rendering faq page");
    render_tera_page(&state, "faq.html.tera", lang, None).await
}

pub async fn terms_handler(
    State(state): State<AppState>,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");
    trace!(lang, "rendering terms page");
    render_tera_page(&state, "terms.html.tera", lang, None).await
}

pub async fn report_page_handler_i18n(
    State(state): State<AppState>,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");
    trace!(lang, "rendering report page");
    render_tera_page(&state, "report.html.tera", lang, None).await
}

pub async fn simple_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");
    trace!(lang, "rendering simple upload page");

    let message = if let Some(_) = query.deleted {
        Some("File DEleted Successfully.".to_string())
    } else {
        query.m.clone()
    };

    let client_ip = real_client_ip(&headers, &addr);
    let owner_hash = match state.hash_ip_to_string(&client_ip) {
        Some(hash) => hash,
        #[allow(non_snake_case)]
        None => {
            warn!(%client_ip, "simple page access denied: unable to hash ip");
            return (
                StatusCode::FORBIDDEN,
                [(axum::http::header::CONTENT_TYPE, "text/html")],
                "<html><body><h1>Access denied</h1><p>Unable to fingerprint client.</p></body></html>",
            )
                .into_response();
        }
    };
    let mut files: Vec<(String, u64, String)> = state
        .owners
        .iter()
        .filter_map(|entry| {
            let m = entry.value();
            if m.owner_hash == owner_hash {
                Some((entry.key().clone(), m.expires, m.original.clone()))
            } else {
                None
            }
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let now = now_secs();
    let mut rows = String::new();
    for (fname, expires, original) in &files {
        let url = qualify_path(&state, &format!("f/{}", fname));
        let expired = now >= *expires;
        let expires_in = if *expires > now { *expires - now } else { 0 };
        let human = if expired {
            "expired".to_string()
        } else if expires_in >= 86400 {
            format!("{}d", expires_in / 86400)
        } else if expires_in >= 3600 {
            format!("{}h", expires_in / 3600)
        } else if expires_in >= 60 {
            format!("{}m", expires_in / 60)
        } else {
            format!("{}s", expires_in)
        };
        rows.push_str(&format!(
            "<tr><td><a href=\"{}\" data-lang-skip=\"true\">{}</a></td><td>{}</td><td><a href=\"/simple/delete?f={}\" class=delete-link>Delete</a></td></tr>",
            url,
            htmlescape::encode_minimal(original),
            human,
            htmlescape::encode_minimal(fname)
        ));
    }
    let mut ctx = tera::Context::new();
    ctx.insert("lang", lang);
    ctx.insert("ROWS", &rows);
    if let Some(msg) = message {
        ctx.insert("MESSAGE", &msg);
    }
    let t_map = load_translation_map(lang).await;
    ctx.insert("t", &t_map);
    apply_manifest_assets(&state, &mut ctx).await;
    let tera = &state.tera;
    match tera.render("simple.html.tera", &ctx) {
        Ok(rendered) => {
            debug!(lang, files = files.len(), "rendered simple page");
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "text/html")],
                rendered,
            )
                .into_response()
        }
        Err(e) => {
            error!(?e, "failed to render simple template");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("template error: {}", e),
            )
                .into_response()
        }
    }
}

pub async fn banned_handler(
    State(state): State<AppState>,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");
    trace!(lang, "rendering banned page");
    render_tera_page(&state, "banned.html.tera", lang, None).await
}
// piece of shit function 
// todo: avoid using println, use tracing
pub async fn load_translation_map(lang: &str) -> HashMap<String, String> {
    let lang_file = format!("translations/lang_{}.toml", lang);
    println!("[i18n] Attempting to load translation file: {}", lang_file);
    let content = match fs::read_to_string(&lang_file).await {
        Ok(s) => {
            println!("[i18n] Loaded file: {}", lang_file);
            s
        }
        Err(e) => {
            println!(
                "[i18n] Failed to load {}: {}. Falling back to lang_en.toml",
                lang_file, e
            );
            match fs::read_to_string("translations/lang_en.toml").await {
                Ok(s) => s,
                Err(e2) => {
                    println!("[i18n] Failed to load fallback"); // ur fucked
                    String::new()
                }
            }
        }
    };
    let parsed = match toml::from_str::<HashMap<String, String>>(&content) {
        Ok(map) => {
            println!("[i18n] Loaded {} keys for lang {}", map.len(), lang);
            map
        }
        Err(e) => {
            println!("[i18n] Failed to parse TOML for lang {}: {}", lang, e);
            HashMap::new()
        }
    };
    let max_str = format_bytes(max_file_bytes());
    parsed
        .into_iter()
        .map(|(k, v)| (k, v.replace("{max_size}", &max_str)))
        .collect()
}

pub async fn render_tera_page(
    state: &AppState,
    template: &str,
    lang: &str,
    extra: Option<(&str, &tera::Value)>,
) -> Response {
    let t_map = load_translation_map(lang).await;
    trace!(template, lang, "rendering tera page");
    let mut ctx = Context::new();
    ctx.insert("lang", lang);
    ctx.insert("t", &t_map);
    ctx.insert("max_file_bytes", &max_file_bytes());
    ctx.insert("max_file_size_str", &format_bytes(max_file_bytes()));
    apply_manifest_assets(state, &mut ctx).await;
    if let Some((k, v)) = extra {
        ctx.insert(k, v);
    }
    let tera = &state.tera;
    match tera.render(template, &ctx) {
        Ok(rendered) => {
            debug!(template, lang, "rendered tera page");
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "text/html")],
                rendered,
            )
                .into_response()
        }
        Err(e) => {
            error!(?e, template, "failed to render tera page");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("template error: {}", e),
            )
                .into_response()
        }
    }
}
