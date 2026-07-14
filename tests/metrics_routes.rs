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
    let state = app::AppState::new(config, pool).await?;
    let router = app::router(state);

    let response = router
        .oneshot(Request::builder().uri("/metrics").body(Body::empty())?)
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("\"requests_total\":3"));
    assert!(text.contains("\"first_token_min_ms\":25"));
    assert!(text.contains("\"first_token_max_ms\":25"));
    assert!(text.contains("\"response_min_ms\":100"));
    assert!(text.contains("\"response_max_ms\":200"));
    assert!(text.contains("\"timeout_filtered_attempts\":2"));
    assert!(text.contains("\"attempts_response_header_timeout\":1"));
    assert!(text.contains("\"attempts_first_token_timeout\":1"));
    Ok(())
}

async fn seed_timeout_record(pool: &sqlx::SqlitePool) -> anyhow::Result<()> {
    seed_attempt(
        pool,
        SeedMetricAttempt {
            request_id: "req-success",
            attempt_id: "attempt-success",
            request_status: "success",
            attempt_status: "success",
            response_header_ms: Some(10),
            first_token_ms: Some(25),
            timeout_reason: None,
            emitted_to_client: true,
            duration_ms: 100,
        },
    )
    .await?;
    seed_attempt(
        pool,
        SeedMetricAttempt {
            request_id: "req-token-timeout",
            attempt_id: "attempt-token-timeout",
            request_status: "exhausted_timeout",
            attempt_status: "first_token_timeout",
            response_header_ms: Some(10),
            first_token_ms: Some(50),
            timeout_reason: Some("first_token_timeout"),
            emitted_to_client: false,
            duration_ms: 200,
        },
    )
    .await?;
    seed_attempt(
        pool,
        SeedMetricAttempt {
            request_id: "req-header-timeout",
            attempt_id: "attempt-header-timeout",
            request_status: "exhausted_timeout",
            attempt_status: "response_header_timeout",
            response_header_ms: None,
            first_token_ms: None,
            timeout_reason: Some("response_header_timeout"),
            emitted_to_client: false,
            duration_ms: 150,
        },
    )
    .await?;
    Ok(())
}

struct SeedMetricAttempt<'a> {
    request_id: &'a str,
    attempt_id: &'a str,
    request_status: &'a str,
    attempt_status: &'a str,
    response_header_ms: Option<i64>,
    first_token_ms: Option<i64>,
    timeout_reason: Option<&'a str>,
    emitted_to_client: bool,
    duration_ms: i64,
}

async fn seed_attempt(pool: &sqlx::SqlitePool, seed: SeedMetricAttempt<'_>) -> anyhow::Result<()> {
    storage::records::create_request(
        pool,
        &NewRequestRecord {
            id: seed.request_id.to_string(),
            method: "POST".to_string(),
            endpoint: "/responses".to_string(),
            model: Some("model-a".to_string()),
        },
    )
    .await?;
    storage::records::create_attempt(
        pool,
        &NewAttemptRecord {
            id: seed.attempt_id.to_string(),
            request_id: seed.request_id.to_string(),
            attempt_index: 1,
            upstream_id: Some(1),
            upstream_name: "default".to_string(),
        },
    )
    .await?;
    storage::records::finish_attempt(
        pool,
        seed.attempt_id,
        &FinishAttempt {
            status: seed.attempt_status.to_string(),
            http_status: Some(200),
            response_header_ms: seed.response_header_ms,
            first_token_ms: seed.first_token_ms,
            timeout_reason: seed.timeout_reason.map(str::to_string),
            error_message: seed.timeout_reason.map(str::to_string),
            emitted_to_client: seed.emitted_to_client,
        },
    )
    .await?;
    storage::records::complete_request(
        pool,
        seed.request_id,
        seed.request_status,
        Some("default"),
        1,
        Some(200),
        seed.timeout_reason,
    )
    .await?;
    sqlx::query("UPDATE request_records SET duration_ms = ?2 WHERE id = ?1")
        .bind(seed.request_id)
        .bind(seed.duration_ms)
        .execute(pool)
        .await?;
    Ok(())
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
