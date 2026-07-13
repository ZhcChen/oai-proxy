use std::{convert::Infallible, path::PathBuf, time::Duration};

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
    storage::{
        self,
        settings::RuntimeSettings,
        upstreams::{self, NewUpstream},
    },
};
use tokio::net::TcpListener;
use tower::ServiceExt;

#[tokio::test]
async fn first_token_timeout_retries_without_leaking_previous_sse_frames() -> anyhow::Result<()> {
    let slow_upstream = spawn_sse_upstream("first", 200).await?;
    let fast_upstream = spawn_sse_upstream("second", 0).await?;

    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    storage::settings::save_runtime_settings(
        &pool,
        &RuntimeSettings {
            max_body_bytes: 1024 * 1024,
            response_header_timeout_ms: 1000,
            first_token_timeout_ms: 50,
            max_attempts: 2,
            auto_retry_enabled: true,
        },
    )
    .await?;
    upstreams::create(
        &pool,
        &NewUpstream {
            name: "slow".to_string(),
            base_url: slow_upstream,
            api_key: "slow-key".to_string(),
            enabled: true,
            response_header_timeout_ms: None,
            first_token_timeout_ms: None,
            max_attempts: None,
        },
    )
    .await?;
    upstreams::create(
        &pool,
        &NewUpstream {
            name: "fast".to_string(),
            base_url: fast_upstream,
            api_key: "fast-key".to_string(),
            enabled: true,
            response_header_timeout_ms: None,
            first_token_timeout_ms: None,
            max_attempts: None,
        },
    )
    .await?;

    let state = app::AppState::new(config, pool.clone())?;
    let router = app::router(state);
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"test-model","stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
                ))?,
        )
        .await?;

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
    let slow_header_upstream = spawn_header_delay_upstream(200).await?;
    let fast_upstream = spawn_sse_upstream("header-recovered", 0).await?;
    let (router, pool) = router_with_upstreams(
        vec![("slow", slow_header_upstream), ("fast", fast_upstream)],
        RuntimeSettings {
            max_body_bytes: 1024 * 1024,
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
async fn exhausted_first_token_timeouts_return_504() -> anyhow::Result<()> {
    let slow_upstream = spawn_sse_upstream("never-visible", 200).await?;
    let (router, pool) = router_with_upstreams(
        vec![("slow", slow_upstream)],
        RuntimeSettings {
            max_body_bytes: 1024 * 1024,
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
    let slow_upstream = spawn_sse_upstream("slow", 200).await?;
    let fast_upstream = spawn_sse_upstream("fast", 0).await?;
    let (router, pool) = router_with_upstreams(
        vec![("slow", slow_upstream), ("fast", fast_upstream)],
        RuntimeSettings {
            max_body_bytes: 1024 * 1024,
            response_header_timeout_ms: 1000,
            first_token_timeout_ms: 50,
            max_attempts: 2,
            auto_retry_enabled: false,
        },
    )
    .await?;

    let response = post_stream_request(router).await?;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    let request = storage::records::list_recent_requests(&pool, 1)
        .await?
        .remove(0);
    assert_eq!(request.status, "exhausted_timeout");
    assert_eq!(request.attempt_count, 1);
    let attempts = storage::records::list_attempts(&pool, &request.id).await?;
    assert_eq!(attempts.len(), 1);
    Ok(())
}

async fn wait_for_request_status(pool: &sqlx::SqlitePool, expected: &str) -> anyhow::Result<()> {
    for _ in 0..20 {
        let requests = storage::records::list_recent_requests(pool, 1).await?;
        if requests
            .first()
            .is_some_and(|request| request.status == expected)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("request status did not become {expected}");
}

async fn router_with_upstreams(
    upstreams_to_create: Vec<(&str, String)>,
    settings: RuntimeSettings,
) -> anyhow::Result<(Router, sqlx::SqlitePool)> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    storage::settings::save_runtime_settings(&pool, &settings).await?;
    for (name, base_url) in upstreams_to_create {
        upstreams::create(
            &pool,
            &NewUpstream {
                name: name.to_string(),
                base_url,
                api_key: format!("{name}-key"),
                enabled: true,
                response_header_timeout_ms: None,
                first_token_timeout_ms: None,
                max_attempts: None,
            },
        )
        .await?;
    }

    let state = app::AppState::new(config, pool.clone())?;
    Ok((app::router(state), pool))
}

async fn post_stream_request(router: Router) -> anyhow::Result<axum::http::Response<Body>> {
    Ok(router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"test-model","stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
                ))?,
        )
        .await?)
}

async fn spawn_sse_upstream(content: &'static str, token_delay_ms: u64) -> anyhow::Result<String> {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || async move {
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
                    2 => Some((Ok::<Event, Infallible>(Event::default().data("[DONE]")), 3)),
                    _ => None,
                }
            });
            Sse::new(events)
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{addr}"))
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
        default_max_body_bytes: 1024 * 1024,
        default_response_header_timeout_ms: 1000,
        default_first_token_timeout_ms: 1000,
        default_max_attempts: 2,
    }
}
