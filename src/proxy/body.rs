use axum::body::{Body, to_bytes};
use bytes::Bytes;
use serde_json::Value;

use crate::error::AppError;

pub async fn read_limited(body: Body, limit: usize) -> Result<Bytes, AppError> {
    to_bytes(body, limit)
        .await
        .map_err(|error| AppError::PayloadTooLarge(format!("request body exceeds limit: {error}")))
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
