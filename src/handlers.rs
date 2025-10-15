use axum::Router;
use axum::routing::{delete, get, post, put};
use tower_http::services::ServeDir;
use tracing::{debug, info};

use crate::state::AppState;

pub mod admin;
pub mod delete;
pub mod hosting;
pub mod reports;
pub mod security;
pub mod upload;
pub mod web;

pub use admin::{
    AdminAuthForm, AdminFileDeleteForm, AdminReportDeleteForm, BanForm, UnbanForm,
    admin_file_delete_handler, admin_files_handler, admin_report_delete_handler,
    admin_reports_handler, auth_get_handler, auth_post_handler, ban_page_handler, ban_post_handler,
    is_admin_handler, unban_post_handler,
};
pub use delete::{
    SimpleDeleteForm, delete_handler, simple_delete_handler, simple_delete_post_handler,
};
pub use hosting::{ConfigResponse, config_handler, fetch_file_handler, file_handler};
pub use reports::{ReportForm, ReportRecordEmail, report_handler};
pub use security::{add_cache_headers, add_security_headers, ban_gate, enforce_host};
pub use upload::{
    CheckHashQuery, ChunkCompleteRequest, ChunkInitRequest, ChunkInitResponse, FileMetaEntry,
    ListResponse, UploadResponse, cancel_chunk_upload_handler, checkhash_handler,
    chunk_status_handler, complete_chunk_upload_handler, init_chunk_upload_handler, list_handler,
    simple_list_handler, simple_upload_handler, upload_chunk_part_handler, upload_handler,
};
pub use web::{
    LangQuery, SimpleQuery, banned_handler, debug_ip_handler, faq_handler,
    report_page_handler_i18n, root_handler, simple_handler, terms_handler, trusted_handler,
    visitor_debug_handler,
};

#[tracing::instrument(level = "info", skip(state))]
pub fn build_router(state: AppState) -> Router {
    let static_root = state.static_dir.clone();
    info!(?static_root, "building application router");
    let css_service = ServeDir::new(static_root.join("css"));
    let js_service = ServeDir::new(static_root.join("js"));
    let dist_service = ServeDir::new(static_root.join("dist"));
    let router = Router::new()
        .route("/checkhash", get(checkhash_handler))
        .route("/upload", post(upload_handler))
        .route("/chunk/init", post(init_chunk_upload_handler))
        .route("/chunk/{id}/status", get(chunk_status_handler))
        .route("/chunk/{id}/complete", post(complete_chunk_upload_handler))
        .route("/chunk/{id}/cancel", delete(cancel_chunk_upload_handler))
        .route("/chunk/{id}/{index}", put(upload_chunk_part_handler))
        .route("/list", get(list_handler))
        .route("/mine", get(list_handler))
        .route("/f/{file}", get(fetch_file_handler).delete(delete_handler))
        .route("/d/{file}", delete(delete_handler))
        .route(
            "/report",
            get(report_page_handler_i18n).post(report_handler),
        )
        .route("/unban", post(unban_post_handler))
        .route("/healthz", get(|| async { "ok" }))
        .route("/simple", get(simple_handler))
        .route("/simple/upload", post(simple_upload_handler))
        .route(
            "/simple/delete",
            get(simple_delete_handler).post(simple_delete_post_handler),
        )
        .route("/auth", get(auth_get_handler).post(auth_post_handler))
        .route("/isadmin", get(is_admin_handler))
        .route("/debug-ip", get(debug_ip_handler))
        .route("/visitor-debug", get(visitor_debug_handler))
        .route("/trusted", get(trusted_handler))
        .route("/admin/ban", get(ban_page_handler).post(ban_post_handler))
        .route(
            "/admin/files",
            get(admin_files_handler).post(admin_file_delete_handler),
        )
        .route(
            "/admin/reports",
            get(admin_reports_handler).post(admin_report_delete_handler),
        )
        .route("/faq", get(faq_handler))
        .route("/terms", get(terms_handler))
        .route("/api/config", get(config_handler))
        .nest_service("/css", css_service.clone())
        .nest_service("/js", js_service.clone())
        .nest_service("/dist", dist_service.clone())
        .route("/", get(root_handler))
        .route("/{*path}", get(file_handler))
        .with_state(state);
    debug!("router configured with static assets and handlers");
    router
}
