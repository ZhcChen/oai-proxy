use axum::{
    Router,
    routing::{get, post},
};
use reqwest::Client;
use sqlx::SqlitePool;
use tower_http::{services::ServeDir, trace::TraceLayer};

use crate::{admin, config::AppConfig, observability, proxy};

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub pool: SqlitePool,
    pub http_client: Client,
}

impl AppState {
    pub fn new(config: AppConfig, pool: SqlitePool) -> Result<Self, reqwest::Error> {
        let http_client = Client::builder()
            .user_agent("oai-proxy/0.1")
            .pool_max_idle_per_host(64)
            .build()?;

        Ok(Self {
            config,
            pool,
            http_client,
        })
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(observability::metrics::metrics))
        .route("/", get(admin::handlers::dashboard))
        .route("/admin", get(admin::handlers::dashboard))
        .route(
            "/admin/login",
            get(admin::handlers::login_page).post(admin::handlers::login),
        )
        .route("/admin/logout", post(admin::handlers::logout))
        .route(
            "/admin/settings",
            get(admin::handlers::settings_page).post(admin::handlers::save_settings),
        )
        .route(
            "/admin/upstreams",
            get(admin::handlers::upstreams_page).post(admin::handlers::create_upstream),
        )
        .route("/admin/requests", get(admin::handlers::requests_page))
        .route(
            "/admin/partials/requests",
            get(admin::handlers::requests_partial),
        )
        .route("/v1/chat/completions", post(proxy::routes::proxy_openai))
        .route("/v1/responses", post(proxy::routes::proxy_openai))
        .nest_service("/static", ServeDir::new("static"))
        .fallback(get(not_found))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn not_found() -> (axum::http::StatusCode, &'static str) {
    (axum::http::StatusCode::NOT_FOUND, "not found")
}
