use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use oai_proxy::{app, config::AppConfig, storage};
use std::path::PathBuf;
use tower::ServiceExt;

#[tokio::test]
async fn admin_login_sets_http_only_cookie() -> anyhow::Result<()> {
    let router = test_router().await?;
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("token=admin"))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(cookie.contains("oai_proxy_admin=session"));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("Path=/admin"));
    Ok(())
}

#[tokio::test]
async fn authenticated_admin_dashboard_renders_html() -> anyhow::Result<()> {
    let router = test_router().await?;
    let response = router
        .oneshot(
            Request::builder()
                .uri("/admin")
                .header(header::COOKIE, "oai_proxy_admin=session")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn authenticated_requests_partial_renders_fragment() -> anyhow::Result<()> {
    let router = test_router().await?;
    let response = router
        .oneshot(
            Request::builder()
                .uri("/admin/partials/requests")
                .header(header::COOKIE, "oai_proxy_admin=session")
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
        admin_token: "admin".to_string(),
        admin_token_is_default: true,
        admin_session_token: "session".to_string(),
        data_dir: PathBuf::from("data"),
        default_max_body_bytes: 1024 * 1024,
        default_response_header_timeout_ms: 1000,
        default_first_token_timeout_ms: 1000,
        default_max_attempts: 2,
    }
}
