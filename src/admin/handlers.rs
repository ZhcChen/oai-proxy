use askama::Template;
use axum::{
    extract::{Form, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use serde::Deserialize;

use crate::{
    app::AppState,
    error::AppError,
    storage::{
        records::{self, RequestRecord},
        settings::{self, RuntimeSettings},
        upstreams::{self, NewUpstream, UpstreamView},
    },
};

use super::auth;

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    error: String,
    has_error: bool,
}

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardTemplate {
    settings: RuntimeSettings,
    upstream_count: i64,
    total_requests: i64,
    success_requests: i64,
    timeout_requests: i64,
    requests: Vec<RequestRecordView>,
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
    settings: RuntimeSettings,
    message: String,
    has_message: bool,
}

#[derive(Template)]
#[template(path = "upstreams.html")]
struct UpstreamsTemplate {
    upstreams: Vec<UpstreamView>,
    error: String,
    has_error: bool,
}

#[derive(Template)]
#[template(path = "requests.html")]
struct RequestsTemplate {
    requests: Vec<RequestRecordView>,
}

#[derive(Template)]
#[template(path = "partials/requests_table.html")]
struct RequestsTableTemplate {
    requests: Vec<RequestRecordView>,
}

#[derive(Clone)]
struct RequestRecordView {
    id: String,
    method: String,
    endpoint: String,
    model: String,
    status: String,
    upstream_name: String,
    attempt_count: i64,
    final_http_status: String,
    retry_count: i64,
    created_at: String,
    duration_ms: String,
    error_message: String,
}

impl From<RequestRecord> for RequestRecordView {
    fn from(record: RequestRecord) -> Self {
        Self {
            id: record.id,
            method: record.method,
            endpoint: record.endpoint,
            model: record.model.unwrap_or_else(|| "-".to_string()),
            status: record.status,
            upstream_name: record.upstream_name.unwrap_or_else(|| "-".to_string()),
            attempt_count: record.attempt_count,
            final_http_status: record
                .final_http_status
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            retry_count: record.retry_count,
            created_at: record.created_at,
            duration_ms: record
                .duration_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            error_message: record.error_message.unwrap_or_default(),
        }
    }
}

#[derive(Deserialize)]
pub struct LoginForm {
    token: String,
}

#[derive(Deserialize)]
pub struct SettingsForm {
    max_body_bytes: i64,
    response_header_timeout_ms: i64,
    first_token_timeout_ms: i64,
    max_attempts: i64,
    auto_retry_enabled: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateUpstreamForm {
    name: String,
    base_url: String,
    api_key: String,
    enabled: Option<String>,
    response_header_timeout_ms: Option<String>,
    first_token_timeout_ms: Option<String>,
    max_attempts: Option<String>,
}

pub async fn login_page(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if auth::is_authenticated(&headers, &state.config) {
        return Ok(Redirect::to("/admin").into_response());
    }

    render(LoginTemplate {
        error: String::new(),
        has_error: false,
    })
}

pub async fn login(
    State(state): State<AppState>,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    if form.token == state.config.admin_token {
        let mut response = Redirect::to("/admin").into_response();
        response.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_str(&auth::login_cookie(&state.config))
                .map_err(|error| AppError::Internal(error.to_string()))?,
        );
        return Ok(response);
    }

    let mut response = render(LoginTemplate {
        error: "管理员 token 不正确".to_string(),
        has_error: true,
    })?;
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    Ok(response)
}

pub async fn logout() -> Result<Response, AppError> {
    let mut response = Redirect::to("/admin/login").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&auth::logout_cookie())
            .map_err(|error| AppError::Internal(error.to_string()))?,
    );
    Ok(response)
}

pub async fn dashboard(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if !auth::is_authenticated(&headers, &state.config) {
        return Ok(Redirect::to("/admin/login").into_response());
    }

    let settings = settings::get_runtime_settings(&state.pool, &state.config).await?;
    let requests = records::list_recent_requests(&state.pool, 10)
        .await?
        .into_iter()
        .map(RequestRecordView::from)
        .collect();
    render(DashboardTemplate {
        settings,
        upstream_count: upstreams::count_enabled(&state.pool).await?,
        total_requests: records::total_requests(&state.pool).await?,
        success_requests: records::count_by_status(&state.pool, "success").await?
            + records::count_by_status(&state.pool, "retried_success").await?,
        timeout_requests: records::count_by_status(&state.pool, "exhausted_timeout").await?,
        requests,
    })
}

pub async fn settings_page(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    require_admin(&state, &headers)?;
    let settings = settings::get_runtime_settings(&state.pool, &state.config).await?;
    render(SettingsTemplate {
        settings,
        message: String::new(),
        has_message: false,
    })
}

pub async fn save_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<SettingsForm>,
) -> Result<Response, AppError> {
    require_admin(&state, &headers)?;
    validate_positive(form.max_body_bytes, "max_body_bytes")?;
    validate_positive(
        form.response_header_timeout_ms,
        "response_header_timeout_ms",
    )?;
    validate_positive(form.first_token_timeout_ms, "first_token_timeout_ms")?;
    validate_positive(form.max_attempts, "max_attempts")?;

    let runtime_settings = RuntimeSettings {
        max_body_bytes: form.max_body_bytes,
        response_header_timeout_ms: form.response_header_timeout_ms,
        first_token_timeout_ms: form.first_token_timeout_ms,
        max_attempts: form.max_attempts,
        auto_retry_enabled: form.auto_retry_enabled.is_some(),
    };
    settings::save_runtime_settings(&state.pool, &runtime_settings).await?;
    render(SettingsTemplate {
        settings: runtime_settings,
        message: "配置已保存".to_string(),
        has_message: true,
    })
}

pub async fn upstreams_page(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    require_admin(&state, &headers)?;
    render_upstreams(&state, "", false).await
}

pub async fn create_upstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<CreateUpstreamForm>,
) -> Result<Response, AppError> {
    require_admin(&state, &headers)?;

    if form.name.trim().is_empty() || form.base_url.trim().is_empty() {
        return render_upstreams(&state, "名称和 base URL 必填", true).await;
    }

    let upstream = NewUpstream {
        name: form.name,
        base_url: form.base_url,
        api_key: form.api_key,
        enabled: form.enabled.is_some(),
        response_header_timeout_ms: parse_optional_i64(form.response_header_timeout_ms)?,
        first_token_timeout_ms: parse_optional_i64(form.first_token_timeout_ms)?,
        max_attempts: parse_optional_i64(form.max_attempts)?,
    };

    match upstreams::create(&state.pool, &upstream).await {
        Ok(_) => render_upstreams(&state, "", false).await,
        Err(error) => render_upstreams(&state, &format!("创建上游失败：{error}"), true).await,
    }
}

pub async fn requests_page(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    require_admin(&state, &headers)?;
    let requests = records::list_recent_requests(&state.pool, 100)
        .await?
        .into_iter()
        .map(RequestRecordView::from)
        .collect();
    render(RequestsTemplate { requests })
}

pub async fn requests_partial(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    require_admin(&state, &headers)?;
    let requests = records::list_recent_requests(&state.pool, 100)
        .await?
        .into_iter()
        .map(RequestRecordView::from)
        .collect();
    render(RequestsTableTemplate { requests })
}

async fn render_upstreams(
    state: &AppState,
    error: &str,
    has_error: bool,
) -> Result<Response, AppError> {
    let upstreams = upstreams::list_all(&state.pool)
        .await?
        .into_iter()
        .map(UpstreamView::from)
        .collect();
    render(UpstreamsTemplate {
        upstreams,
        error: error.to_string(),
        has_error,
    })
}

fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    if auth::is_authenticated(headers, &state.config) {
        Ok(())
    } else {
        Err(AppError::Unauthorized("unauthorized".to_string()))
    }
}

fn validate_positive(value: i64, field: &str) -> Result<(), AppError> {
    if value > 0 {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!("{field} 必须大于 0")))
    }
}

fn parse_optional_i64(value: Option<String>) -> Result<Option<i64>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }

    let parsed = value
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest(format!("{value} 不是有效整数")))?;
    if parsed <= 0 {
        return Err(AppError::BadRequest("可选超时值必须大于 0".to_string()));
    }
    Ok(Some(parsed))
}

fn render<T: Template>(template: T) -> Result<Response, AppError> {
    Ok(Html(template.render()?).into_response())
}
