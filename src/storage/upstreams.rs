use std::env;

use serde::Serialize;
use sqlx::{FromRow, SqlitePool};
use url::Url;

use super::{mask_secret, now};

#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid upstream base URL: {0}")]
    InvalidBaseUrl(String),
}

#[derive(Clone, Debug, FromRow)]
pub struct Upstream {
    pub id: i64,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub enabled: i64,
    pub response_header_timeout_ms: Option<i64>,
    pub first_token_timeout_ms: Option<i64>,
    pub max_attempts: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct UpstreamView {
    pub id: i64,
    pub name: String,
    pub base_url: String,
    pub masked_api_key: String,
    pub enabled: bool,
    pub response_header_timeout_ms: String,
    pub first_token_timeout_ms: String,
    pub max_attempts: String,
    pub created_at: String,
}

impl From<Upstream> for UpstreamView {
    fn from(upstream: Upstream) -> Self {
        Self {
            id: upstream.id,
            name: upstream.name,
            base_url: upstream.base_url,
            masked_api_key: mask_secret(&upstream.api_key),
            enabled: upstream.enabled == 1,
            response_header_timeout_ms: upstream
                .response_header_timeout_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            first_token_timeout_ms: upstream
                .first_token_timeout_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            max_attempts: upstream
                .max_attempts
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            created_at: upstream.created_at,
        }
    }
}

#[derive(Clone, Debug)]
pub struct NewUpstream {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub enabled: bool,
    pub response_header_timeout_ms: Option<i64>,
    pub first_token_timeout_ms: Option<i64>,
    pub max_attempts: Option<i64>,
}

pub async fn seed_from_env(pool: &SqlitePool) -> Result<(), UpstreamError> {
    let Some(base_url) = env::var("OAI_PROXY_UPSTREAM_BASE_URL")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };

    let name = env::var("OAI_PROXY_UPSTREAM_NAME").unwrap_or_else(|_| "default".to_string());
    let api_key = env::var("OAI_PROXY_UPSTREAM_API_KEY").unwrap_or_default();

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM upstreams WHERE name = ?1")
        .bind(&name)
        .fetch_one(pool)
        .await?;

    if count.0 == 0 {
        create(
            pool,
            &NewUpstream {
                name,
                base_url,
                api_key,
                enabled: true,
                response_header_timeout_ms: None,
                first_token_timeout_ms: None,
                max_attempts: None,
            },
        )
        .await?;
    }

    Ok(())
}

pub async fn create(pool: &SqlitePool, upstream: &NewUpstream) -> Result<i64, UpstreamError> {
    let now = now();
    let base_url = normalize_base_url(&upstream.base_url)?;
    let result = sqlx::query(
        r#"
        INSERT INTO upstreams (
            name,
            base_url,
            api_key,
            enabled,
            response_header_timeout_ms,
            first_token_timeout_ms,
            max_attempts,
            created_at,
            updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
        "#,
    )
    .bind(upstream.name.trim())
    .bind(base_url)
    .bind(upstream.api_key.trim())
    .bind(if upstream.enabled { 1 } else { 0 })
    .bind(upstream.response_header_timeout_ms)
    .bind(upstream.first_token_timeout_ms)
    .bind(upstream.max_attempts)
    .bind(now)
    .execute(pool)
    .await?;

    Ok(result.last_insert_rowid())
}

pub fn normalize_base_url(base_url: &str) -> Result<String, UpstreamError> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let parsed =
        Url::parse(trimmed).map_err(|error| UpstreamError::InvalidBaseUrl(error.to_string()))?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(UpstreamError::InvalidBaseUrl(
            "scheme must be http or https".to_string(),
        ));
    }
    if parsed.host_str().is_none() {
        return Err(UpstreamError::InvalidBaseUrl(
            "host is required".to_string(),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(UpstreamError::InvalidBaseUrl(
            "query and fragment are not allowed".to_string(),
        ));
    }

    Ok(trimmed.to_string())
}

pub async fn list_all(pool: &SqlitePool) -> Result<Vec<Upstream>, sqlx::Error> {
    sqlx::query_as::<_, Upstream>(
        r#"
        SELECT id, name, base_url, api_key, enabled, response_header_timeout_ms,
               first_token_timeout_ms, max_attempts, created_at, updated_at
        FROM upstreams
        ORDER BY id ASC
        "#,
    )
    .fetch_all(pool)
    .await
}

pub async fn list_enabled(pool: &SqlitePool) -> Result<Vec<Upstream>, sqlx::Error> {
    sqlx::query_as::<_, Upstream>(
        r#"
        SELECT id, name, base_url, api_key, enabled, response_header_timeout_ms,
               first_token_timeout_ms, max_attempts, created_at, updated_at
        FROM upstreams
        WHERE enabled = 1
        ORDER BY id ASC
        "#,
    )
    .fetch_all(pool)
    .await
}

pub async fn count_enabled(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM upstreams WHERE enabled = 1")
        .fetch_one(pool)
        .await?;
    Ok(row.0)
}
