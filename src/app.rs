use axum::{Router, response::Redirect, routing::get};
use reqwest::Client;
use sqlx::SqlitePool;
use tower_http::{services::ServeDir, trace::TraceLayer};

use crate::{
    admin, config::AppConfig, error::AppError, observability, proxy, recording::RecordWriter,
    runtime::RuntimeCache,
};

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub pool: SqlitePool,
    pub http_client: Client,
    pub runtime: RuntimeCache,
    pub record_writer: RecordWriter,
}

impl AppState {
    pub async fn new(config: AppConfig, pool: SqlitePool) -> Result<Self, AppError> {
        let http_client = Client::builder()
            .pool_max_idle_per_host(64)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .no_gzip()
            .no_brotli()
            .no_zstd()
            .no_deflate()
            .build()?;
        let runtime = RuntimeCache::load(&pool, &config).await?;
        let record_writer = RecordWriter::spawn(pool.clone());

        Ok(Self {
            config,
            pool,
            http_client,
            runtime,
            record_writer,
        })
    }

    pub async fn refresh_runtime(&self) -> Result<(), AppError> {
        Ok(self.runtime.refresh(&self.pool, &self.config).await?)
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(observability::metrics::metrics))
        .route("/", get(root))
        .route("/admin", get(admin::handlers::dashboard))
        .route(
            "/admin/settings",
            get(admin::handlers::settings_page).post(admin::handlers::save_settings),
        )
        .route(
            "/admin/upstreams",
            get(admin::handlers::upstreams_page).post(admin::handlers::save_upstream),
        )
        .route("/admin/requests", get(admin::handlers::requests_page))
        .route(
            "/admin/partials/requests",
            get(admin::handlers::requests_partial),
        )
        .nest_service("/static", ServeDir::new("static"))
        .fallback(proxy::routes::proxy_openai)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn root() -> Redirect {
    Redirect::to("/admin")
}
