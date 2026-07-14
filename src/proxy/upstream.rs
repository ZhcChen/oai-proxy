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
