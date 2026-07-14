use oai_proxy::{
    config::AppConfig,
    storage::{
        self,
        records::{FinishAttempt, NewAttemptRecord, NewRequestRecord},
    },
};
use std::path::PathBuf;

#[tokio::test]
async fn request_and_attempt_records_are_listed_newest_first() -> anyhow::Result<()> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;

    storage::records::create_request(
        &pool,
        &NewRequestRecord {
            id: "req-1".to_string(),
            method: "POST".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            model: Some("model-a".to_string()),
        },
    )
    .await?;
    storage::records::create_attempt(
        &pool,
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
        &pool,
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
        &pool,
        "req-1",
        "exhausted_timeout",
        Some("default"),
        1,
        Some(504),
        Some("timeout"),
    )
    .await?;
    storage::records::save_request_body(&pool, "req-1", br#"{"model":"model-a"}"#).await?;
    storage::records::save_response_body(&pool, "req-1", b"data: done\n\n").await?;

    let requests = storage::records::list_recent_requests(&pool, 10).await?;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].status, "exhausted_timeout");
    assert_eq!(requests[0].attempt_count, 1);

    let attempts = storage::records::list_attempts(&pool, "req-1").await?;
    assert_eq!(attempts.len(), 1);
    assert_eq!(
        attempts[0].timeout_reason.as_deref(),
        Some("first_token_timeout")
    );
    let payload = storage::records::get_payload(&pool, "req-1")
        .await?
        .expect("payload is saved");
    assert_eq!(payload.request_body, br#"{"model":"model-a"}"#);
    assert_eq!(payload.request_body_bytes, 19);
    assert_eq!(payload.request_body_complete, 1);
    assert_eq!(payload.request_body_error, None);
    assert_eq!(payload.response_body, b"data: done\n\n");
    assert_eq!(payload.response_body_bytes, 12);
    assert_eq!(payload.response_body_complete, 1);
    assert_eq!(payload.response_body_error, None);
    Ok(())
}

#[tokio::test]
async fn request_records_can_be_listed_by_page() -> anyhow::Result<()> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;

    for index in 0..30 {
        let request_id = format!("req-{index:02}");
        storage::records::create_request(
            &pool,
            &NewRequestRecord {
                id: request_id.clone(),
                method: "POST".to_string(),
                endpoint: "/responses".to_string(),
                model: Some("model-a".to_string()),
            },
        )
        .await?;
        sqlx::query("UPDATE request_records SET created_at = ?2 WHERE id = ?1")
            .bind(&request_id)
            .bind(format!("2026-07-14T00:00:{index:02}.000Z"))
            .execute(&pool)
            .await?;
    }

    let page = storage::records::list_requests_page(&pool, 10, 10).await?;
    assert_eq!(page.len(), 10);
    assert_eq!(page[0].id, "req-19");
    assert_eq!(page[9].id, "req-10");
    Ok(())
}

#[tokio::test]
async fn traffic_stats_summarize_latencies_and_timeout_attempts() -> anyhow::Result<()> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;

    seed_request_with_attempt(
        &pool,
        SeedAttempt {
            request_id: "req-success",
            attempt_id: "attempt-success",
            request_status: "success",
            attempt_status: "success",
            response_header_ms: Some(12),
            first_token_ms: Some(35),
            timeout_reason: None,
            emitted_to_client: true,
            duration_ms: 120,
        },
    )
    .await?;
    seed_request_with_attempt(
        &pool,
        SeedAttempt {
            request_id: "req-token-timeout",
            attempt_id: "attempt-token-timeout",
            request_status: "exhausted_timeout",
            attempt_status: "first_token_timeout",
            response_header_ms: Some(8),
            first_token_ms: Some(1000),
            timeout_reason: Some("first_token_timeout"),
            emitted_to_client: false,
            duration_ms: 340,
        },
    )
    .await?;
    seed_request_with_attempt(
        &pool,
        SeedAttempt {
            request_id: "req-header-timeout",
            attempt_id: "attempt-header-timeout",
            request_status: "exhausted_timeout",
            attempt_status: "response_header_timeout",
            response_header_ms: None,
            first_token_ms: None,
            timeout_reason: Some("response_header_timeout"),
            emitted_to_client: false,
            duration_ms: 220,
        },
    )
    .await?;

    let stats = storage::records::traffic_stats(&pool).await?;
    assert_eq!(stats.first_token_min_ms, Some(35));
    assert_eq!(stats.first_token_max_ms, Some(35));
    assert_eq!(stats.response_min_ms, Some(120));
    assert_eq!(stats.response_max_ms, Some(340));
    assert_eq!(stats.timeout_filtered_attempts, 2);
    assert_eq!(stats.response_header_timeout_attempts, 1);
    assert_eq!(stats.first_token_timeout_attempts, 1);
    Ok(())
}

struct SeedAttempt<'a> {
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

async fn seed_request_with_attempt(
    pool: &sqlx::SqlitePool,
    seed: SeedAttempt<'_>,
) -> anyhow::Result<()> {
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
