use axum::http::Uri;
use url::Url;

use crate::{error::AppError, storage::upstreams::Upstream};

pub fn build_url(upstream: &Upstream, uri: &Uri) -> Result<Url, AppError> {
    let path_and_query = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    let base = upstream.base_url.trim_end_matches('/');
    Url::parse(&format!("{base}{path_and_query}"))
        .map_err(|error| AppError::BadRequest(format!("invalid upstream url: {error}")))
}

pub fn header_timeout_ms(upstream: &Upstream, global: i64) -> i64 {
    upstream.response_header_timeout_ms.unwrap_or(global).max(1)
}

pub fn first_token_timeout_ms(upstream: &Upstream, global: i64) -> i64 {
    upstream.first_token_timeout_ms.unwrap_or(global).max(1)
}

pub fn max_attempts(upstream: &Upstream, global: usize) -> usize {
    upstream
        .max_attempts
        .map(|value| value.max(1) as usize)
        .unwrap_or(global)
        .max(1)
}
