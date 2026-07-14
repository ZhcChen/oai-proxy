use std::path::PathBuf;

use oai_proxy::{
    config::AppConfig,
    storage::{self, upstreams},
};
use sqlx::{Row, SqlitePool};

#[tokio::test]
async fn legacy_upstreams_migrate_to_one_enabled_base_url_without_keys() -> anyhow::Result<()> {
    let pool = storage::connect("sqlite::memory:").await?;
    apply_sql(&pool, include_str!("../migrations/0001_initial.sql")).await?;

    sqlx::query(
        r#"
        INSERT INTO upstreams (
            name, base_url, api_key, enabled,
            response_header_timeout_ms, first_token_timeout_ms, max_attempts,
            created_at, updated_at
        )
        VALUES
            ('disabled-first', 'https://disabled.example.test', 'old-disabled-key', 0, NULL, NULL, NULL, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
            ('enabled-second', 'https://enabled.example.test', 'old-enabled-key', 1, 111, 222, 3, '2026-01-02T00:00:00Z', '2026-01-02T00:00:00Z'),
            ('enabled-third', 'https://third.example.test', 'old-third-key', 1, 999, 999, 9, '2026-01-03T00:00:00Z', '2026-01-03T00:00:00Z')
        "#,
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO proxy_keys (name, key_secret, enabled, created_at, updated_at)
        VALUES ('legacy-client', 'secret-hash', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')
        "#,
    )
    .execute(&pool)
    .await?;

    apply_sql(
        &pool,
        include_str!("../migrations/0002_drop_proxy_keys.sql"),
    )
    .await?;
    apply_sql(
        &pool,
        include_str!("../migrations/0003_drop_upstream_api_key.sql"),
    )
    .await?;
    apply_sql(
        &pool,
        include_str!("../migrations/0004_remove_max_body_bytes_setting.sql"),
    )
    .await?;
    apply_sql(
        &pool,
        include_str!("../migrations/0005_request_payloads.sql"),
    )
    .await?;
    apply_sql(
        &pool,
        include_str!("../migrations/0006_rebalance_default_timeouts.sql"),
    )
    .await?;

    assert_eq!(table_exists(&pool, "proxy_keys").await?, 0);
    assert_eq!(table_exists(&pool, "request_payloads").await?, 1);
    let upstream_columns = table_columns(&pool, "upstreams").await?;
    assert!(!upstream_columns.iter().any(|column| column == "api_key"));
    assert!(!upstream_columns.iter().any(|column| column == "enabled"));

    let upstreams = upstreams::list_runtime(&pool).await?;
    assert_eq!(upstreams.len(), 1);
    assert_eq!(upstreams[0].name, "default");
    assert_eq!(upstreams[0].base_url, "https://enabled.example.test");
    Ok(())
}

#[tokio::test]
async fn legacy_disabled_only_upstreams_migrate_to_unconfigured_state() -> anyhow::Result<()> {
    let pool = storage::connect("sqlite::memory:").await?;
    apply_sql(&pool, include_str!("../migrations/0001_initial.sql")).await?;

    sqlx::query(
        r#"
        INSERT INTO upstreams (
            name, base_url, api_key, enabled, created_at, updated_at
        )
        VALUES ('disabled-only', 'https://disabled.example.test', 'old-key', 0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')
        "#,
    )
    .execute(&pool)
    .await?;

    apply_sql(
        &pool,
        include_str!("../migrations/0002_drop_proxy_keys.sql"),
    )
    .await?;
    apply_sql(
        &pool,
        include_str!("../migrations/0003_drop_upstream_api_key.sql"),
    )
    .await?;
    apply_sql(
        &pool,
        include_str!("../migrations/0004_remove_max_body_bytes_setting.sql"),
    )
    .await?;
    apply_sql(
        &pool,
        include_str!("../migrations/0005_request_payloads.sql"),
    )
    .await?;
    apply_sql(
        &pool,
        include_str!("../migrations/0006_rebalance_default_timeouts.sql"),
    )
    .await?;

    assert!(upstreams::list_runtime(&pool).await?.is_empty());
    assert_eq!(table_exists(&pool, "request_payloads").await?, 1);
    assert_eq!(upstreams::count_configured(&pool).await?, 0);
    Ok(())
}

#[tokio::test]
async fn legacy_max_body_bytes_setting_is_removed() -> anyhow::Result<()> {
    let pool = storage::connect("sqlite::memory:").await?;
    apply_sql(&pool, include_str!("../migrations/0001_initial.sql")).await?;
    sqlx::query(
        r#"
        INSERT INTO settings (key, value, updated_at)
        VALUES ('max_body_bytes', '4096', '2026-01-01T00:00:00Z')
        "#,
    )
    .execute(&pool)
    .await?;

    apply_sql(
        &pool,
        include_str!("../migrations/0004_remove_max_body_bytes_setting.sql"),
    )
    .await?;

    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM settings WHERE key = 'max_body_bytes'")
        .fetch_one(&pool)
        .await?;
    assert_eq!(row.0, 0);
    Ok(())
}

#[tokio::test]
async fn legacy_default_timeout_settings_migrate_to_new_response_defaults() -> anyhow::Result<()> {
    let pool = storage::connect("sqlite::memory:").await?;
    apply_sql(&pool, include_str!("../migrations/0001_initial.sql")).await?;
    sqlx::query(
        r#"
        INSERT INTO settings (key, value, updated_at)
        VALUES
            ('response_header_timeout_ms', '15000', '2026-01-01T00:00:00Z'),
            ('first_token_timeout_ms', '20000', '2026-01-01T00:00:00Z'),
            ('max_attempts', '3', '2026-01-01T00:00:00Z')
        "#,
    )
    .execute(&pool)
    .await?;

    apply_sql(
        &pool,
        include_str!("../migrations/0006_rebalance_default_timeouts.sql"),
    )
    .await?;

    assert_eq!(
        setting_value(&pool, "response_header_timeout_ms").await?,
        "5000"
    );
    assert_eq!(
        setting_value(&pool, "first_token_timeout_ms").await?,
        "10000"
    );
    assert_eq!(setting_value(&pool, "max_attempts").await?, "3");
    Ok(())
}

#[tokio::test]
async fn custom_timeout_settings_are_not_overwritten_by_default_rebalance() -> anyhow::Result<()> {
    let pool = storage::connect("sqlite::memory:").await?;
    apply_sql(&pool, include_str!("../migrations/0001_initial.sql")).await?;
    sqlx::query(
        r#"
        INSERT INTO settings (key, value, updated_at)
        VALUES
            ('response_header_timeout_ms', '7000', '2026-01-01T00:00:00Z'),
            ('first_token_timeout_ms', '12000', '2026-01-01T00:00:00Z')
        "#,
    )
    .execute(&pool)
    .await?;

    apply_sql(
        &pool,
        include_str!("../migrations/0006_rebalance_default_timeouts.sql"),
    )
    .await?;

    assert_eq!(
        setting_value(&pool, "response_header_timeout_ms").await?,
        "7000"
    );
    assert_eq!(
        setting_value(&pool, "first_token_timeout_ms").await?,
        "12000"
    );
    Ok(())
}

#[tokio::test]
async fn save_single_base_url_replaces_existing_rows() -> anyhow::Result<()> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;

    upstreams::save_single_base_url(&pool, "https://first.example.test").await?;
    sqlx::query(
        r#"
        INSERT INTO upstreams (name, base_url, created_at, updated_at)
        VALUES ('legacy-extra', 'https://extra.example.test', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')
        "#,
    )
    .execute(&pool)
    .await?;

    upstreams::save_single_base_url(&pool, "https://second.example.test").await?;

    let upstreams = upstreams::list_all(&pool).await?;
    assert_eq!(upstreams.len(), 1);
    assert_eq!(upstreams[0].name, "default");
    assert_eq!(upstreams[0].base_url, "https://second.example.test");
    Ok(())
}

async fn apply_sql(pool: &SqlitePool, sql: &str) -> anyhow::Result<()> {
    for statement in sql
        .split(';')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        sqlx::query(statement).execute(pool).await?;
    }
    Ok(())
}

async fn table_exists(pool: &SqlitePool, table: &str) -> anyhow::Result<i64> {
    let row: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1")
            .bind(table)
            .fetch_one(pool)
            .await?;
    Ok(row.0)
}

async fn table_columns(pool: &SqlitePool, table: &str) -> anyhow::Result<Vec<String>> {
    let rows = sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect())
}

async fn setting_value(pool: &SqlitePool, key: &str) -> anyhow::Result<String> {
    let row: (String,) = sqlx::query_as("SELECT value FROM settings WHERE key = ?1")
        .bind(key)
        .fetch_one(pool)
        .await?;
    Ok(row.0)
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
