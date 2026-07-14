use oai_proxy::{
    config::AppConfig,
    storage::{self, settings::RuntimeSettings},
};
use std::path::PathBuf;

#[tokio::test]
async fn ensure_defaults_and_update_runtime_settings() -> anyhow::Result<()> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;

    let defaults = storage::settings::get_runtime_settings(&pool, &config).await?;
    assert_eq!(defaults.first_token_timeout_ms, 456);
    assert!(!defaults.policy_enabled);
    assert!(defaults.request_record_enabled);
    assert!(defaults.auto_retry_enabled);

    storage::settings::save_runtime_settings(
        &pool,
        &RuntimeSettings {
            policy_enabled: false,
            request_record_enabled: false,
            response_header_timeout_ms: 111,
            first_token_timeout_ms: 222,
            max_attempts: 1,
            auto_retry_enabled: false,
        },
    )
    .await?;

    let updated = storage::settings::get_runtime_settings(&pool, &config).await?;
    assert!(!updated.policy_enabled);
    assert!(!updated.request_record_enabled);
    assert_eq!(updated.response_header_timeout_ms, 111);
    assert_eq!(updated.first_token_timeout_ms, 222);
    assert_eq!(updated.max_attempts, 1);
    assert!(!updated.auto_retry_enabled);
    Ok(())
}

fn test_config() -> AppConfig {
    AppConfig {
        bind_host: "127.0.0.1".to_string(),
        database_url: "sqlite::memory:".to_string(),
        data_dir: PathBuf::from("data"),
        default_response_header_timeout_ms: 123,
        default_first_token_timeout_ms: 456,
        default_max_attempts: 3,
    }
}
