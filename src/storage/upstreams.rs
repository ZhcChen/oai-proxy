use std::env;

use serde::Serialize;
use sqlx::{FromRow, SqlitePool};
use url::Url;

use super::now;

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
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct UpstreamView {
    pub id: i64,
    pub name: String,
    pub base_url: String,
    pub created_at: String,
}

impl From<Upstream> for UpstreamView {
    fn from(upstream: Upstream) -> Self {
        Self {
            id: upstream.id,
            name: upstream.name,
            base_url: upstream.base_url,
            created_at: upstream.created_at,
        }
    }
}

pub async fn seed_from_env(pool: &SqlitePool) -> Result<(), UpstreamError> {
    let Some(base_url) = env::var("OAI_PROXY_UPSTREAM_BASE_URL")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };

    if get_configured(pool).await?.is_none() {
        save_single_base_url(pool, &base_url).await?;
    }

    Ok(())
}

pub async fn save_single_base_url(pool: &SqlitePool, base_url: &str) -> Result<(), UpstreamError> {
    let normalized = normalize_base_url(base_url)?;
    let now = now();
    let mut tx = pool.begin().await?;

    if let Some(existing) = get_configured_in_tx(&mut tx).await? {
        sqlx::query(
            r#"
            UPDATE upstreams
            SET name = 'default',
                base_url = ?2,
                updated_at = ?3
            WHERE id = ?1
            "#,
        )
        .bind(existing.id)
        .bind(normalized)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM upstreams WHERE id != ?1")
            .bind(existing.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(());
    }

    sqlx::query(
        r#"
        INSERT INTO upstreams (
            name,
            base_url,
            created_at,
            updated_at
        )
        VALUES (?1, ?2, ?3, ?3)
        "#,
    )
    .bind("default")
    .bind(normalized)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
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
        SELECT id, name, base_url, created_at, updated_at
        FROM upstreams
        ORDER BY id ASC
        "#,
    )
    .fetch_all(pool)
    .await
}

pub async fn get_configured(pool: &SqlitePool) -> Result<Option<Upstream>, sqlx::Error> {
    sqlx::query_as::<_, Upstream>(
        r#"
        SELECT id, name, base_url, created_at, updated_at
        FROM upstreams
        ORDER BY id ASC
        LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await
}

pub async fn list_runtime(pool: &SqlitePool) -> Result<Vec<Upstream>, sqlx::Error> {
    Ok(get_configured(pool).await?.into_iter().collect())
}

pub async fn count_configured(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    Ok(if get_configured(pool).await?.is_some() {
        1
    } else {
        0
    })
}

async fn get_configured_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<Option<Upstream>, sqlx::Error> {
    sqlx::query_as::<_, Upstream>(
        r#"
        SELECT id, name, base_url, created_at, updated_at
        FROM upstreams
        ORDER BY id ASC
        LIMIT 1
        "#,
    )
    .fetch_optional(&mut **tx)
    .await
}
