use std::collections::HashMap;

use serde::Serialize;
use sqlx::SqlitePool;

use crate::config::AppConfig;

use super::now;

pub const KEY_MAX_BODY_BYTES: &str = "max_body_bytes";
pub const KEY_RESPONSE_HEADER_TIMEOUT_MS: &str = "response_header_timeout_ms";
pub const KEY_FIRST_TOKEN_TIMEOUT_MS: &str = "first_token_timeout_ms";
pub const KEY_MAX_ATTEMPTS: &str = "max_attempts";
pub const KEY_AUTO_RETRY_ENABLED: &str = "auto_retry_enabled";

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeSettings {
    pub max_body_bytes: i64,
    pub response_header_timeout_ms: i64,
    pub first_token_timeout_ms: i64,
    pub max_attempts: i64,
    pub auto_retry_enabled: bool,
}

impl RuntimeSettings {
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            max_body_bytes: config.default_max_body_bytes,
            response_header_timeout_ms: config.default_response_header_timeout_ms,
            first_token_timeout_ms: config.default_first_token_timeout_ms,
            max_attempts: config.default_max_attempts,
            auto_retry_enabled: true,
        }
    }

    pub fn max_attempts_for_request(&self) -> usize {
        if self.auto_retry_enabled {
            self.max_attempts.max(1) as usize
        } else {
            1
        }
    }
}

pub async fn ensure_defaults(pool: &SqlitePool, config: &AppConfig) -> Result<(), sqlx::Error> {
    let defaults = RuntimeSettings::from_config(config);
    let mut tx = pool.begin().await?;
    for (key, value) in [
        (KEY_MAX_BODY_BYTES, defaults.max_body_bytes.to_string()),
        (
            KEY_RESPONSE_HEADER_TIMEOUT_MS,
            defaults.response_header_timeout_ms.to_string(),
        ),
        (
            KEY_FIRST_TOKEN_TIMEOUT_MS,
            defaults.first_token_timeout_ms.to_string(),
        ),
        (KEY_MAX_ATTEMPTS, defaults.max_attempts.to_string()),
        (
            KEY_AUTO_RETRY_ENABLED,
            defaults.auto_retry_enabled.to_string(),
        ),
    ] {
        sqlx::query(
            r#"
            INSERT OR IGNORE INTO settings (key, value, updated_at)
            VALUES (?1, ?2, ?3)
            "#,
        )
        .bind(key)
        .bind(value)
        .bind(now())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn get_runtime_settings(
    pool: &SqlitePool,
    config: &AppConfig,
) -> Result<RuntimeSettings, sqlx::Error> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM settings")
        .fetch_all(pool)
        .await?;
    let values: HashMap<String, String> = rows.into_iter().collect();
    let defaults = RuntimeSettings::from_config(config);

    Ok(RuntimeSettings {
        max_body_bytes: read_i64(&values, KEY_MAX_BODY_BYTES, defaults.max_body_bytes),
        response_header_timeout_ms: read_i64(
            &values,
            KEY_RESPONSE_HEADER_TIMEOUT_MS,
            defaults.response_header_timeout_ms,
        ),
        first_token_timeout_ms: read_i64(
            &values,
            KEY_FIRST_TOKEN_TIMEOUT_MS,
            defaults.first_token_timeout_ms,
        ),
        max_attempts: read_i64(&values, KEY_MAX_ATTEMPTS, defaults.max_attempts),
        auto_retry_enabled: read_bool(&values, KEY_AUTO_RETRY_ENABLED, defaults.auto_retry_enabled),
    })
}

pub async fn save_runtime_settings(
    pool: &SqlitePool,
    settings: &RuntimeSettings,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for (key, value) in [
        (KEY_MAX_BODY_BYTES, settings.max_body_bytes.to_string()),
        (
            KEY_RESPONSE_HEADER_TIMEOUT_MS,
            settings.response_header_timeout_ms.to_string(),
        ),
        (
            KEY_FIRST_TOKEN_TIMEOUT_MS,
            settings.first_token_timeout_ms.to_string(),
        ),
        (KEY_MAX_ATTEMPTS, settings.max_attempts.to_string()),
        (
            KEY_AUTO_RETRY_ENABLED,
            settings.auto_retry_enabled.to_string(),
        ),
    ] {
        sqlx::query(
            r#"
            INSERT INTO settings (key, value, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(key)
        .bind(value)
        .bind(now())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

fn read_i64(values: &HashMap<String, String>, key: &str, default: i64) -> i64 {
    values
        .get(key)
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn read_bool(values: &HashMap<String, String>, key: &str, default: bool) -> bool {
    values
        .get(key)
        .map(|value| matches!(value.as_str(), "1" | "true" | "on" | "yes"))
        .unwrap_or(default)
}
