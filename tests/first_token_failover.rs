use std::{
    convert::Infallible,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::sse::{Event, Sse},
    routing::post,
};
use futures_util::stream;
use oai_proxy::{
    app,
    config::AppConfig,
    storage::{self, settings::RuntimeSettings, upstreams},
};
use tokio::net::TcpListener;
use tower::ServiceExt;

#[tokio::test]
async fn first_token_timeout_retries_without_leaking_previous_sse_frames() -> anyhow::Result<()> {
    let upstream = spawn_sse_sequence_upstream(vec![("first", 200), ("second", 0)]).await?;
    let (router, pool) = router_with_upstream(
        upstream,
        RuntimeSettings {
            policy_enabled: true,
            request_record_enabled: true,
            response_header_timeout_ms: 1000,
            first_token_timeout_ms: 50,
            max_attempts: 2,
            auto_retry_enabled: true,
        },
    )
    .await?;

    let response = post_stream_request(router).await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("second"));
    assert!(!text.contains("first"));
    let created_index = text
        .find("response.created")
        .expect("created prefix is flushed");
    let token_index = text.find("second").expect("semantic token is flushed");
    assert!(created_index < token_index);

    wait_for_request_status(&pool, "retried_success").await?;
    let requests = storage::records::list_recent_requests(&pool, 1).await?;
    assert_eq!(requests[0].status, "retried_success");
    assert_eq!(requests[0].attempt_count, 2);
    let attempts = storage::records::list_attempts(&pool, &requests[0].id).await?;
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        attempts[0].timeout_reason.as_deref(),
        Some("first_token_timeout")
    );
    assert_eq!(attempts[1].status, "success");
    Ok(())
}

#[tokio::test]
async fn response_header_timeout_retries_next_attempt() -> anyhow::Result<()> {
    let upstream = spawn_header_delay_sequence_upstream(200, "header-recovered").await?;
    let (router, pool) = router_with_upstream(
        upstream,
        RuntimeSettings {
            policy_enabled: true,
            request_record_enabled: true,
            response_header_timeout_ms: 50,
            first_token_timeout_ms: 500,
            max_attempts: 2,
            auto_retry_enabled: true,
        },
    )
    .await?;

    let response = post_stream_request(router).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert!(String::from_utf8_lossy(&body).contains("header-recovered"));

    wait_for_request_status(&pool, "retried_success").await?;
    let request = storage::records::list_recent_requests(&pool, 1)
        .await?
        .remove(0);
    let attempts = storage::records::list_attempts(&pool, &request.id).await?;
    assert_eq!(
        attempts[0].timeout_reason.as_deref(),
        Some("response_header_timeout")
    );
    assert_eq!(attempts[1].status, "success");
    Ok(())
}

#[tokio::test]
async fn policy_stream_records_full_response_duration_after_tail_finishes() -> anyhow::Result<()> {
    let upstream = spawn_sse_tail_delay_upstream("tail-delay", 0, 120).await?;
    let (router, pool) = router_with_upstream(
        upstream,
        RuntimeSettings {
            policy_enabled: true,
            request_record_enabled: true,
            response_header_timeout_ms: 1000,
            first_token_timeout_ms: 500,
            max_attempts: 1,
            auto_retry_enabled: true,
        },
    )
    .await?;

    let response = post_stream_request(router).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert!(String::from_utf8_lossy(&body).contains("tail-delay"));

    let request = wait_for_request_status(&pool, "success").await?;
    assert_eq!(request.attempt_count, 1);
    assert!(request.duration_ms.unwrap_or_default() >= 80);
    let attempts = storage::records::list_attempts(&pool, &request.id).await?;
    assert_eq!(attempts.len(), 1);
    assert!(attempts[0].first_token_ms.is_some());
    assert!(attempts[0].duration_ms.unwrap_or_default() >= 80);
    let payload = wait_for_payload(&pool, &request.id).await?;
    assert!(String::from_utf8_lossy(&payload.request_body).contains("\"stream\":true"));
    assert_eq!(payload.request_body_complete, 1);
    assert!(String::from_utf8_lossy(&payload.response_body).contains("tail-delay"));
    assert_eq!(payload.response_body_complete, 1);
    Ok(())
}

#[tokio::test]
async fn exhausted_first_token_timeouts_return_504() -> anyhow::Result<()> {
    let slow_upstream = spawn_sse_upstream("never-visible", 200).await?;
    let (router, pool) = router_with_upstream(
        slow_upstream,
        RuntimeSettings {
            policy_enabled: true,
            request_record_enabled: true,
            response_header_timeout_ms: 1000,
            first_token_timeout_ms: 50,
            max_attempts: 2,
            auto_retry_enabled: true,
        },
    )
    .await?;

    let response = post_stream_request(router).await?;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("\"type\":\"oai_proxy_error\""));
    assert!(!text.contains("never-visible"));

    wait_for_request_status(&pool, "exhausted_timeout").await?;
    let request = storage::records::list_recent_requests(&pool, 1)
        .await?
        .remove(0);
    assert_eq!(request.status, "exhausted_timeout");
    assert_eq!(request.attempt_count, 2);
    let attempts = storage::records::list_attempts(&pool, &request.id).await?;
    assert_eq!(attempts.len(), 2);
    assert!(
        attempts
            .iter()
            .all(|attempt| attempt.timeout_reason.as_deref() == Some("first_token_timeout"))
    );
    Ok(())
}

#[tokio::test]
async fn auto_retry_disabled_uses_single_attempt() -> anyhow::Result<()> {
    let upstream = spawn_sse_sequence_upstream(vec![("slow", 200), ("fast", 0)]).await?;
    let (router, pool) = router_with_upstream(
        upstream,
        RuntimeSettings {
            policy_enabled: true,
            request_record_enabled: true,
            response_header_timeout_ms: 1000,
            first_token_timeout_ms: 50,
            max_attempts: 2,
            auto_retry_enabled: false,
        },
    )
    .await?;

    let response = post_stream_request(router).await?;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    wait_for_request_status(&pool, "exhausted_timeout").await?;
    let request = storage::records::list_recent_requests(&pool, 1)
        .await?
        .remove(0);
    assert_eq!(request.status, "exhausted_timeout");
    assert_eq!(request.attempt_count, 1);
    let attempts = storage::records::list_attempts(&pool, &request.id).await?;
    assert_eq!(attempts.len(), 1);
    Ok(())
}

#[tokio::test]
async fn policy_disabled_uses_direct_proxy_with_records_but_without_retry() -> anyhow::Result<()> {
    let slow_upstream = spawn_sse_upstream("slow-direct", 120).await?;
    let (router, pool) = router_with_upstream(
        slow_upstream,
        RuntimeSettings {
            policy_enabled: false,
            request_record_enabled: true,
            response_header_timeout_ms: 1000,
            first_token_timeout_ms: 10,
            max_attempts: 2,
            auto_retry_enabled: true,
        },
    )
    .await?;

    let response = post_stream_request(router).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("slow-direct"));

    let request = wait_for_request_status(&pool, "success").await?;
    assert_eq!(request.attempt_count, 1);
    assert_eq!(request.final_http_status, Some(200));
    assert!(request.response_header_ms.is_some());
    assert!(request.first_token_ms.is_some());
    assert!(request.duration_ms.is_some());
    let attempts = storage::records::list_attempts(&pool, &request.id).await?;
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, "success");
    assert_eq!(attempts[0].http_status, Some(200));
    assert!(attempts[0].response_header_ms.is_some());
    let first_token_ms = attempts[0]
        .first_token_ms
        .expect("direct SSE request records first semantic token latency");
    assert!(first_token_ms >= 80);
    assert!(attempts[0].response_header_ms.unwrap_or_default() < first_token_ms);
    assert!(attempts[0].duration_ms.is_some());
    let payload = wait_for_payload(&pool, &request.id).await?;
    assert!(String::from_utf8_lossy(&payload.request_body).contains("\"stream\":true"));
    assert_eq!(payload.request_body_complete, 1);
    assert!(String::from_utf8_lossy(&payload.response_body).contains("slow-direct"));
    assert_eq!(payload.response_body_complete, 1);
    Ok(())
}

#[tokio::test]
async fn request_record_disabled_keeps_policy_but_skips_sqlite_records() -> anyhow::Result<()> {
    let upstream =
        spawn_sse_sequence_upstream(vec![("no-record-slow", 120), ("no-record-fast", 0)]).await?;
    let (router, pool) = router_with_upstream(
        upstream,
        RuntimeSettings {
            policy_enabled: true,
            request_record_enabled: false,
            response_header_timeout_ms: 1000,
            first_token_timeout_ms: 10,
            max_attempts: 2,
            auto_retry_enabled: true,
        },
    )
    .await?;

    let response = post_stream_request(router).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("no-record-fast"));
    assert!(!text.contains("no-record-slow"));
    assert_eq!(storage::records::total_requests(&pool).await?, 0);
    assert_eq!(total_payloads(&pool).await?, 0);
    Ok(())
}

#[tokio::test]
async fn policy_disabled_forwards_with_or_without_authorization() -> anyhow::Result<()> {
    let upstream = spawn_sse_upstream("authorized-direct", 0).await?;
    let (router, pool) = router_with_upstream(
        upstream,
        RuntimeSettings {
            policy_enabled: false,
            request_record_enabled: false,
            response_header_timeout_ms: 1,
            first_token_timeout_ms: 1,
            max_attempts: 2,
            auto_retry_enabled: true,
        },
    )
    .await?;

    let no_auth = post_stream_request(router.clone()).await?;
    assert_eq!(no_auth.status(), StatusCode::OK);

    let ok = post_stream_request_with_auth(router, Some("Bearer client-key")).await?;
    assert_eq!(ok.status(), StatusCode::OK);
    let body = to_bytes(ok.into_body(), usize::MAX).await?;
    assert!(String::from_utf8_lossy(&body).contains("authorized-direct"));
    assert_eq!(storage::records::total_requests(&pool).await?, 0);
    assert_eq!(total_payloads(&pool).await?, 0);
    Ok(())
}

#[tokio::test]
async fn policy_disabled_ignores_response_header_timeout() -> anyhow::Result<()> {
    let slow_header_upstream = spawn_header_delay_upstream(80).await?;
    let (router, _pool) = router_with_upstream(
        slow_header_upstream,
        RuntimeSettings {
            policy_enabled: false,
            request_record_enabled: true,
            response_header_timeout_ms: 1,
            first_token_timeout_ms: 1,
            max_attempts: 1,
            auto_retry_enabled: true,
        },
    )
    .await?;

    let response = post_stream_request(router).await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

async fn wait_for_request_status(
    pool: &sqlx::SqlitePool,
    expected: &str,
) -> anyhow::Result<storage::records::RequestRecord> {
    for _ in 0..100 {
        let requests = storage::records::list_recent_requests(pool, 1).await?;
        if let Some(request) = requests
            .into_iter()
            .find(|request| request.status == expected)
        {
            return Ok(request);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("request status did not become {expected}");
}

async fn wait_for_payload(
    pool: &sqlx::SqlitePool,
    request_id: &str,
) -> anyhow::Result<storage::records::RequestPayload> {
    for _ in 0..100 {
        if let Some(payload) = storage::records::get_payload(pool, request_id).await?
            && (!payload.request_body.is_empty() || !payload.response_body.is_empty())
        {
            return Ok(payload);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("payload was not recorded for request {request_id}");
}

async fn total_payloads(pool: &sqlx::SqlitePool) -> anyhow::Result<i64> {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM request_payloads")
        .fetch_one(pool)
        .await?;
    Ok(row.0)
}

async fn router_with_upstream(
    upstream_url: String,
    settings: RuntimeSettings,
) -> anyhow::Result<(Router, sqlx::SqlitePool)> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    storage::settings::save_runtime_settings(&pool, &settings).await?;
    upstreams::save_single_base_url(&pool, &upstream_url).await?;

    let state = app::AppState::new(config, pool.clone()).await?;
    Ok((app::router(state), pool))
}

async fn post_stream_request(router: Router) -> anyhow::Result<axum::http::Response<Body>> {
    post_stream_request_with_auth(router, None).await
}

async fn post_stream_request_with_auth(
    router: Router,
    authorization: Option<&str>,
) -> anyhow::Result<axum::http::Response<Body>> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json");
    if let Some(authorization) = authorization {
        builder = builder.header("authorization", authorization);
    }

    Ok(router
        .oneshot(builder.body(Body::from(
            r#"{"model":"test-model","stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
        ))?)
        .await?)
}

async fn spawn_sse_upstream(content: &'static str, token_delay_ms: u64) -> anyhow::Result<String> {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || async move { sse_response(content, token_delay_ms) }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{addr}"))
}

async fn spawn_sse_tail_delay_upstream(
    content: &'static str,
    token_delay_ms: u64,
    done_delay_ms: u64,
) -> anyhow::Result<String> {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(
            move || async move { sse_response_with_delays(content, token_delay_ms, done_delay_ms) },
        ),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{addr}"))
}

async fn spawn_sse_sequence_upstream(sequence: Vec<(&'static str, u64)>) -> anyhow::Result<String> {
    let sequence = Arc::new(sequence);
    let counter = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post({
            let sequence = Arc::clone(&sequence);
            let counter = Arc::clone(&counter);
            move || {
                let sequence = Arc::clone(&sequence);
                let counter = Arc::clone(&counter);
                async move {
                    let index = counter.fetch_add(1, Ordering::SeqCst);
                    let selected = sequence
                        .get(index)
                        .or_else(|| sequence.last())
                        .expect("sequence upstream needs at least one response");
                    sse_response(selected.0, selected.1)
                }
            }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{addr}"))
}

async fn spawn_header_delay_sequence_upstream(
    first_header_delay_ms: u64,
    recovered_content: &'static str,
) -> anyhow::Result<String> {
    let counter = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post({
            let counter = Arc::clone(&counter);
            move || {
                let counter = Arc::clone(&counter);
                async move {
                    if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                        tokio::time::sleep(Duration::from_millis(first_header_delay_ms)).await;
                    }
                    sse_response(recovered_content, 0)
                }
            }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{addr}"))
}

fn sse_response(
    content: &'static str,
    token_delay_ms: u64,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    sse_response_with_delays(content, token_delay_ms, 0)
}

fn sse_response_with_delays(
    content: &'static str,
    token_delay_ms: u64,
    done_delay_ms: u64,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let events = stream::unfold(0_u8, move |step| async move {
        match step {
            0 => Some((
                Ok::<Event, Infallible>(
                    Event::default()
                        .event("response.created")
                        .data("{\"type\":\"response.created\"}"),
                ),
                1,
            )),
            1 => {
                tokio::time::sleep(Duration::from_millis(token_delay_ms)).await;
                Some((
                    Ok::<Event, Infallible>(Event::default().data(format!(
                        "{{\"choices\":[{{\"delta\":{{\"content\":\"{content}\"}}}}]}}"
                    ))),
                    2,
                ))
            }
            2 => {
                tokio::time::sleep(Duration::from_millis(done_delay_ms)).await;
                Some((Ok::<Event, Infallible>(Event::default().data("[DONE]")), 3))
            }
            _ => None,
        }
    });
    Sse::new(events)
}

async fn spawn_header_delay_upstream(header_delay_ms: u64) -> anyhow::Result<String> {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || async move {
            tokio::time::sleep(Duration::from_millis(header_delay_ms)).await;
            Json(serde_json::json!({"ok": true}))
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{addr}"))
}

fn test_config() -> AppConfig {
    AppConfig {
        bind_host: "127.0.0.1".to_string(),
        database_url: "sqlite::memory:".to_string(),
        data_dir: PathBuf::from("data"),
        default_response_header_timeout_ms: 1000,
        default_first_token_timeout_ms: 1000,
        default_max_attempts: 2,
    }
}
