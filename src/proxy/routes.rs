use axum::{
    Json,
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Response, StatusCode, Uri},
    response::IntoResponse,
};
use serde_json::json;
use uuid::Uuid;

use crate::{
    app::AppState,
    error::AppError,
    storage::{
        proxy_keys,
        records::{self, FinishAttempt, NewAttemptRecord, NewRequestRecord},
        settings,
        upstreams::{self, Upstream},
    },
};

use super::{
    attempt::{self, AttemptOutcome, AttemptRequest, StreamRecordContext},
    body, headers,
    semantic_token::EndpointKind,
    upstream,
};

pub async fn proxy_openai(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response<Body>, AppError> {
    if !proxy_keys::is_authorized(&state.pool, headers::bearer_token(&headers)).await? {
        return Ok(openai_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid proxy authorization",
        ));
    }

    let runtime_settings = match settings::get_runtime_settings(&state.pool, &state.config).await {
        Ok(settings) => settings,
        Err(error) => {
            tracing::error!(error = %error, "failed to load runtime proxy settings");
            return Ok(openai_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "settings_error",
                "failed to load proxy settings",
            ));
        }
    };
    let request_body =
        match body::read_limited(body, runtime_settings.max_body_bytes as usize).await {
            Ok(body) => body,
            Err(error) => {
                return Ok(openai_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "payload_too_large",
                    &error.to_string(),
                ));
            }
        };
    let request_id = Uuid::new_v4().to_string();
    let endpoint = uri
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| uri.path().to_string());
    let model = body::extract_model(&request_body);

    let records_enabled = match records::create_request(
        &state.pool,
        &NewRequestRecord {
            id: request_id.clone(),
            method: method.to_string(),
            endpoint: endpoint.clone(),
            model,
        },
    )
    .await
    {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!(error = %error, request_id = %request_id, "failed to create request record");
            false
        }
    };

    let upstreams = upstreams::list_enabled(&state.pool).await?;
    if upstreams.is_empty() {
        if records_enabled {
            complete_request_best_effort(
                &state,
                &request_id,
                "no_upstream",
                None,
                0,
                Some(StatusCode::SERVICE_UNAVAILABLE.as_u16() as i64),
                Some("no enabled upstream configured"),
            )
            .await;
        }
        return Ok(openai_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_upstream",
            "no enabled upstream configured",
        ));
    }

    let endpoint_kind = EndpointKind::from_path(uri.path());
    let attempt_plan = build_attempt_plan(&runtime_settings, &upstreams);
    let mut last_failure = None;

    for (plan_index, upstream_index) in attempt_plan.iter().enumerate() {
        let attempt_index = plan_index + 1;
        let upstream = &upstreams[*upstream_index];
        let attempt_id = Uuid::new_v4().to_string();
        let attempt_records_enabled = if records_enabled {
            match records::create_attempt(
                &state.pool,
                &NewAttemptRecord {
                    id: attempt_id.clone(),
                    request_id: request_id.clone(),
                    attempt_index: attempt_index as i64,
                    upstream_id: Some(upstream.id),
                    upstream_name: upstream.name.clone(),
                },
            )
            .await
            {
                Ok(_) => true,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        request_id = %request_id,
                        attempt_index,
                        "failed to create attempt record"
                    );
                    false
                }
            }
        } else {
            false
        };
        let request_status_on_success = if attempt_index > 1 {
            "retried_success"
        } else {
            "success"
        };
        let stream_record_context = attempt_records_enabled.then(|| StreamRecordContext {
            pool: state.pool.clone(),
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
                    )
                    .await;
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
                    )
                    .await;
                    complete_request_best_effort(
                        &state,
                        &request_id,
                        request_status_on_success,
                        Some(&upstream.name),
                        attempt_index as i64,
                        Some(http_status),
                        None,
                    )
                    .await;
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
                    )
                    .await;
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
        complete_request_best_effort(
            &state,
            &request_id,
            exhausted_status,
            Some(&upstream_name),
            attempt_plan.len() as i64,
            Some(failure.final_http_status.as_u16() as i64),
            Some(&failure.error_message),
        )
        .await;
    }

    Ok(openai_error(
        failure.final_http_status,
        &failure.status,
        &failure.error_message,
    ))
}

fn build_attempt_plan(
    runtime_settings: &settings::RuntimeSettings,
    upstreams: &[Upstream],
) -> Vec<usize> {
    if !runtime_settings.auto_retry_enabled {
        return vec![0];
    }

    let global = runtime_settings.max_attempts_for_request();
    let per_upstream_limits: Vec<usize> = upstreams
        .iter()
        .map(|item| upstream::max_attempts(item, global))
        .collect();
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

async fn finish_attempt_best_effort(state: &AppState, attempt_id: &str, update: FinishAttempt) {
    if let Err(error) = records::finish_attempt(&state.pool, attempt_id, &update).await {
        tracing::warn!(error = %error, attempt_id, "failed to finish attempt record");
    }
}

async fn complete_request_best_effort(
    state: &AppState,
    request_id: &str,
    status: &str,
    upstream_name: Option<&str>,
    attempt_count: i64,
    final_http_status: Option<i64>,
    error_message: Option<&str>,
) {
    if let Err(error) = records::complete_request(
        &state.pool,
        request_id,
        status,
        upstream_name,
        attempt_count,
        final_http_status,
        error_message,
    )
    .await
    {
        tracing::warn!(error = %error, request_id, "failed to complete request record");
    }
}

fn openai_error(status: StatusCode, code: &str, message: &str) -> Response<Body> {
    (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": "oai_proxy_error",
                "code": code
            }
        })),
    )
        .into_response()
}
