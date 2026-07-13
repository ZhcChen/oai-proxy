use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode, Uri},
    routing::post,
};
use oai_proxy::{
    app,
    config::AppConfig,
    storage::{
        self,
        settings::RuntimeSettings,
        upstreams::{self, NewUpstream},
    },
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
    body: String,
}

type Captures = Arc<Mutex<Vec<CapturedRequest>>>;

#[tokio::test]
async fn forwards_request_and_rewrites_sensitive_headers() -> anyhow::Result<()> {
    let captures = Captures::default();
    let upstream_url = spawn_capture_upstream(captures.clone()).await?;
    let (router, _pool) = test_router_with_upstream(&upstream_url, 1024 * 1024).await?;

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions?trace=1")
                .header("authorization", "Bearer client-key")
                .header("cookie", "browser_cookie=should-not-forward")
                .header("x-trace", "abc")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"test-model","stream":false,"messages":[]}"#,
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let captured = captures
        .lock()
        .unwrap()
        .pop()
        .expect("upstream captured request");
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path_and_query, "/v1/chat/completions?trace=1");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-key")
    );
    assert_eq!(captured.cookie, None);
    assert_eq!(captured.trace.as_deref(), Some("abc"));
    assert!(captured.body.contains("test-model"));
    Ok(())
}

#[tokio::test]
async fn forwards_responses_endpoint() -> anyhow::Result<()> {
    let captures = Captures::default();
    let upstream_url = spawn_capture_upstream(captures.clone()).await?;
    let (router, _pool) = test_router_with_upstream(&upstream_url, 1024 * 1024).await?;

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
async fn body_limit_returns_openai_compatible_413() -> anyhow::Result<()> {
    let captures = Captures::default();
    let upstream_url = spawn_capture_upstream(captures.clone()).await?;
    let (router, _pool) = test_router_with_upstream(&upstream_url, 10).await?;

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"too-large"}"#))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("\"code\":\"payload_too_large\""));
    assert!(captures.lock().unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn no_enabled_upstream_returns_503() -> anyhow::Result<()> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    let state = app::AppState::new(config, pool)?;
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
    Ok(())
}

#[tokio::test]
async fn generated_proxy_key_is_required_when_present() -> anyhow::Result<()> {
    let captures = Captures::default();
    let upstream_url = spawn_capture_upstream(captures.clone()).await?;
    let (router, pool) = test_router_with_upstream(&upstream_url, 1024 * 1024).await?;
    storage::proxy_keys::create(&pool, "client", "valid-proxy-key").await?;

    let no_auth = router.clone().oneshot(proxy_request(None)?).await?;
    assert_eq!(no_auth.status(), StatusCode::UNAUTHORIZED);

    let bad_auth = router
        .clone()
        .oneshot(proxy_request(Some("Bearer wrong-key"))?)
        .await?;
    assert_eq!(bad_auth.status(), StatusCode::UNAUTHORIZED);

    let ok = router
        .oneshot(proxy_request(Some("Bearer valid-proxy-key"))?)
        .await?;
    assert_eq!(ok.status(), StatusCode::OK);
    let captured = captures
        .lock()
        .unwrap()
        .pop()
        .expect("upstream captured request");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer upstream-key")
    );
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
        .route("/v1/chat/completions", post(capture_handler))
        .route("/v1/responses", post(capture_handler))
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
) -> Json<serde_json::Value> {
    captures.lock().unwrap().push(CapturedRequest {
        method: method.to_string(),
        path_and_query: uri
            .path_and_query()
            .map(|value| value.as_str().to_string())
            .unwrap_or_else(|| uri.path().to_string()),
        authorization: header_value(&headers, "authorization"),
        cookie: header_value(&headers, "cookie"),
        trace: header_value(&headers, "x-trace"),
        body: String::from_utf8_lossy(&body).to_string(),
    });
    Json(json!({"ok": true}))
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

async fn test_router_with_upstream(
    upstream_url: &str,
    max_body_bytes: i64,
) -> anyhow::Result<(Router, sqlx::SqlitePool)> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    storage::settings::save_runtime_settings(
        &pool,
        &RuntimeSettings {
            max_body_bytes,
            response_header_timeout_ms: 1000,
            first_token_timeout_ms: 1000,
            max_attempts: 2,
            auto_retry_enabled: true,
        },
    )
    .await?;
    upstreams::create(
        &pool,
        &NewUpstream {
            name: "default".to_string(),
            base_url: upstream_url.to_string(),
            api_key: "upstream-key".to_string(),
            enabled: true,
            response_header_timeout_ms: None,
            first_token_timeout_ms: None,
            max_attempts: None,
        },
    )
    .await?;
    let state = app::AppState::new(config, pool.clone())?;
    Ok((app::router(state), pool))
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
