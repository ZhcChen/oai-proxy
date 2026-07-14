use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use axum::{
    Error,
    body::Body,
    http::{HeaderMap, Method, Response, StatusCode, Uri, header},
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::json;
use tokio::time::Instant;

use crate::{
    app::AppState,
    error::AppError,
    recording::{CompleteRequest, FinishAttemptRecord, FinishBody, RecordBodyChunk, RecordWriter},
    storage::{records::FinishAttempt, upstreams::Upstream},
};

use super::{headers, semantic_token::EndpointKind, sse::SseParser, upstream};

const OBSERVED_SSE_BUFFER_LIMIT: usize = 256 * 1024;

#[derive(Clone)]
pub struct RecordContext {
    pub record_writer: RecordWriter,
    pub request_id: String,
    pub attempt_id: Option<String>,
    pub upstream_name: String,
    pub endpoint: EndpointKind,
}

pub async fn pass(
    state: &AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
    upstream: &Upstream,
    record_context: Option<RecordContext>,
) -> Result<Response<Body>, AppError> {
    let started = Instant::now();
    let target_url = match upstream::build_url(upstream, &uri) {
        Ok(url) => url,
        Err(error) => {
            let response_body =
                openai_error_payload("invalid_upstream_url", "invalid upstream url");
            if let Some(context) = record_context {
                context.finish_before_response(
                    "invalid_upstream_url",
                    StatusCode::BAD_GATEWAY,
                    error.to_string(),
                    response_body.clone(),
                );
            }
            return Ok(openai_error(StatusCode::BAD_GATEWAY, response_body));
        }
    };
    let outbound_body = reqwest::Body::wrap_stream(RecordedRequestStream {
        upstream: body.into_data_stream(),
        recorder: record_context.as_ref().map(|context| RequestBodyRecorder {
            record_writer: context.record_writer.clone(),
            request_id: context.request_id.clone(),
        }),
        finished: false,
    });
    let request = state
        .http_client
        .request(method, target_url)
        .body(outbound_body);
    let request = headers::copy_request_headers(&headers, request);

    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(error = %error, upstream = %upstream.name, "direct proxy upstream request failed");
            let response_body = openai_error_payload("upstream_error", "upstream request failed");
            if let Some(context) = record_context {
                context.finish_before_response(
                    "upstream_error",
                    StatusCode::BAD_GATEWAY,
                    "upstream request failed".to_string(),
                    response_body.clone(),
                );
            }
            return Ok(openai_error(StatusCode::BAD_GATEWAY, response_body));
        }
    };

    let response_header_ms = elapsed_ms(started);
    Ok(response_from_upstream(
        response,
        record_context,
        started,
        response_header_ms,
    ))
}

fn response_from_upstream(
    response: reqwest::Response,
    record_context: Option<RecordContext>,
    started: Instant,
    response_header_ms: i64,
) -> Response<Body> {
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let is_sse = headers::is_sse_response(response.headers());
    let headers = headers::response_headers(response.headers());
    let body = if let Some(context) = record_context {
        Body::from_stream(RecordedTransparentStream {
            upstream: response.bytes_stream(),
            finalizer: Some(DirectFinalizer {
                context,
                http_status: status.as_u16() as i64,
                response_header_ms,
                first_token_ms: None,
                started,
                observe_sse: is_sse && status.is_success(),
                parser: SseParser::new(),
                emitted_to_client: false,
            }),
            finished: false,
        })
    } else {
        Body::from_stream(
            response
                .bytes_stream()
                .map(|item| item.map_err(|error| io::Error::other(error.to_string()))),
        )
    };
    let mut response = Response::builder()
        .status(status)
        .body(body)
        .expect("response builder should accept upstream status");
    response.headers_mut().extend(headers);
    response
}

fn openai_error_payload(code: &str, message: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "error": {
            "message": message,
            "type": "oai_proxy_error",
            "code": code
        }
    }))
    .expect("OpenAI-compatible error payload should serialize")
}

fn openai_error(status: StatusCode, body: Vec<u8>) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("response builder should accept JSON error body")
}

struct RequestBodyRecorder {
    record_writer: RecordWriter,
    request_id: String,
}

impl RequestBodyRecorder {
    fn append(&self, bytes: &[u8]) {
        self.record_writer.append_request_body(RecordBodyChunk {
            request_id: self.request_id.clone(),
            body: bytes.to_vec(),
        });
    }

    fn finish(self, complete: bool, error_message: Option<String>) {
        self.record_writer.finish_request_body(FinishBody {
            request_id: self.request_id,
            complete,
            error_message,
        });
    }
}

struct RecordedRequestStream<S> {
    upstream: S,
    recorder: Option<RequestBodyRecorder>,
    finished: bool,
}

impl<S> futures_util::Stream for RecordedRequestStream<S>
where
    S: futures_util::Stream<Item = Result<Bytes, Error>> + Unpin,
{
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.upstream).poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                if let Some(recorder) = self.recorder.as_mut() {
                    recorder.append(&bytes);
                }
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(error))) => {
                self.finished = true;
                let message = error.to_string();
                if let Some(recorder) = self.recorder.take() {
                    recorder.finish(false, Some(message.clone()));
                }
                Poll::Ready(Some(Err(io::Error::other(message))))
            }
            Poll::Ready(None) => {
                self.finished = true;
                if let Some(recorder) = self.recorder.take() {
                    recorder.finish(true, None);
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> Drop for RecordedRequestStream<S> {
    fn drop(&mut self) {
        if !self.finished
            && let Some(recorder) = self.recorder.take()
        {
            recorder.finish(
                false,
                Some("request body stream dropped before completion".to_string()),
            );
        }
    }
}

struct RecordedTransparentStream<S> {
    upstream: S,
    finalizer: Option<DirectFinalizer>,
    finished: bool,
}

impl<S> futures_util::Stream for RecordedTransparentStream<S>
where
    S: futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.upstream).poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                if let Some(finalizer) = self.finalizer.as_mut() {
                    finalizer.emitted_to_client = true;
                    finalizer.observe_chunk(&bytes);
                }
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(error))) => {
                self.finished = true;
                let message = error.to_string();
                if let Some(finalizer) = self.finalizer.take() {
                    finalizer.finish_stream_error(message.clone());
                }
                Poll::Ready(Some(Err(io::Error::other(message))))
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

impl<S> Drop for RecordedTransparentStream<S> {
    fn drop(&mut self) {
        if !self.finished
            && let Some(finalizer) = self.finalizer.take()
        {
            finalizer.finish_client_disconnected();
        }
    }
}

struct DirectFinalizer {
    context: RecordContext,
    http_status: i64,
    response_header_ms: i64,
    first_token_ms: Option<i64>,
    started: Instant,
    observe_sse: bool,
    parser: SseParser,
    emitted_to_client: bool,
}

impl DirectFinalizer {
    fn observe_chunk(&mut self, bytes: &[u8]) {
        self.context
            .record_writer
            .append_response_body(RecordBodyChunk {
                request_id: self.context.request_id.clone(),
                body: bytes.to_vec(),
            });

        if !self.observe_sse || self.first_token_ms.is_some() {
            return;
        }

        let events = self.parser.push(bytes);
        if events
            .iter()
            .any(|event| super::semantic_token::is_semantic_event(self.context.endpoint, event))
        {
            self.first_token_ms = Some(elapsed_ms(self.started));
            self.observe_sse = false;
            return;
        }

        if self.parser.buffered_len() > OBSERVED_SSE_BUFFER_LIMIT {
            tracing::warn!(
                request_id = %self.context.request_id,
                "direct SSE first-token observation buffer exceeded limit; stop observing"
            );
            let _ = self.parser.take_buffer();
            self.observe_sse = false;
        }
    }

    fn finish_success(self) {
        let status = if (200..400).contains(&self.http_status) {
            "success".to_string()
        } else {
            format!("upstream_http_{}", self.http_status)
        };
        let http_status = self.http_status;
        let emitted_to_client = self.emitted_to_client;
        self.finish(status, None, Some(http_status), emitted_to_client);
    }

    fn finish_stream_error(self, error_message: String) {
        let http_status = self.http_status;
        let emitted_to_client = self.emitted_to_client;
        self.finish(
            "stream_error".to_string(),
            Some(error_message),
            Some(http_status),
            emitted_to_client,
        );
    }

    fn finish_client_disconnected(self) {
        let http_status = self.http_status;
        let emitted_to_client = self.emitted_to_client;
        self.finish(
            "client_disconnected".to_string(),
            Some("downstream client disconnected before response completed".to_string()),
            Some(http_status),
            emitted_to_client,
        );
    }

    fn finish(
        self,
        status: String,
        error_message: Option<String>,
        final_http_status: Option<i64>,
        emitted_to_client: bool,
    ) {
        self.context.record_writer.finish_response_body(FinishBody {
            request_id: self.context.request_id.clone(),
            complete: error_message.is_none(),
            error_message: error_message.clone(),
        });
        if let Some(attempt_id) = self.context.attempt_id {
            self.context
                .record_writer
                .finish_attempt(FinishAttemptRecord {
                    attempt_id,
                    update: FinishAttempt {
                        status: status.clone(),
                        http_status: Some(self.http_status),
                        response_header_ms: Some(self.response_header_ms),
                        first_token_ms: self.first_token_ms,
                        timeout_reason: None,
                        error_message: error_message.clone(),
                        emitted_to_client,
                    },
                });
        }
        self.context
            .record_writer
            .complete_request(CompleteRequest {
                request_id: self.context.request_id,
                status,
                upstream_name: Some(self.context.upstream_name),
                attempt_count: 1,
                final_http_status,
                error_message,
            });
    }
}

impl RecordContext {
    fn finish_before_response(
        self,
        status: &str,
        final_http_status: StatusCode,
        error_message: String,
        response_body: Vec<u8>,
    ) {
        self.record_writer.save_response_body(RecordBodyChunk {
            request_id: self.request_id.clone(),
            body: response_body,
        });
        if let Some(attempt_id) = self.attempt_id {
            self.record_writer.finish_attempt(FinishAttemptRecord {
                attempt_id,
                update: FinishAttempt {
                    status: status.to_string(),
                    http_status: None,
                    response_header_ms: None,
                    first_token_ms: None,
                    timeout_reason: None,
                    error_message: Some(error_message.clone()),
                    emitted_to_client: false,
                },
            });
        }
        self.record_writer.complete_request(CompleteRequest {
            request_id: self.request_id,
            status: status.to_string(),
            upstream_name: Some(self.upstream_name),
            attempt_count: 1,
            final_http_status: Some(final_http_status.as_u16() as i64),
            error_message: Some(error_message),
        });
    }
}

fn elapsed_ms(start: Instant) -> i64 {
    start.elapsed().as_millis().min(i64::MAX as u128) as i64
}
