use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use oai_proxy::{
    app,
    config::AppConfig,
    storage::{
        self,
        records::{FinishAttempt, NewAttemptRecord, NewRequestRecord},
    },
};
use std::path::PathBuf;
use tower::ServiceExt;

#[tokio::test]
async fn metrics_returns_request_and_timeout_counters() -> anyhow::Result<()> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    seed_timeout_record(&pool).await?;
    let state = app::AppState::new(config, pool)?;
    let router = app::router(state);

    let response = router
        .oneshot(Request::builder().uri("/metrics").body(Body::empty())?)
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("\"requests_total\":1"));
    assert!(text.contains("\"attempts_first_token_timeout\":1"));
    Ok(())
}

async fn seed_timeout_record(pool: &sqlx::SqlitePool) -> anyhow::Result<()> {
    storage::records::create_request(
        pool,
        &NewRequestRecord {
            id: "req-1".to_string(),
            method: "POST".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            model: Some("model-a".to_string()),
        },
    )
    .await?;
    storage::records::create_attempt(
        pool,
        &NewAttemptRecord {
            id: "attempt-1".to_string(),
            request_id: "req-1".to_string(),
            attempt_index: 1,
            upstream_id: Some(1),
            upstream_name: "default".to_string(),
        },
    )
    .await?;
    storage::records::finish_attempt(
        pool,
        "attempt-1",
        &FinishAttempt {
            status: "first_token_timeout".to_string(),
            http_status: Some(200),
            response_header_ms: Some(10),
            first_token_ms: Some(50),
            timeout_reason: Some("first_token_timeout".to_string()),
            error_message: Some("timeout".to_string()),
            emitted_to_client: false,
        },
    )
    .await?;
    storage::records::complete_request(
        pool,
        "req-1",
        "exhausted_timeout",
        Some("default"),
        1,
        Some(504),
        Some("timeout"),
    )
    .await?;
    Ok(())
}

fn test_config() -> AppConfig {
    AppConfig {
        bind_host: "127.0.0.1".to_string(),
        database_url: "sqlite::memory:".to_string(),
        admin_token: "admin".to_string(),
        admin_token_is_default: true,
        admin_session_token: "session".to_string(),
        data_dir: PathBuf::from("data"),
        default_max_body_bytes: 1024 * 1024,
        default_response_header_timeout_ms: 1000,
        default_first_token_timeout_ms: 1000,
        default_max_attempts: 2,
    }
}
