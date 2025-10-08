use axum::extract::{ConnectInfo, Form, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use time::OffsetDateTime;

use crate::state::{AppState, ReportRecord};
use crate::util::{json_error, now_secs, real_client_ip};

#[derive(Clone, Debug)]
pub struct ReportRecordEmail {
    pub file: String,
    pub reason: String,
    pub details: String,
    pub reporter_hash: String,
    pub time: u64,
    pub iso_time: String,
    pub owner_hash: String,
    pub original_name: String,
    pub expires: u64,
    pub size: u64,
    pub report_index: usize,
    pub total_reports_for_file: usize,
    pub total_reports: usize,
}

#[derive(Deserialize)]
pub struct ReportForm {
    pub file: String,
    pub reason: String,
    pub details: Option<String>,
}

#[axum::debug_handler]
pub async fn report_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<ReportForm>,
) -> Response {
    if state.is_banned(&real_client_ip(&headers, &addr)).await {
        return json_error(StatusCode::FORBIDDEN, "banned", "ip banned");
    }
    let ip = real_client_ip(&headers, &addr);
    let Some(reporter_hash) = state.hash_ip_to_string(&ip) else {
        return json_error(
            StatusCode::FORBIDDEN,
            "invalid_ip",
            "unable to fingerprint client",
        );
    };
    let now = now_secs();
    let mut file_name = form.file.trim().to_string();
    if state.owners.get(&file_name).is_none() && !file_name.contains('.') {
        let prefix = format!("{file_name}.");
        let mut candidates: Vec<String> = state
            .owners
            .iter()
            .filter_map(|entry| {
                let k = entry.key();
                if k.starts_with(&prefix) {
                    Some(k.clone())
                } else {
                    None
                }
            })
            .collect();
        candidates.sort();
        candidates.sort_by_key(|k| k.len());
        if let Some(best) = candidates.first() {
            file_name = best.clone();
        }
    }
    let record = ReportRecord {
        file: file_name.clone(),
        reason: form.reason.clone(),
        details: form.details.clone().unwrap_or_default(),
        reporter_hash: reporter_hash.clone(),
        time: now,
    };
    let (owner_hash, original_name, expires, size) = {
        if let Some(meta) = state.owners.get(&record.file) {
            let meta = meta.value();
            let path = state.upload_dir.join(&record.file);
            let sz = tokio::fs::metadata(&path)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            (
                meta.owner_hash.clone(),
                meta.original.clone(),
                meta.expires,
                sz,
            )
        } else {
            (String::new(), String::new(), 0u64, 0u64)
        }
    };
    let (report_index, total_reports_for_file, total_reports) = {
        let mut reports = state.reports.write().await;
        reports.push(record.clone());
        let idx = reports.len() - 1;
        let count_file = reports.iter().filter(|r| r.file == record.file).count();
        let total = reports.len();
        (idx, count_file, total)
    };
    state.persist_reports().await;
    if let Some(tx) = &state.email_tx {
        let iso = OffsetDateTime::from_unix_timestamp(now as i64)
            .map(|t| {
                t.format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let _ = tx
            .send(ReportRecordEmail {
                file: record.file.clone(),
                reason: record.reason.clone(),
                details: record.details.clone(),
                reporter_hash: record.reporter_hash.clone(),
                time: record.time,
                iso_time: iso,
                owner_hash,
                original_name,
                expires,
                size,
                report_index,
                total_reports_for_file,
                total_reports,
            })
            .await;
    }
    (StatusCode::NO_CONTENT, ()).into_response()
}
