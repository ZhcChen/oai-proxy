use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use oai_proxy::{app, config::AppConfig, storage};
use std::{path::PathBuf, time::Duration};
use tokio::time::sleep;
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
async fn unmatched_route_enters_proxy_fallback() -> anyhow::Result<()> {
    let router = test_router().await?;
    let response = router
        .oneshot(Request::builder().uri("/missing").body(Body::empty())?)
        .await?;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    Ok(())
}

#[tokio::test]
async fn control_plane_requests_do_not_enter_proxy_records() -> anyhow::Result<()> {
    let (router, pool) = test_router_with_pool().await?;
    let cases = [
        (Method::GET, "/admin", StatusCode::OK),
        (Method::GET, "/admin/not-found", StatusCode::NOT_FOUND),
        (Method::POST, "/admin/requests", StatusCode::NOT_FOUND),
        (
            Method::GET,
            "/admin/partials/missing",
            StatusCode::NOT_FOUND,
        ),
        (Method::GET, "/metrics", StatusCode::OK),
        (Method::GET, "/healthz", StatusCode::OK),
        (Method::GET, "/favicon.ico", StatusCode::NOT_FOUND),
        (
            Method::GET,
            "/.well-known/appspecific/com.chrome.devtools.json",
            StatusCode::NOT_FOUND,
        ),
    ];

    for (method, uri, expected_status) in cases {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(
            response.status(),
            expected_status,
            "unexpected status for {uri}"
        );
    }

    sleep(Duration::from_millis(50)).await;
    assert_eq!(storage::records::total_requests(&pool).await?, 0);
    Ok(())
}

#[tokio::test]
async fn proxy_fallback_requests_are_recorded() -> anyhow::Result<()> {
    let (router, pool) = test_router_with_pool().await?;
    let response = router
        .oneshot(Request::builder().uri("/missing").body(Body::empty())?)
        .await?;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    wait_for_total_requests(&pool, 1).await?;
    Ok(())
}

async fn test_router() -> anyhow::Result<axum::Router> {
    Ok(test_router_with_pool().await?.0)
}

async fn test_router_with_pool() -> anyhow::Result<(axum::Router, sqlx::SqlitePool)> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    let state = app::AppState::new(config, pool.clone()).await?;
    Ok((app::router(state), pool))
}

async fn wait_for_total_requests(pool: &sqlx::SqlitePool, expected: i64) -> anyhow::Result<()> {
    for _ in 0..100 {
        let total = storage::records::total_requests(pool).await?;
        if total == expected {
            return Ok(());
        }
        sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("request total did not become {expected}");
}

fn test_config() -> AppConfig {
    AppConfig {
        bind_host: "127.0.0.1".to_string(),
        database_url: "sqlite::memory:".to_string(),
        data_dir: PathBuf::from("data"),
        default_response_header_timeout_ms: 1000,
        default_first_token_timeout_ms: 1000,
        default_max_attempts: 2,
    }
}
