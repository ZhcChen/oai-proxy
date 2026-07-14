use axum::body::{Body, to_bytes};
use bytes::Bytes;
use serde_json::Value;

use crate::error::AppError;

pub async fn read_all(body: Body) -> Result<Bytes, AppError> {
    to_bytes(body, usize::MAX)
        .await
        .map_err(|error| AppError::BadRequest(format!("failed to read request body: {error}")))
}

pub fn extract_model(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    value
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub fn wants_stream(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };

    value
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}
