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

use crate::state::AppState;
use crate::util::{format_bytes, max_file_bytes, now_secs, qualify_path, real_client_ip};

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

pub async fn root_handler(
    State(state): State<AppState>,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");
    let t_map = load_translation_map(lang).await;
    let mut ctx = Context::new();
    ctx.insert("lang", lang);
    ctx.insert("t", &t_map);
    ctx.insert("max_file_bytes", &max_file_bytes());
    ctx.insert("max_file_size_str", &format_bytes(max_file_bytes()));
    let manifest_path = state.static_dir.join("dist/manifest.json");
    if let Ok(manifest_str) = fs::read_to_string(&manifest_path).await {
        if let Ok(manifest_map) = serde_json::from_str::<HashMap<String, String>>(&manifest_str) {
            if let Some(app_bundle) = manifest_map.get("app") {
                ctx.insert("app_bundle", app_bundle);
            }
        }
    }
    let tera = &state.tera;
    match tera.render("index.html.tera", &ctx) {
        Ok(rendered) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "text/html")],
            rendered,
        )
            .into_response(),
        Err(e) => {
            eprintln!(
                "[Tera] Error rendering template 'index.html.tera': {e}\n{:#?}",
                e
            );
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
    Json(json!({"edge": edge, "cf": cf, "xff": xff})).into_response()
}

pub async fn faq_handler(
    State(state): State<AppState>,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");
    render_tera_page(&state, "faq.html.tera", lang, None).await
}

pub async fn terms_handler(
    State(state): State<AppState>,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");
    render_tera_page(&state, "terms.html.tera", lang, None).await
}

pub async fn report_page_handler_i18n(
    State(state): State<AppState>,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");
    render_tera_page(&state, "report.html.tera", lang, None).await
}

pub async fn simple_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = query.lang.as_deref().unwrap_or("en");

    let message = if let Some(_) = query.deleted {
        Some("File DEleted Successfully.".to_string())
    } else {
        query.m.clone()
    };

    let client_ip = real_client_ip(&headers, &addr);
    let mut files: Vec<(String, u64, String)> = state
        .owners
        .iter()
        .filter_map(|entry| {
            let m = entry.value();
            if m.owner == client_ip {
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
            "<tr><td><a href=\"{}\">{}</a></td><td>{}</td><td><a href=\"/simple/delete?f={}\" class=delete-link>Delete</a></td></tr>",
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
    let tera = &state.tera;
    match tera.render("simple.html.tera", &ctx) {
        Ok(rendered) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "text/html")],
            rendered,
        )
            .into_response(),
        Err(e) => {
            eprintln!(
                "[Tera] Error rendering template 'simple.html.tera': {e}\n{:#?}",
                e
            );
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
    render_tera_page(&state, "banned.html.tera", lang, None).await
}

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
                    println!("[i18n] Failed to load fallback lang_en.toml: {}", e2);
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
    let mut ctx = Context::new();
    ctx.insert("lang", lang);
    ctx.insert("t", &t_map);
    ctx.insert("max_file_bytes", &max_file_bytes());
    ctx.insert("max_file_size_str", &format_bytes(max_file_bytes()));
    if let Some((k, v)) = extra {
        ctx.insert(k, v);
    }
    let tera = &state.tera;
    match tera.render(template, &ctx) {
        Ok(rendered) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "text/html")],
            rendered,
        )
            .into_response(),
        Err(e) => {
            eprintln!(
                "[Tera] Error rendering template '{}': {e}\n{:#?}",
                template, e
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("template error: {}", e),
            )
                .into_response()
        }
    }
}
