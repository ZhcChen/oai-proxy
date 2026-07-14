use std::{
    convert::Infallible,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::State,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, Uri, header},
    response::IntoResponse,
    routing::{any, post},
};
use futures_util::stream;
use oai_proxy::{
    app,
    config::AppConfig,
    storage::{self, settings::RuntimeSettings, upstreams},
};
use serde_json::json;
use tokio::net::TcpListener;
use tower::ServiceExt;

#[derive(Clone, Debug)]
struct CapturedRequest {
    method: String,
    path_and_query: String,
    authorization: Option<String>,
    cookie: Option<String>,
    trace: Option<String>,
    hop_header: Option<String>,
    body: String,
}

type Captures = Arc<Mutex<Vec<CapturedRequest>>>;

#[tokio::test]
async fn forwards_request_headers_without_rewriting_authorization() -> anyhow::Result<()> {
    let captures = Captures::default();
    let upstream_url = spawn_capture_upstream(captures.clone()).await?;
    let (router, _pool) = test_router_with_upstream(&upstream_url).await?;

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions?trace=1")
                .header("authorization", "Bearer client-key")
                .header("cookie", "browser_cookie=should-forward")
                .header("connection", "x-test-hop")
                .header("x-trace", "abc")
                .header("x-test-hop", "must-not-forward")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"test-model","stream":false,"messages":[]}"#,
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok()),
        Some("sid=upstream; Path=/")
    );
    assert!(response.headers().get(header::CONNECTION).is_none());
    assert!(response.headers().get("x-upstream-hop").is_none());
    assert!(response.headers().get("x-oai-proxy-request-id").is_none());
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert!(String::from_utf8_lossy(&body).contains("\"ok\":true"));
    let captured = captures
        .lock()
        .unwrap()
        .pop()
        .expect("upstream captured request");
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path_and_query, "/v1/chat/completions?trace=1");
    assert_eq!(captured.authorization.as_deref(), Some("Bearer client-key"));
    assert_eq!(
        captured.cookie.as_deref(),
        Some("browser_cookie=should-forward")
    );
    assert_eq!(captured.trace.as_deref(), Some("abc"));
    assert_eq!(captured.hop_header, None);
    assert!(captured.body.contains("test-model"));
    Ok(())
}

#[tokio::test]
async fn forwards_responses_endpoint() -> anyhow::Result<()> {
    let captures = Captures::default();
    let upstream_url = spawn_capture_upstream(captures.clone()).await?;
    let (router, _pool) = test_router_with_upstream(&upstream_url).await?;

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"test-model","input":"hi"}"#))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let captured = captures
        .lock()
        .unwrap()
        .pop()
        .expect("upstream captured request");
    assert_eq!(captured.path_and_query, "/v1/responses");
    Ok(())
}

#[tokio::test]
async fn forwards_bare_responses_endpoint_for_codex_cli() -> anyhow::Result<()> {
    let captures = Captures::default();
    let upstream_url = spawn_capture_upstream(captures.clone()).await?;
    let (router, _pool) = test_router_with_upstream(&upstream_url).await?;

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/responses?trace=codex")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"test-model","input":"hi"}"#))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let captured = captures
        .lock()
        .unwrap()
        .pop()
        .expect("upstream captured request");
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path_and_query, "/responses?trace=codex");
    Ok(())
}

#[tokio::test]
async fn forwards_non_post_method_instead_of_returning_405() -> anyhow::Result<()> {
    let captures = Captures::default();
    let upstream_url = spawn_capture_upstream(captures.clone()).await?;
    let (router, _pool) = test_router_with_upstream(&upstream_url).await?;

    let response = router
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/responses?trace=method")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"op":"transparent"}"#))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let captured = captures
        .lock()
        .unwrap()
        .pop()
        .expect("upstream captured request");
    assert_eq!(captured.method, "PATCH");
    assert_eq!(captured.path_and_query, "/responses?trace=method");
    assert_eq!(captured.body, r#"{"op":"transparent"}"#);
    Ok(())
}

#[tokio::test]
async fn no_upstream_returns_503() -> anyhow::Result<()> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    let state = app::AppState::new(config, pool.clone()).await?;
    let router = app::router(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"test"}"#))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("\"code\":\"no_upstream\""));
    let request = wait_for_request_status(&pool, "no_upstream").await?;
    assert_eq!(request.attempt_count, 0);
    assert_eq!(request.final_http_status, Some(503));
    let payload = wait_for_payload(&pool, &request.id).await?;
    assert!(payload.request_body.is_empty());
    assert_eq!(payload.request_body_complete, 0);
    assert!(String::from_utf8_lossy(&payload.response_body).contains("\"code\":\"no_upstream\""));
    assert_eq!(payload.response_body_complete, 1);
    Ok(())
}

#[tokio::test]
async fn no_upstream_returns_503_without_reading_body_for_policy() -> anyhow::Result<()> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    storage::settings::save_runtime_settings(
        &pool,
        &RuntimeSettings {
            policy_enabled: true,
            request_record_enabled: true,
            response_header_timeout_ms: 1000,
            first_token_timeout_ms: 1000,
            max_attempts: 1,
            auto_retry_enabled: true,
        },
    )
    .await?;
    let state = app::AppState::new(config, pool.clone()).await?;
    let router = app::router(state);

    let response = tokio::time::timeout(
        Duration::from_millis(100),
        router.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from_stream(stream::pending::<
                    Result<Bytes, Infallible>,
                >()))?,
        ),
    )
    .await??;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("\"code\":\"no_upstream\""));
    let request = wait_for_request_status(&pool, "no_upstream").await?;
    assert_eq!(request.attempt_count, 0);
    assert_eq!(request.final_http_status, Some(503));
    let payload = wait_for_payload(&pool, &request.id).await?;
    assert!(payload.request_body.is_empty());
    assert_eq!(payload.request_body_complete, 0);
    assert!(String::from_utf8_lossy(&payload.response_body).contains("\"code\":\"no_upstream\""));
    assert_eq!(payload.response_body_complete, 1);
    Ok(())
}

#[tokio::test]
async fn request_record_completes_when_attempt_record_insert_fails() -> anyhow::Result<()> {
    let captures = Captures::default();
    let upstream_url = spawn_capture_upstream(captures.clone()).await?;
    let (router, pool) = test_router_with_upstream(&upstream_url).await?;
    sqlx::query("DROP TABLE attempt_records")
        .execute(&pool)
        .await?;

    let response = router.oneshot(proxy_request(None)?).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert!(String::from_utf8_lossy(&body).contains("\"ok\":true"));

    let (_status, attempt_count) =
        wait_for_request_status_without_attempt_table(&pool, "success").await?;
    assert_eq!(attempt_count, 1);
    Ok(())
}

#[tokio::test]
async fn direct_proxy_forwards_headers_without_policy_layer() -> anyhow::Result<()> {
    let captures = Captures::default();
    let upstream_url = spawn_capture_upstream(captures.clone()).await?;
    let (router, pool) = test_router_with_upstream_settings(
        &upstream_url,
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

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions?direct=1")
                .header("authorization", "Bearer client-direct-key")
                .header("cookie", "direct_cookie=should-forward")
                .header("x-trace", "direct-trace")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"test-model","stream":false,"messages":[]}"#,
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok()),
        Some("sid=upstream; Path=/")
    );
    assert!(response.headers().get(header::CONNECTION).is_none());
    assert!(response.headers().get("x-upstream-hop").is_none());
    assert!(response.headers().get("x-oai-proxy-request-id").is_none());
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert!(String::from_utf8_lossy(&body).contains("\"ok\":true"));
    let captured = captures
        .lock()
        .unwrap()
        .pop()
        .expect("upstream captured request");
    assert_eq!(captured.path_and_query, "/v1/chat/completions?direct=1");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer client-direct-key")
    );
    assert_eq!(
        captured.cookie.as_deref(),
        Some("direct_cookie=should-forward")
    );
    assert_eq!(captured.trace.as_deref(), Some("direct-trace"));
    assert_eq!(captured.hop_header, None);
    let request = wait_for_request_status(&pool, "success").await?;
    assert_eq!(request.attempt_count, 1);
    assert_eq!(request.final_http_status, Some(200));
    assert!(request.response_header_ms.is_some());
    assert_eq!(request.first_token_ms, None);
    assert!(request.duration_ms.is_some());

    let attempts = storage::records::list_attempts(&pool, &request.id).await?;
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, "success");
    assert_eq!(attempts[0].http_status, Some(200));
    assert!(attempts[0].response_header_ms.is_some());
    assert_eq!(attempts[0].first_token_ms, None);
    assert!(attempts[0].duration_ms.is_some());
    assert_eq!(attempts[0].emitted_to_client, 1);
    let payload = wait_for_payload(&pool, &request.id).await?;
    assert_eq!(
        payload.request_body,
        br#"{"model":"test-model","stream":false,"messages":[]}"#
    );
    assert_eq!(payload.request_body_complete, 1);
    assert!(String::from_utf8_lossy(&payload.response_body).contains("\"ok\":true"));
    assert_eq!(payload.response_body_complete, 1);
    assert_eq!(
        payload.request_body_bytes,
        br#"{"model":"test-model","stream":false,"messages":[]}"#.len() as i64
    );
    assert_eq!(
        payload.response_body_bytes,
        payload.response_body.len() as i64
    );
    Ok(())
}

#[tokio::test]
async fn direct_proxy_preserves_upstream_redirect_without_following() -> anyhow::Result<()> {
    let hits = Arc::new(AtomicUsize::new(0));
    let upstream_url = spawn_redirect_upstream(Arc::clone(&hits)).await?;
    let (router, pool) = test_router_with_upstream_settings(
        &upstream_url,
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

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"test-model","messages":[]}"#))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/redirected")
    );
    assert_eq!(
        response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok()),
        Some("redirected=1; Path=/")
    );
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert!(String::from_utf8_lossy(&body).contains("redirect"));
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let request = wait_for_request_status(&pool, "success").await?;
    assert_eq!(request.final_http_status, Some(307));
    let payload = wait_for_payload(&pool, &request.id).await?;
    assert_eq!(
        payload.request_body,
        br#"{"model":"test-model","messages":[]}"#
    );
    assert_eq!(payload.request_body_complete, 1);
    assert_eq!(payload.response_body, b"redirect");
    assert_eq!(payload.response_body_complete, 1);
    Ok(())
}

fn proxy_request(authorization: Option<&str>) -> anyhow::Result<Request<Body>> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json");
    if let Some(authorization) = authorization {
        builder = builder.header("authorization", authorization);
    }

    Ok(builder.body(Body::from(
        r#"{"model":"test-model","stream":false,"messages":[]}"#,
    ))?)
}

async fn spawn_capture_upstream(captures: Captures) -> anyhow::Result<String> {
    let app = Router::new()
        .route("/v1/chat/completions", any(capture_handler))
        .route("/v1/responses", any(capture_handler))
        .route("/responses", any(capture_handler))
        .route("/admin", any(capture_handler))
        .with_state(captures);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{addr}"))
}

async fn capture_handler(
    State(captures): State<Captures>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    captures.lock().unwrap().push(CapturedRequest {
        method: method.to_string(),
        path_and_query: uri
            .path_and_query()
            .map(|value| value.as_str().to_string())
            .unwrap_or_else(|| uri.path().to_string()),
        authorization: header_value(&headers, "authorization"),
        cookie: header_value(&headers, "cookie"),
        trace: header_value(&headers, "x-trace"),
        hop_header: header_value(&headers, "x-test-hop"),
        body: String::from_utf8_lossy(&body).to_string(),
    });
    let mut response = Json(json!({"ok": true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("sid=upstream; Path=/"),
    );
    response.headers_mut().insert(
        header::CONNECTION,
        HeaderValue::from_static("x-upstream-hop"),
    );
    response.headers_mut().insert(
        "x-upstream-hop",
        HeaderValue::from_static("must-not-forward"),
    );
    response
}

async fn spawn_redirect_upstream(hits: Arc<AtomicUsize>) -> anyhow::Result<String> {
    let app = Router::new()
        .route("/v1/chat/completions", post(redirect_handler))
        .route("/redirected", post(redirected_handler))
        .with_state(hits);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{addr}"))
}

async fn redirect_handler(State(hits): State<Arc<AtomicUsize>>) -> impl IntoResponse {
    hits.fetch_add(1, Ordering::SeqCst);
    let mut response = "redirect".into_response();
    *response.status_mut() = StatusCode::TEMPORARY_REDIRECT;
    response
        .headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static("/redirected"));
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("redirected=1; Path=/"),
    );
    response
}

async fn redirected_handler(State(hits): State<Arc<AtomicUsize>>) -> impl IntoResponse {
    hits.fetch_add(1, Ordering::SeqCst);
    Json(json!({"followed": true}))
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

async fn wait_for_request_status(
    pool: &sqlx::SqlitePool,
    expected: &str,
) -> anyhow::Result<storage::records::RequestRecord> {
    let mut observed = Vec::new();
    for _ in 0..100 {
        let requests = storage::records::list_recent_requests(pool, 1).await?;
        if let Some(request) = requests
            .into_iter()
            .find(|request| request.status == expected)
        {
            return Ok(request);
        }
        observed = storage::records::list_recent_requests(pool, 5)
            .await?
            .into_iter()
            .map(|request| request.status)
            .collect();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("request status did not become {expected}; observed: {observed:?}");
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

async fn wait_for_request_status_without_attempt_table(
    pool: &sqlx::SqlitePool,
    expected: &str,
) -> anyhow::Result<(String, i64)> {
    let mut observed = Vec::new();
    for _ in 0..100 {
        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT status, attempt_count FROM request_records")
                .fetch_all(pool)
                .await?;
        if let Some(row) = rows
            .iter()
            .find(|(status, _attempt_count)| status == expected)
        {
            return Ok(row.clone());
        }
        observed = rows.into_iter().map(|(status, _)| status).collect();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("request status did not become {expected}; observed: {observed:?}");
}

async fn test_router_with_upstream(
    upstream_url: &str,
) -> anyhow::Result<(Router, sqlx::SqlitePool)> {
    test_router_with_upstream_settings(
        upstream_url,
        RuntimeSettings {
            policy_enabled: true,
            request_record_enabled: true,
            response_header_timeout_ms: 1000,
            first_token_timeout_ms: 1000,
            max_attempts: 2,
            auto_retry_enabled: true,
        },
    )
    .await
}

async fn test_router_with_upstream_settings(
    upstream_url: &str,
    settings: RuntimeSettings,
) -> anyhow::Result<(Router, sqlx::SqlitePool)> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    storage::settings::save_runtime_settings(&pool, &settings).await?;
    upstreams::save_single_base_url(&pool, upstream_url).await?;
    let state = app::AppState::new(config, pool.clone()).await?;
    Ok((app::router(state), pool))
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
