use axum::http::{HeaderMap, HeaderName, header};
use std::collections::HashSet;

const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
];

pub fn copy_request_headers(
    inbound: &HeaderMap,
    builder: reqwest::RequestBuilder,
) -> reqwest::RequestBuilder {
    let mut builder = builder;
    let connection_headers = connection_header_tokens(inbound);
    for (name, value) in inbound {
        if should_skip_header(name, &connection_headers) {
            continue;
        }
        builder = builder.header(name, value);
    }

    builder
}

pub fn response_headers(upstream: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let connection_headers = connection_header_tokens(upstream);
    for (name, value) in upstream {
        if should_skip_header(name, &connection_headers) {
            continue;
        }
        headers.append(name, value.clone());
    }
    headers
}

pub fn is_sse_response(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false)
}

fn should_skip_header(name: &HeaderName, connection_headers: &HashSet<String>) -> bool {
    let lower = name.as_str().to_ascii_lowercase();
    HOP_BY_HOP_HEADERS.contains(&lower.as_str()) || connection_headers.contains(&lower)
}

fn connection_header_tokens(headers: &HeaderMap) -> HashSet<String> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}
