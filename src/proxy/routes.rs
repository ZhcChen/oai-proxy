use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Response, StatusCode, Uri, header},
};
use serde_json::json;
use uuid::Uuid;

use crate::{
    app::AppState,
    error::AppError,
    recording::{CompleteRequest, FinishAttemptRecord, RecordBodyChunk},
    storage::{
        records::{FinishAttempt, NewAttemptRecord, NewRequestRecord},
        settings,
        upstreams::Upstream,
    },
};

use super::{
    attempt::{self, AttemptOutcome, AttemptRequest, StreamRecordContext},
    body, direct,
    semantic_token::EndpointKind,
};

pub async fn proxy_openai(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response<Body>, AppError> {
    if is_control_plane_path(uri.path()) {
        return Ok(not_found());
    }

    let runtime = state.runtime.snapshot();
    let upstreams = runtime.configured_upstreams();
    let runtime_settings = runtime.settings;
    let request_id = Uuid::new_v4().to_string();
    let endpoint = uri
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| uri.path().to_string());

    if !runtime_settings.policy_enabled {
        let records_enabled = if runtime_settings.request_record_enabled {
            state.record_writer.create_request(NewRequestRecord {
                id: request_id.clone(),
                method: method.to_string(),
                endpoint: endpoint.clone(),
                model: None,
            })
        } else {
            false
        };

        if upstreams.is_empty() {
            let response_body = openai_error_payload("no_upstream", "no upstream configured");
            if records_enabled {
                complete_request_best_effort(
                    &state,
                    &request_id,
                    "no_upstream",
                    None,
                    0,
                    Some(StatusCode::SERVICE_UNAVAILABLE.as_u16() as i64),
                    Some("no upstream configured"),
                );
                save_response_body_best_effort(&state, &request_id, response_body.clone());
            }
            return Ok(openai_error(StatusCode::SERVICE_UNAVAILABLE, response_body));
        }

        let upstream = &upstreams[0];
        let attempt_id = Uuid::new_v4().to_string();
        let attempt_records_enabled = if records_enabled {
            state.record_writer.create_attempt(NewAttemptRecord {
                id: attempt_id.clone(),
                request_id: request_id.clone(),
                attempt_index: 1,
                upstream_id: Some(upstream.id),
                upstream_name: upstream.name.clone(),
            })
        } else {
            false
        };
        let record_context = records_enabled.then(|| direct::RecordContext {
            record_writer: state.record_writer.clone(),
            request_id: request_id.clone(),
            attempt_id: attempt_records_enabled.then_some(attempt_id),
            upstream_name: upstream.name.clone(),
            endpoint: EndpointKind::from_path(uri.path()),
        });

        return direct::pass(&state, method, uri, headers, body, upstream, record_context).await;
    }

    if upstreams.is_empty() {
        let records_enabled = if runtime_settings.request_record_enabled {
            state.record_writer.create_request(NewRequestRecord {
                id: request_id.clone(),
                method: method.to_string(),
                endpoint,
                model: None,
            })
        } else {
            false
        };
        if records_enabled {
            let response_body = openai_error_payload("no_upstream", "no upstream configured");
            complete_request_best_effort(
                &state,
                &request_id,
                "no_upstream",
                None,
                0,
                Some(StatusCode::SERVICE_UNAVAILABLE.as_u16() as i64),
                Some("no upstream configured"),
            );
            save_response_body_best_effort(&state, &request_id, response_body.clone());
            return Ok(openai_error(StatusCode::SERVICE_UNAVAILABLE, response_body));
        }
        return Ok(openai_error(
            StatusCode::SERVICE_UNAVAILABLE,
            openai_error_payload("no_upstream", "no upstream configured"),
        ));
    }

    let request_body = match body::read_all(body).await {
        Ok(body) => body,
        Err(error) => {
            return Ok(openai_error(
                StatusCode::BAD_REQUEST,
                openai_error_payload("invalid_request_body", &error.to_string()),
            ));
        }
    };
    let model = body::extract_model(&request_body);

    let records_enabled = if runtime_settings.request_record_enabled {
        state.record_writer.create_request(NewRequestRecord {
            id: request_id.clone(),
            method: method.to_string(),
            endpoint: endpoint.clone(),
            model,
        })
    } else {
        false
    };
    if records_enabled {
        save_request_body_best_effort(&state, &request_id, request_body.to_vec());
    }

    let endpoint_kind = EndpointKind::from_path(uri.path());
    let attempt_plan = build_attempt_plan(&runtime_settings, &upstreams);
    let mut last_failure = None;

    for (plan_index, upstream_index) in attempt_plan.iter().enumerate() {
        let attempt_index = plan_index + 1;
        let upstream = &upstreams[*upstream_index];
        let attempt_id = Uuid::new_v4().to_string();
        let attempt_records_enabled = if records_enabled {
            state.record_writer.create_attempt(NewAttemptRecord {
                id: attempt_id.clone(),
                request_id: request_id.clone(),
                attempt_index: attempt_index as i64,
                upstream_id: Some(upstream.id),
                upstream_name: upstream.name.clone(),
            })
        } else {
            false
        };
        let request_status_on_success = if attempt_index > 1 {
            "retried_success"
        } else {
            "success"
        };
        let stream_record_context = attempt_records_enabled.then(|| StreamRecordContext {
            record_writer: state.record_writer.clone(),
            request_id: request_id.clone(),
            attempt_id: attempt_id.clone(),
            request_status_on_success: request_status_on_success.to_string(),
            upstream_name: upstream.name.clone(),
            attempt_count: attempt_index as i64,
        });

        let outcome = match attempt::run(AttemptRequest {
            state: &state,
            method: &method,
            uri: &uri,
            inbound_headers: &headers,
            request_body: request_body.clone(),
            request_id: &request_id,
            endpoint: endpoint_kind,
            settings: &runtime_settings,
            upstream,
            stream_record_context,
        })
        .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                let message = "proxy attempt failed before contacting upstream";
                tracing::warn!(
                    error = %error,
                    request_id = %request_id,
                    attempt_index,
                    upstream = %upstream.name,
                    "attempt failed before downstream commit"
                );
                if attempt_records_enabled {
                    finish_attempt_best_effort(
                        &state,
                        &attempt_id,
                        FinishAttempt {
                            status: "proxy_attempt_error".to_string(),
                            http_status: None,
                            response_header_ms: None,
                            first_token_ms: None,
                            timeout_reason: None,
                            error_message: Some(message.to_string()),
                            emitted_to_client: false,
                        },
                    );
                }
                last_failure = Some((
                    attempt::AttemptFailure {
                        status: "proxy_attempt_error".to_string(),
                        final_http_status: StatusCode::BAD_GATEWAY,
                        upstream_http_status: None,
                        response_header_ms: None,
                        first_token_ms: None,
                        timeout_reason: None,
                        error_message: message.to_string(),
                    },
                    upstream.name.clone(),
                ));
                continue;
            }
        };

        match outcome {
            AttemptOutcome::Committed {
                response,
                http_status,
                response_header_ms,
                first_token_ms,
                emitted_to_client,
                records_deferred,
            } => {
                if attempt_records_enabled && !records_deferred {
                    finish_attempt_best_effort(
                        &state,
                        &attempt_id,
                        FinishAttempt {
                            status: "success".to_string(),
                            http_status: Some(http_status),
                            response_header_ms: Some(response_header_ms),
                            first_token_ms,
                            timeout_reason: None,
                            error_message: None,
                            emitted_to_client,
                        },
                    );
                }
                if records_enabled && !records_deferred {
                    complete_request_best_effort(
                        &state,
                        &request_id,
                        request_status_on_success,
                        Some(&upstream.name),
                        attempt_index as i64,
                        Some(http_status),
                        None,
                    );
                }
                return Ok(response);
            }
            AttemptOutcome::RetryableFailure(failure) => {
                tracing::warn!(
                    request_id = %request_id,
                    attempt_index,
                    upstream = %upstream.name,
                    status = %failure.status,
                    timeout_reason = ?failure.timeout_reason,
                    error = %failure.error_message,
                    "upstream attempt failed before downstream commit"
                );
                if attempt_records_enabled {
                    finish_attempt_best_effort(
                        &state,
                        &attempt_id,
                        FinishAttempt {
                            status: failure.status.clone(),
                            http_status: failure.upstream_http_status,
                            response_header_ms: failure.response_header_ms,
                            first_token_ms: failure.first_token_ms,
                            timeout_reason: failure.timeout_reason.clone(),
                            error_message: Some(failure.error_message.clone()),
                            emitted_to_client: false,
                        },
                    );
                }
                last_failure = Some((failure, upstream.name.clone()));
            }
        }
    }

    let (failure, upstream_name) = last_failure.expect("at least one attempt should have run");
    let exhausted_status = if failure.timeout_reason.is_some() {
        "exhausted_timeout"
    } else {
        "exhausted_failure"
    };
    if records_enabled {
        let response_body = openai_error_payload(&failure.status, &failure.error_message);
        complete_request_best_effort(
            &state,
            &request_id,
            exhausted_status,
            Some(&upstream_name),
            attempt_plan.len() as i64,
            Some(failure.final_http_status.as_u16() as i64),
            Some(&failure.error_message),
        );
        save_response_body_best_effort(&state, &request_id, response_body.clone());
        return Ok(openai_error(failure.final_http_status, response_body));
    }

    Ok(openai_error(
        failure.final_http_status,
        openai_error_payload(&failure.status, &failure.error_message),
    ))
}

fn is_control_plane_path(path: &str) -> bool {
    matches!(path, "/healthz" | "/metrics" | "/favicon.ico")
        || path == "/admin"
        || path.starts_with("/admin/")
        || path == "/static"
        || path.starts_with("/static/")
        || path.starts_with("/.well-known/appspecific/")
}

fn build_attempt_plan(
    runtime_settings: &settings::RuntimeSettings,
    upstreams: &[Upstream],
) -> Vec<usize> {
    if !runtime_settings.auto_retry_enabled {
        return vec![0];
    }

    let global = runtime_settings.max_attempts_for_request();
    let per_upstream_limits: Vec<usize> = upstreams.iter().map(|_| global).collect();
    let mut counts = vec![0_usize; upstreams.len()];
    let mut plan = Vec::new();

    while plan.len() < global {
        let mut added = false;
        for (index, limit) in per_upstream_limits.iter().enumerate() {
            if counts[index] < *limit && plan.len() < global {
                plan.push(index);
                counts[index] += 1;
                added = true;
            }
        }
        if !added {
            break;
        }
    }

    if plan.is_empty() {
        plan.push(0);
    }
    plan
}

fn finish_attempt_best_effort(state: &AppState, attempt_id: &str, update: FinishAttempt) {
    state.record_writer.finish_attempt(FinishAttemptRecord {
        attempt_id: attempt_id.to_string(),
        update,
    });
}

fn complete_request_best_effort(
    state: &AppState,
    request_id: &str,
    status: &str,
    upstream_name: Option<&str>,
    attempt_count: i64,
    final_http_status: Option<i64>,
    error_message: Option<&str>,
) {
    state.record_writer.complete_request(CompleteRequest {
        request_id: request_id.to_string(),
        status: status.to_string(),
        upstream_name: upstream_name.map(ToOwned::to_owned),
        attempt_count,
        final_http_status,
        error_message: error_message.map(ToOwned::to_owned),
    });
}

fn save_request_body_best_effort(state: &AppState, request_id: &str, body: Vec<u8>) {
    state.record_writer.save_request_body(RecordBodyChunk {
        request_id: request_id.to_string(),
        body,
    });
}

fn save_response_body_best_effort(state: &AppState, request_id: &str, body: Vec<u8>) {
    state.record_writer.save_response_body(RecordBodyChunk {
        request_id: request_id.to_string(),
        body,
    });
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

fn not_found() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::empty())
        .expect("response builder should accept empty body")
}
