use axum::{Json, extract::State, response::IntoResponse};
use serde::Serialize;

use crate::{app::AppState, error::AppError, storage::records};

#[derive(Serialize)]
struct MetricsSnapshot {
    requests_total: i64,
    requests_success: i64,
    requests_retried_success: i64,
    requests_exhausted_timeout: i64,
    attempts_response_header_timeout: i64,
    attempts_first_token_timeout: i64,
}

pub async fn metrics(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let snapshot = MetricsSnapshot {
        requests_total: records::total_requests(&state.pool).await?,
        requests_success: records::count_by_status(&state.pool, "success").await?,
        requests_retried_success: records::count_by_status(&state.pool, "retried_success").await?,
        requests_exhausted_timeout: records::count_by_status(&state.pool, "exhausted_timeout")
            .await?,
        attempts_response_header_timeout: records::count_attempt_timeout_reason(
            &state.pool,
            "response_header_timeout",
        )
        .await?,
        attempts_first_token_timeout: records::count_attempt_timeout_reason(
            &state.pool,
            "first_token_timeout",
        )
        .await?,
    };

    Ok(Json(snapshot))
}
