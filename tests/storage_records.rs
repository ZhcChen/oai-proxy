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
    Ok(())
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
