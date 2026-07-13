use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use oai_proxy::{app, config::AppConfig, storage};
use std::path::PathBuf;
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
    Ok(())
}

#[tokio::test]
async fn settings_page_generates_proxy_api_key() -> anyhow::Result<()> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    let state = app::AppState::new(config, pool.clone())?;
    let router = app::router(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/api-keys")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("name=test-client"))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("opk_"));
    assert!(text.contains("test-client"));
    assert_eq!(storage::proxy_keys::list_all(&pool).await?.len(), 1);
    let key = extract_generated_key(&text).expect("generated key is shown once");
    assert!(storage::proxy_keys::is_authorized(&pool, Some(&key)).await?);
    Ok(())
}

#[tokio::test]
async fn duplicate_api_key_name_renders_form_message() -> anyhow::Result<()> {
    let config = test_config();
    let pool = storage::connect(&config.database_url).await?;
    storage::migrate(&pool).await?;
    storage::settings::ensure_defaults(&pool, &config).await?;
    let state = app::AppState::new(config, pool.clone())?;
    let router = app::router(state);

    for _ in 0..2 {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/api-keys")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from("name=dup-client"))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let text = String::from_utf8_lossy(&body);
        if text.contains("API Key 名称已存在") {
            assert_eq!(storage::proxy_keys::list_all(&pool).await?.len(), 1);
            return Ok(());
        }
    }

    anyhow::bail!("duplicate key name did not render a form message");
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
    let state = app::AppState::new(config, pool)?;
    Ok(app::router(state))
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

fn extract_generated_key(text: &str) -> Option<String> {
    let start = text.find("opk_")?;
    let rest = &text[start..];
    let end = rest
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}
