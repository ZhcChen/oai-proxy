use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use oai_proxy::{app, config::AppConfig, storage};
use std::path::PathBuf;
use tower::ServiceExt;

#[tokio::test]
async fn healthz_returns_ok() -> anyhow::Result<()> {
    let router = test_router().await?;
    let response = router
        .oneshot(Request::builder().uri("/healthz").body(Body::empty())?)
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(&body[..], b"ok");
    Ok(())
}

#[tokio::test]
async fn unknown_route_returns_404() -> anyhow::Result<()> {
    let router = test_router().await?;
    let response = router
        .oneshot(Request::builder().uri("/missing").body(Body::empty())?)
        .await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

async fn test_router() -> anyhow::Result<axum::Router> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    let state = app::AppState::new(config, pool)?;
    Ok(app::router(state))
}

fn test_config() -> AppConfig {
    AppConfig {
        bind_host: "127.0.0.1".to_string(),
        database_url: "sqlite::memory:".to_string(),
        data_dir: PathBuf::from("data"),
        default_max_body_bytes: 1024 * 1024,
        default_response_header_timeout_ms: 1000,
        default_first_token_timeout_ms: 1000,
        default_max_attempts: 2,
    }
}
