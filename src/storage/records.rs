use serde::Serialize;
use sqlx::{FromRow, SqlitePool};

use super::now;

#[derive(Clone, Debug)]
pub struct NewRequestRecord {
    pub id: String,
    pub method: String,
    pub endpoint: String,
    pub model: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NewAttemptRecord {
    pub id: String,
    pub request_id: String,
    pub attempt_index: i64,
    pub upstream_id: Option<i64>,
    pub upstream_name: String,
}

#[derive(Clone, Debug, Default)]
pub struct FinishAttempt {
    pub status: String,
    pub http_status: Option<i64>,
    pub response_header_ms: Option<i64>,
    pub first_token_ms: Option<i64>,
    pub timeout_reason: Option<String>,
    pub error_message: Option<String>,
    pub emitted_to_client: bool,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct RequestRecord {
    pub id: String,
    pub method: String,
    pub endpoint: String,
    pub model: Option<String>,
    pub status: String,
    pub upstream_name: Option<String>,
    pub attempt_count: i64,
    pub final_http_status: Option<i64>,
    pub error_message: Option<String>,
    pub retry_count: i64,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub duration_ms: Option<i64>,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct AttemptRecord {
    pub id: String,
    pub request_id: String,
    pub attempt_index: i64,
    pub upstream_id: Option<i64>,
    pub upstream_name: String,
    pub status: String,
    pub http_status: Option<i64>,
    pub response_header_ms: Option<i64>,
    pub first_token_ms: Option<i64>,
    pub timeout_reason: Option<String>,
    pub error_message: Option<String>,
    pub emitted_to_client: i64,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub duration_ms: Option<i64>,
}

pub async fn create_request(
    pool: &SqlitePool,
    record: &NewRequestRecord,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO request_records (id, method, endpoint, model, status, created_at)
        VALUES (?1, ?2, ?3, ?4, 'started', ?5)
        "#,
    )
    .bind(&record.id)
    .bind(&record.method)
    .bind(&record.endpoint)
    .bind(&record.model)
    .bind(now())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn complete_request(
    pool: &SqlitePool,
    request_id: &str,
    status: &str,
    upstream_name: Option<&str>,
    attempt_count: i64,
    final_http_status: Option<i64>,
    error_message: Option<&str>,
) -> Result<(), sqlx::Error> {
    let completed_at = now();
    sqlx::query(
        r#"
        UPDATE request_records
        SET status = ?2,
            upstream_name = ?3,
            attempt_count = ?4,
            final_http_status = ?5,
            error_message = ?6,
            retry_count = CASE WHEN ?4 > 0 THEN ?4 - 1 ELSE 0 END,
            completed_at = ?7,
            duration_ms = CAST((julianday(?7) - julianday(created_at)) * 86400000 AS INTEGER)
        WHERE id = ?1
        "#,
    )
    .bind(request_id)
    .bind(status)
    .bind(upstream_name)
    .bind(attempt_count)
    .bind(final_http_status)
    .bind(error_message)
    .bind(completed_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn create_attempt(
    pool: &SqlitePool,
    record: &NewAttemptRecord,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO attempt_records (
            id,
            request_id,
            attempt_index,
            upstream_id,
            upstream_name,
            status,
            started_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, 'started', ?6)
        "#,
    )
    .bind(&record.id)
    .bind(&record.request_id)
    .bind(record.attempt_index)
    .bind(record.upstream_id)
    .bind(&record.upstream_name)
    .bind(now())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn finish_attempt(
    pool: &SqlitePool,
    attempt_id: &str,
    update: &FinishAttempt,
) -> Result<(), sqlx::Error> {
    let completed_at = now();
    sqlx::query(
        r#"
        UPDATE attempt_records
        SET status = ?2,
            http_status = ?3,
            response_header_ms = ?4,
            first_token_ms = ?5,
            timeout_reason = ?6,
            error_message = ?7,
            emitted_to_client = ?8,
            completed_at = ?9,
            duration_ms = CAST((julianday(?9) - julianday(started_at)) * 86400000 AS INTEGER)
        WHERE id = ?1
        "#,
    )
    .bind(attempt_id)
    .bind(&update.status)
    .bind(update.http_status)
    .bind(update.response_header_ms)
    .bind(update.first_token_ms)
    .bind(&update.timeout_reason)
    .bind(&update.error_message)
    .bind(if update.emitted_to_client { 1 } else { 0 })
    .bind(completed_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_recent_requests(
    pool: &SqlitePool,
    limit: i64,
) -> Result<Vec<RequestRecord>, sqlx::Error> {
    sqlx::query_as::<_, RequestRecord>(
        r#"
        SELECT id, method, endpoint, model, status, upstream_name, attempt_count,
               final_http_status, error_message, retry_count, created_at, completed_at, duration_ms
        FROM request_records
        ORDER BY created_at DESC
        LIMIT ?1
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}

pub async fn list_attempts(
    pool: &SqlitePool,
    request_id: &str,
) -> Result<Vec<AttemptRecord>, sqlx::Error> {
    sqlx::query_as::<_, AttemptRecord>(
        r#"
        SELECT id, request_id, attempt_index, upstream_id, upstream_name, status, http_status,
               response_header_ms, first_token_ms, timeout_reason, error_message,
               emitted_to_client, started_at, completed_at, duration_ms
        FROM attempt_records
        WHERE request_id = ?1
        ORDER BY attempt_index ASC
        "#,
    )
    .bind(request_id)
    .fetch_all(pool)
    .await
}

pub async fn count_by_status(pool: &SqlitePool, status: &str) -> Result<i64, sqlx::Error> {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM request_records WHERE status = ?1")
        .bind(status)
        .fetch_one(pool)
        .await?;
    Ok(row.0)
}

pub async fn count_attempt_timeout_reason(
    pool: &SqlitePool,
    reason: &str,
) -> Result<i64, sqlx::Error> {
    let row: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM attempt_records WHERE timeout_reason = ?1")
            .bind(reason)
            .fetch_one(pool)
            .await?;
    Ok(row.0)
}

pub async fn total_requests(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM request_records")
        .fetch_one(pool)
        .await?;
    Ok(row.0)
}
