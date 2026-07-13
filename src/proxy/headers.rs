use axum::http::{HeaderMap, HeaderName, HeaderValue, header};

const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
    "cookie",
    "set-cookie",
];

pub fn copy_request_headers(
    inbound: &HeaderMap,
    builder: reqwest::RequestBuilder,
    upstream_api_key: &str,
) -> reqwest::RequestBuilder {
    let mut builder = builder;
    for (name, value) in inbound {
        if should_skip_request_header(name) {
            continue;
        }
        builder = builder.header(name, value);
    }

    if !upstream_api_key.is_empty() {
        builder = builder.bearer_auth(upstream_api_key);
    }

    builder
}

pub fn response_headers(upstream: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in upstream {
        if should_skip_response_header(name) {
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

fn should_skip_request_header(name: &HeaderName) -> bool {
    let lower = name.as_str().to_ascii_lowercase();
    lower == header::AUTHORIZATION.as_str() || HOP_BY_HOP_HEADERS.contains(&lower.as_str())
}

fn should_skip_response_header(name: &HeaderName) -> bool {
    let lower = name.as_str().to_ascii_lowercase();
    HOP_BY_HOP_HEADERS.contains(&lower.as_str())
}

pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let header = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))
}

pub fn openai_request_id_header(request_id: &str) -> HeaderValue {
    HeaderValue::from_str(request_id)
        .unwrap_or_else(|_| HeaderValue::from_static("invalid-request-id"))
}
