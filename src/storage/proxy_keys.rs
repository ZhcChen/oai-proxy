use std::env;

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqlitePool};

use super::now;

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct ProxyKeyView {
    pub id: i64,
    pub name: String,
    pub enabled: i64,
    pub created_at: String,
}

pub async fn seed_from_env(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let Some(key_secret) = env::var("OAI_PROXY_PROXY_KEY")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };

    let name = env::var("OAI_PROXY_PROXY_KEY_NAME").unwrap_or_else(|_| "default".to_string());
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM proxy_keys WHERE name = ?1")
        .bind(&name)
        .fetch_one(pool)
        .await?;

    if count.0 == 0 {
        create(pool, &name, &key_secret).await?;
    }

    Ok(())
}

pub async fn create(pool: &SqlitePool, name: &str, key_secret: &str) -> Result<i64, sqlx::Error> {
    let now = now();
    let result = sqlx::query(
        r#"
        INSERT INTO proxy_keys (name, key_secret, enabled, created_at, updated_at)
        VALUES (?1, ?2, 1, ?3, ?3)
        "#,
    )
    .bind(name.trim())
    .bind(hash_secret(key_secret.trim()))
    .bind(now)
    .execute(pool)
    .await?;

    Ok(result.last_insert_rowid())
}

pub async fn name_exists(pool: &SqlitePool, name: &str) -> Result<bool, sqlx::Error> {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM proxy_keys WHERE name = ?1")
        .bind(name.trim())
        .fetch_one(pool)
        .await?;
    Ok(row.0 > 0)
}

pub async fn list_all(pool: &SqlitePool) -> Result<Vec<ProxyKeyView>, sqlx::Error> {
    sqlx::query_as::<_, ProxyKeyView>(
        r#"
        SELECT id, name, enabled, created_at
        FROM proxy_keys
        ORDER BY id DESC
        "#,
    )
    .fetch_all(pool)
    .await
}

pub async fn is_authorized(
    pool: &SqlitePool,
    bearer_token: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let enabled_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM proxy_keys WHERE enabled = 1")
        .fetch_one(pool)
        .await?;

    if enabled_count.0 == 0 {
        return Ok(true);
    }

    let Some(token) = bearer_token.filter(|value| !value.is_empty()) else {
        return Ok(false);
    };

    let matched: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM proxy_keys WHERE enabled = 1 AND key_secret = ?1")
            .bind(hash_secret(token))
            .fetch_one(pool)
            .await?;

    Ok(matched.0 > 0)
}

fn hash_secret(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}
