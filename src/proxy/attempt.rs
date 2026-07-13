use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use axum::{
    body::Body,
    http::{HeaderMap, Method, Response, StatusCode, Uri},
};
use bytes::{Bytes, BytesMut};
use futures_util::{StreamExt, stream};
use tokio::time::{Instant, timeout};

use crate::{
    app::AppState,
    error::AppError,
    storage::{
        records::{self, FinishAttempt},
        settings::RuntimeSettings,
        upstreams::Upstream,
    },
};

use super::{
    body, headers,
    semantic_token::{self, EndpointKind},
    sse::SseParser,
    upstream,
};

const PREFIX_BUFFER_LIMIT: usize = 256 * 1024;

pub struct AttemptRequest<'a> {
    pub state: &'a AppState,
    pub method: &'a Method,
    pub uri: &'a Uri,
    pub inbound_headers: &'a HeaderMap,
    pub request_body: Bytes,
    pub request_id: &'a str,
    pub endpoint: EndpointKind,
    pub settings: &'a RuntimeSettings,
    pub upstream: &'a Upstream,
    pub stream_record_context: Option<StreamRecordContext>,
}

pub enum AttemptOutcome {
    Committed {
        response: Response<Body>,
        http_status: i64,
        response_header_ms: i64,
        first_token_ms: Option<i64>,
        emitted_to_client: bool,
        records_deferred: bool,
    },
    RetryableFailure(AttemptFailure),
}

#[derive(Clone, Debug)]
pub struct AttemptFailure {
    pub status: String,
    pub final_http_status: StatusCode,
    pub upstream_http_status: Option<i64>,
    pub response_header_ms: Option<i64>,
    pub first_token_ms: Option<i64>,
    pub timeout_reason: Option<String>,
    pub error_message: String,
}

#[derive(Clone)]
pub struct StreamRecordContext {
    pub pool: sqlx::SqlitePool,
    pub request_id: String,
    pub attempt_id: String,
    pub request_status_on_success: String,
    pub upstream_name: String,
    pub attempt_count: i64,
}

pub async fn run(input: AttemptRequest<'_>) -> Result<AttemptOutcome, AppError> {
    let attempt_started = Instant::now();
    let target_url = upstream::build_url(input.upstream, input.uri)?;
    let request = input
        .state
        .http_client
        .request(input.method.clone(), target_url)
        .body(input.request_body.clone());
    let request =
        headers::copy_request_headers(input.inbound_headers, request, &input.upstream.api_key);

    let header_timeout = Duration::from_millis(upstream::header_timeout_ms(
        input.upstream,
        input.settings.response_header_timeout_ms,
    ) as u64);

    let response = match timeout(header_timeout, request.send()).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "upstream request failed before response header");
            return Ok(AttemptOutcome::RetryableFailure(AttemptFailure {
                status: "upstream_error".to_string(),
                final_http_status: StatusCode::BAD_GATEWAY,
                upstream_http_status: None,
                response_header_ms: None,
                first_token_ms: None,
                timeout_reason: None,
                error_message: "upstream request failed".to_string(),
            }));
        }
        Err(_) => {
            return Ok(AttemptOutcome::RetryableFailure(AttemptFailure {
                status: "response_header_timeout".to_string(),
                final_http_status: StatusCode::GATEWAY_TIMEOUT,
                upstream_http_status: None,
                response_header_ms: None,
                first_token_ms: None,
                timeout_reason: Some("response_header_timeout".to_string()),
                error_message: "waiting for upstream response header timed out".to_string(),
            }));
        }
    };

    let response_header_ms = elapsed_ms(attempt_started);
    let status = response.status();
    if is_retryable_status(status) {
        return Ok(AttemptOutcome::RetryableFailure(AttemptFailure {
            status: format!("upstream_http_{}", status.as_u16()),
            final_http_status: StatusCode::from_u16(status.as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY),
            upstream_http_status: Some(status.as_u16() as i64),
            response_header_ms: Some(response_header_ms),
            first_token_ms: None,
            timeout_reason: None,
            error_message: format!("upstream returned HTTP {}", status.as_u16()),
        }));
    }

    let is_sse =
        body::wants_stream(&input.request_body) || headers::is_sse_response(response.headers());
    if is_sse && headers::is_sse_response(response.headers()) && status.is_success() {
        prepare_sse_response(input, response, attempt_started, response_header_ms).await
    } else {
        Ok(AttemptOutcome::Committed {
            http_status: status.as_u16() as i64,
            response: response_from_upstream(response, None, input.request_id),
            response_header_ms,
            first_token_ms: None,
            emitted_to_client: false,
            records_deferred: false,
        })
    }
}

async fn prepare_sse_response(
    input: AttemptRequest<'_>,
    response: reqwest::Response,
    attempt_started: Instant,
    response_header_ms: i64,
) -> Result<AttemptOutcome, AppError> {
    let status = response.status();
    let headers = headers::response_headers(response.headers());
    let first_token_timeout = Duration::from_millis(upstream::first_token_timeout_ms(
        input.upstream,
        input.settings.first_token_timeout_ms,
    ) as u64);
    let first_token_deadline = Instant::now() + first_token_timeout;
    let mut stream = response.bytes_stream();
    let mut parser = SseParser::new();
    let mut prefix = BytesMut::new();

    loop {
        if prefix.len() + parser.buffered_len() > PREFIX_BUFFER_LIMIT {
            return Ok(AttemptOutcome::RetryableFailure(AttemptFailure {
                status: "prefix_buffer_overflow".to_string(),
                final_http_status: StatusCode::GATEWAY_TIMEOUT,
                upstream_http_status: Some(status.as_u16() as i64),
                response_header_ms: Some(response_header_ms),
                first_token_ms: Some(elapsed_ms(attempt_started)),
                timeout_reason: Some("first_token_timeout".to_string()),
                error_message: "SSE prefix exceeded buffer limit before first semantic token"
                    .to_string(),
            }));
        }

        let Some(remaining) = first_token_deadline.checked_duration_since(Instant::now()) else {
            return Ok(first_token_timeout_failure(
                status,
                response_header_ms,
                attempt_started,
            ));
        };

        let next_chunk = match timeout(remaining, stream.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(error))) => {
                tracing::warn!(error = %error, "upstream SSE stream failed before first semantic token");
                return Ok(AttemptOutcome::RetryableFailure(AttemptFailure {
                    status: "upstream_stream_error".to_string(),
                    final_http_status: StatusCode::BAD_GATEWAY,
                    upstream_http_status: Some(status.as_u16() as i64),
                    response_header_ms: Some(response_header_ms),
                    first_token_ms: Some(elapsed_ms(attempt_started)),
                    timeout_reason: None,
                    error_message: "upstream stream failed before first semantic token".to_string(),
                }));
            }
            Ok(None) => {
                return Ok(AttemptOutcome::RetryableFailure(AttemptFailure {
                    status: "first_token_missing".to_string(),
                    final_http_status: StatusCode::GATEWAY_TIMEOUT,
                    upstream_http_status: Some(status.as_u16() as i64),
                    response_header_ms: Some(response_header_ms),
                    first_token_ms: Some(elapsed_ms(attempt_started)),
                    timeout_reason: Some("first_token_timeout".to_string()),
                    error_message: "upstream SSE ended before first semantic token".to_string(),
                }));
            }
            Err(_) => {
                return Ok(first_token_timeout_failure(
                    status,
                    response_header_ms,
                    attempt_started,
                ));
            }
        };

        let events = parser.push(&next_chunk);
        for (index, event) in events.iter().enumerate() {
            if semantic_token::is_semantic_event(input.endpoint, event) {
                let mut initial = BytesMut::new();
                initial.extend_from_slice(&prefix);
                for event in events.iter().skip(index) {
                    initial.extend_from_slice(&event.raw);
                }
                initial.extend_from_slice(&parser.take_buffer());

                let first_token_ms = elapsed_ms(attempt_started);
                let records_deferred = input.stream_record_context.is_some();
                let body = match input.stream_record_context {
                    Some(context) => finalized_sse_body(
                        initial.freeze(),
                        stream,
                        StreamFinalizer {
                            context,
                            http_status: status.as_u16() as i64,
                            response_header_ms,
                            first_token_ms,
                        },
                    ),
                    None => Body::from_stream(
                        stream::once(async move { Ok::<Bytes, io::Error>(initial.freeze()) })
                            .chain(stream.map(|item| item.map_err(io_error))),
                    ),
                };

                let response = response_from_parts(status, headers, body, input.request_id);

                return Ok(AttemptOutcome::Committed {
                    response,
                    http_status: status.as_u16() as i64,
                    response_header_ms,
                    first_token_ms: Some(first_token_ms),
                    emitted_to_client: true,
                    records_deferred,
                });
            }

            prefix.extend_from_slice(&event.raw);
            if prefix.len() + parser.buffered_len() > PREFIX_BUFFER_LIMIT {
                return Ok(AttemptOutcome::RetryableFailure(AttemptFailure {
                    status: "prefix_buffer_overflow".to_string(),
                    final_http_status: StatusCode::GATEWAY_TIMEOUT,
                    upstream_http_status: Some(status.as_u16() as i64),
                    response_header_ms: Some(response_header_ms),
                    first_token_ms: Some(elapsed_ms(attempt_started)),
                    timeout_reason: Some("prefix_buffer_overflow".to_string()),
                    error_message: "SSE prefix exceeded buffer limit before first semantic token"
                        .to_string(),
                }));
            }
        }
    }
}

fn finalized_sse_body<S>(initial: Bytes, upstream: S, finalizer: StreamFinalizer) -> Body
where
    S: futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    Body::from_stream(FinalizedSseStream {
        initial: Some(initial),
        upstream,
        finalizer: Some(finalizer),
        finished: false,
    })
}

struct FinalizedSseStream<S> {
    initial: Option<Bytes>,
    upstream: S,
    finalizer: Option<StreamFinalizer>,
    finished: bool,
}

impl<S> futures_util::Stream for FinalizedSseStream<S>
where
    S: futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(initial) = self.initial.take() {
            return Poll::Ready(Some(Ok(initial)));
        }

        match Pin::new(&mut self.upstream).poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => Poll::Ready(Some(Ok(bytes))),
            Poll::Ready(Some(Err(error))) => {
                self.finished = true;
                if let Some(finalizer) = self.finalizer.take() {
                    finalizer.finish_stream_error(error.to_string());
                }
                Poll::Ready(Some(Err(io_error(error))))
            }
            Poll::Ready(None) => {
                self.finished = true;
                if let Some(finalizer) = self.finalizer.take() {
                    finalizer.finish_success();
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> Drop for FinalizedSseStream<S> {
    fn drop(&mut self) {
        if !self.finished
            && let Some(finalizer) = self.finalizer.take()
        {
            finalizer.finish_client_disconnected();
        }
    }
}

struct StreamFinalizer {
    context: StreamRecordContext,
    http_status: i64,
    response_header_ms: i64,
    first_token_ms: i64,
}

impl StreamFinalizer {
    fn finish_success(self) {
        let http_status = self.http_status;
        let request_status = self.context.request_status_on_success.clone();
        self.spawn_finish(
            "success".to_string(),
            request_status,
            None,
            Some(http_status),
        );
    }

    fn finish_stream_error(self, error_message: String) {
        self.spawn_finish(
            "stream_error".to_string(),
            "stream_error".to_string(),
            Some(error_message),
            Some(StatusCode::BAD_GATEWAY.as_u16() as i64),
        );
    }

    fn finish_client_disconnected(self) {
        self.spawn_finish(
            "client_disconnected".to_string(),
            "client_disconnected".to_string(),
            Some("downstream client disconnected before stream completed".to_string()),
            Some(499),
        );
    }

    fn spawn_finish(
        self,
        attempt_status: String,
        request_status: String,
        error_message: Option<String>,
        final_http_status: Option<i64>,
    ) {
        tokio::spawn(async move {
            if let Err(error) = records::finish_attempt(
                &self.context.pool,
                &self.context.attempt_id,
                &FinishAttempt {
                    status: attempt_status,
                    http_status: Some(self.http_status),
                    response_header_ms: Some(self.response_header_ms),
                    first_token_ms: Some(self.first_token_ms),
                    timeout_reason: None,
                    error_message: error_message.clone(),
                    emitted_to_client: true,
                },
            )
            .await
            {
                tracing::warn!(error = %error, "failed to finish streamed attempt record");
            }

            if let Err(error) = records::complete_request(
                &self.context.pool,
                &self.context.request_id,
                &request_status,
                Some(&self.context.upstream_name),
                self.context.attempt_count,
                final_http_status,
                error_message.as_deref(),
            )
            .await
            {
                tracing::warn!(error = %error, "failed to complete streamed request record");
            }
        });
    }
}

fn first_token_timeout_failure(
    status: reqwest::StatusCode,
    response_header_ms: i64,
    attempt_started: Instant,
) -> AttemptOutcome {
    AttemptOutcome::RetryableFailure(AttemptFailure {
        status: "first_token_timeout".to_string(),
        final_http_status: StatusCode::GATEWAY_TIMEOUT,
        upstream_http_status: Some(status.as_u16() as i64),
        response_header_ms: Some(response_header_ms),
        first_token_ms: Some(elapsed_ms(attempt_started)),
        timeout_reason: Some("first_token_timeout".to_string()),
        error_message: "waiting for first semantic token timed out".to_string(),
    })
}

fn response_from_upstream(
    response: reqwest::Response,
    body: Option<Body>,
    request_id: &str,
) -> Response<Body> {
    let status = response.status();
    let headers = headers::response_headers(response.headers());
    let body = body.unwrap_or_else(|| {
        Body::from_stream(response.bytes_stream().map(|item| item.map_err(io_error)))
    });
    response_from_parts(status, headers, body, request_id)
}

fn response_from_parts(
    status: reqwest::StatusCode,
    headers: HeaderMap,
    body: Body,
    request_id: &str,
) -> Response<Body> {
    let status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response = Response::builder()
        .status(status)
        .body(body)
        .expect("response builder should accept upstream status");
    response.headers_mut().extend(headers);
    response.headers_mut().insert(
        "x-oai-proxy-request-id",
        headers::openai_request_id_header(request_id),
    );
    response
}

fn io_error(error: reqwest::Error) -> io::Error {
    io::Error::other(error.to_string())
}

fn elapsed_ms(start: Instant) -> i64 {
    start.elapsed().as_millis().min(i64::MAX as u128) as i64
}

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
        || status.as_u16() == StatusCode::REQUEST_TIMEOUT.as_u16()
        || status.as_u16() == StatusCode::TOO_MANY_REQUESTS.as_u16()
}
