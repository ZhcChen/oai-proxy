use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    routing::post,
};
use oai_proxy::{app, config::AppConfig, storage};
use std::path::PathBuf;
use tokio::net::TcpListener;
use tower::ServiceExt;

#[tokio::test]
async fn admin_dashboard_renders_without_login() -> anyhow::Result<()> {
    let router = test_router().await?;
    let response = router
        .oneshot(Request::builder().uri("/admin").body(Body::empty())?)
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn root_redirects_to_admin() -> anyhow::Result<()> {
    let router = test_router().await?;
    let response = router
        .oneshot(Request::builder().uri("/").body(Body::empty())?)
        .await?;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/admin")
    );
    Ok(())
}

#[tokio::test]
async fn settings_page_renders_base_url_from_request_host() -> anyhow::Result<()> {
    let router = test_router().await?;
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/settings")
                .header(header::HOST, "ignored.example.test")
                .header("x-forwarded-proto", "https,http")
                .header("x-forwarded-host", "proxy.example.test, edge.example.test")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("https://proxy.example.test"));
    assert!(text.contains("/v1/chat/completions"));
    assert!(!text.contains("代理 API Key"));
    assert!(!text.contains("/admin/api-keys"));
    assert!(!text.contains("opk_"));
    assert!(!text.contains("请求体上限"));
    Ok(())
}

#[tokio::test]
async fn legacy_settings_post_preserves_new_switches() -> anyhow::Result<()> {
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
                .uri("/admin/settings")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(
                    "response_header_timeout_ms=111&first_token_timeout_ms=222&max_attempts=1",
                ))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);

    let settings = storage::settings::get_runtime_settings(&pool, &test_config()).await?;
    assert!(settings.policy_enabled);
    assert!(settings.request_record_enabled);
    assert_eq!(settings.response_header_timeout_ms, 111);
    Ok(())
}

#[tokio::test]
async fn creating_upstream_refreshes_runtime_cache() -> anyhow::Result<()> {
    let upstream_url = spawn_ok_upstream().await?;
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    let state = app::AppState::new(config, pool).await?;
    let router = app::router(state);

    let before = router.clone().oneshot(proxy_request(None)?).await?;
    assert_eq!(before.status(), StatusCode::SERVICE_UNAVAILABLE);

    let form = format!("base_url={upstream_url}");
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/upstreams")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("hx-request", "true")
                .body(Body::from(form))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("上游 Base URL 已保存"));
    assert!(text.contains(&upstream_url));

    let after = router.oneshot(proxy_request(None)?).await?;
    assert_eq!(after.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn saving_upstream_replaces_existing_single_base_url() -> anyhow::Result<()> {
    let first_upstream_url = spawn_ok_upstream().await?;
    let second_upstream_url = spawn_ok_upstream().await?;
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    storage::upstreams::save_single_base_url(&pool, &first_upstream_url).await?;
    let state = app::AppState::new(config, pool.clone()).await?;
    let router = app::router(state);

    let form = format!("base_url={second_upstream_url}");
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/upstreams")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("hx-request", "true")
                .body(Body::from(form))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("上游 Base URL 已保存"));
    assert!(text.contains(&second_upstream_url));
    assert!(!text.contains("上游 Base URL 已存在"));
    assert!(!text.contains("alert("));
    assert!(!text.contains("confirm("));
    assert!(!text.contains("prompt("));
    let upstreams = storage::upstreams::list_all(&pool).await?;
    assert_eq!(upstreams.len(), 1);
    assert_eq!(upstreams[0].base_url, second_upstream_url);

    let refreshed = router
        .oneshot(
            Request::builder()
                .uri("/admin/upstreams")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(refreshed.status(), StatusCode::OK);
    let body = to_bytes(refreshed.into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(!text.contains("role=\"dialog\""));
    assert!(!text.contains("上游 Base URL 已存在"));
    assert!(text.contains(&second_upstream_url));
    Ok(())
}

#[tokio::test]
async fn upstreams_page_only_renders_single_base_url_form() -> anyhow::Result<()> {
    let router = test_router().await?;
    let response = router
        .oneshot(
            Request::builder()
                .uri("/admin/upstreams")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("上游 Base URL"));
    assert!(text.contains("name=\"base_url\""));
    assert!(!text.contains("name=\"name\""));
    assert!(!text.contains("name=\"api_key\""));
    assert!(!text.contains("name=\"enabled\""));
    assert!(!text.contains("/toggle"));
    Ok(())
}

#[tokio::test]
async fn requests_partial_renders_fragment_without_login() -> anyhow::Result<()> {
    let router = test_router().await?;
    let response = router
        .oneshot(
            Request::builder()
                .uri("/admin/partials/requests")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

async fn test_router() -> anyhow::Result<axum::Router> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    let state = app::AppState::new(config, pool).await?;
    Ok(app::router(state))
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

fn proxy_request(authorization: Option<&str>) -> anyhow::Result<Request<Body>> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(authorization) = authorization {
        builder = builder.header(header::AUTHORIZATION, authorization);
    }
    Ok(builder.body(Body::from(r#"{"model":"test","messages":[]}"#))?)
}

async fn spawn_ok_upstream() -> anyhow::Result<String> {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async { Json(serde_json::json!({"ok": true})) }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{addr}"))
}
