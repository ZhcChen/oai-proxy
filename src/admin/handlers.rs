use askama::Template;
use axum::{
    extract::{Form, State},
    http::{HeaderMap, header},
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;

use crate::{
    app::AppState,
    error::AppError,
    storage::{
        records::{self, RequestRecord, TrafficStats},
        settings::{self, RuntimeSettings},
        upstreams::{self, UpstreamError, UpstreamView},
    },
};

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardTemplate {
    settings: RuntimeSettings,
    upstream_count: i64,
    total_requests: i64,
    success_requests: i64,
    timeout_requests: i64,
    traffic_stats: TrafficStatsView,
    requests: Vec<RequestRecordView>,
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
    settings: RuntimeSettings,
    api_base_url: String,
    message: String,
    has_message: bool,
}

#[derive(Template)]
#[template(path = "upstreams.html")]
struct UpstreamsTemplate {
    upstream: UpstreamView,
    has_upstream: bool,
    message: String,
    has_message: bool,
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
    response_header_ms: String,
    first_token_ms: String,
    request_body_bytes: String,
    request_body_complete: String,
    response_body_bytes: String,
    response_body_complete: String,
    created_at: String,
    duration_ms: String,
    error_message: String,
}

#[derive(Clone)]
struct TrafficStatsView {
    first_token_min_ms: String,
    first_token_max_ms: String,
    response_min_ms: String,
    response_max_ms: String,
    timeout_filtered_attempts: i64,
    response_header_timeout_attempts: i64,
    first_token_timeout_attempts: i64,
}

impl From<TrafficStats> for TrafficStatsView {
    fn from(stats: TrafficStats) -> Self {
        Self {
            first_token_min_ms: format_optional_ms(stats.first_token_min_ms),
            first_token_max_ms: format_optional_ms(stats.first_token_max_ms),
            response_min_ms: format_optional_ms(stats.response_min_ms),
            response_max_ms: format_optional_ms(stats.response_max_ms),
            timeout_filtered_attempts: stats.timeout_filtered_attempts,
            response_header_timeout_attempts: stats.response_header_timeout_attempts,
            first_token_timeout_attempts: stats.first_token_timeout_attempts,
        }
    }
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
            response_header_ms: record
                .response_header_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            first_token_ms: record
                .first_token_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            request_body_bytes: record
                .request_body_bytes
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            request_body_complete: complete_label(record.request_body_complete),
            response_body_bytes: record
                .response_body_bytes
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            response_body_complete: complete_label(record.response_body_complete),
            created_at: record.created_at,
            duration_ms: record
                .duration_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            error_message: record.error_message.unwrap_or_default(),
        }
    }
}

fn format_optional_ms(value: Option<i64>) -> String {
    value
        .map(|value| format!("{value} ms"))
        .unwrap_or_else(|| "-".to_string())
}

fn complete_label(value: Option<i64>) -> String {
    match value {
        Some(1) => "完整".to_string(),
        Some(_) => "部分".to_string(),
        None => "-".to_string(),
    }
}

#[derive(Deserialize)]
pub struct SettingsForm {
    settings_form_version: Option<String>,
    policy_enabled: Option<String>,
    request_record_enabled: Option<String>,
    response_header_timeout_ms: i64,
    first_token_timeout_ms: i64,
    max_attempts: i64,
    auto_retry_enabled: Option<String>,
}

#[derive(Deserialize)]
pub struct SaveUpstreamForm {
    base_url: String,
}

pub async fn dashboard(State(state): State<AppState>) -> Result<Response, AppError> {
    let settings = state.runtime.snapshot().settings;
    let requests = records::list_recent_requests(&state.pool, 10)
        .await?
        .into_iter()
        .map(RequestRecordView::from)
        .collect();
    render(DashboardTemplate {
        settings,
        upstream_count: upstreams::count_configured(&state.pool).await?,
        total_requests: records::total_requests(&state.pool).await?,
        success_requests: records::count_by_status(&state.pool, "success").await?
            + records::count_by_status(&state.pool, "retried_success").await?,
        timeout_requests: records::count_by_status(&state.pool, "exhausted_timeout").await?,
        traffic_stats: TrafficStatsView::from(records::traffic_stats(&state.pool).await?),
        requests,
    })
}

pub async fn settings_page(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let settings = state.runtime.snapshot().settings;
    render_settings(&headers, settings, "", false).await
}

pub async fn save_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<SettingsForm>,
) -> Result<Response, AppError> {
    validate_positive(
        form.response_header_timeout_ms,
        "response_header_timeout_ms",
    )?;
    validate_positive(form.first_token_timeout_ms, "first_token_timeout_ms")?;
    validate_positive(form.max_attempts, "max_attempts")?;

    let current = state.runtime.snapshot().settings;
    let current_form = form.settings_form_version.as_deref() == Some("2");
    let runtime_settings = RuntimeSettings {
        policy_enabled: if current_form {
            form.policy_enabled.is_some()
        } else {
            current.policy_enabled
        },
        request_record_enabled: if current_form {
            form.request_record_enabled.is_some()
        } else {
            current.request_record_enabled
        },
        response_header_timeout_ms: form.response_header_timeout_ms,
        first_token_timeout_ms: form.first_token_timeout_ms,
        max_attempts: form.max_attempts,
        auto_retry_enabled: form.auto_retry_enabled.is_some(),
    };
    settings::save_runtime_settings(&state.pool, &runtime_settings).await?;
    state.refresh_runtime().await?;
    render_settings(&headers, runtime_settings, "配置已保存", true).await
}

pub async fn upstreams_page(State(state): State<AppState>) -> Result<Response, AppError> {
    render_upstreams(&state, "", false, "", false).await
}

pub async fn save_upstream(
    State(state): State<AppState>,
    _headers: HeaderMap,
    Form(form): Form<SaveUpstreamForm>,
) -> Result<Response, AppError> {
    if form.base_url.trim().is_empty() {
        return render_upstreams(&state, "", false, "上游 Base URL 必填", true).await;
    }

    match upstreams::save_single_base_url(&state.pool, &form.base_url).await {
        Ok(()) => {
            state.refresh_runtime().await?;
            render_upstreams(&state, "上游 Base URL 已保存", true, "", false).await
        }
        Err(UpstreamError::InvalidBaseUrl(error)) => {
            render_upstreams(
                &state,
                "",
                false,
                &format!("上游 Base URL 无效：{error}"),
                true,
            )
            .await
        }
        Err(UpstreamError::Database(error)) => Err(AppError::Database(error)),
    }
}

pub async fn requests_page(State(state): State<AppState>) -> Result<Response, AppError> {
    let requests = records::list_recent_requests(&state.pool, 100)
        .await?
        .into_iter()
        .map(RequestRecordView::from)
        .collect();
    render(RequestsTemplate { requests })
}

pub async fn requests_partial(State(state): State<AppState>) -> Result<Response, AppError> {
    let requests = records::list_recent_requests(&state.pool, 100)
        .await?
        .into_iter()
        .map(RequestRecordView::from)
        .collect();
    render(RequestsTableTemplate { requests })
}

async fn render_settings(
    headers: &HeaderMap,
    settings: RuntimeSettings,
    message: &str,
    has_message: bool,
) -> Result<Response, AppError> {
    render(SettingsTemplate {
        settings,
        api_base_url: api_base_url(headers),
        message: message.to_string(),
        has_message,
    })
}

async fn render_upstreams(
    state: &AppState,
    message: &str,
    has_message: bool,
    error: &str,
    has_error: bool,
) -> Result<Response, AppError> {
    let upstream = upstreams::get_configured(&state.pool)
        .await?
        .map(UpstreamView::from);
    let has_upstream = upstream.is_some();
    render(UpstreamsTemplate {
        upstream: upstream.unwrap_or_else(empty_upstream_view),
        has_upstream,
        message: message.to_string(),
        has_message,
        error: error.to_string(),
        has_error,
    })
}

fn empty_upstream_view() -> UpstreamView {
    UpstreamView {
        id: 0,
        name: String::new(),
        base_url: String::new(),
        created_at: String::new(),
    }
}

fn validate_positive(value: i64, field: &str) -> Result<(), AppError> {
    if value > 0 {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!("{field} 必须大于 0")))
    }
}

fn render<T: Template>(template: T) -> Result<Response, AppError> {
    Ok(Html(template.render()?).into_response())
}

fn api_base_url(headers: &HeaderMap) -> String {
    let proto = first_header_value(headers, "x-forwarded-proto")
        .filter(|value| matches!(*value, "http" | "https"))
        .unwrap_or("http");
    let host = first_header_value(headers, "x-forwarded-host")
        .or_else(|| first_header_value(headers, header::HOST.as_str()))
        .filter(|value| is_valid_authority(value))
        .unwrap_or("127.0.0.1:57999");
    format!("{proto}://{host}")
}

fn first_header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn is_valid_authority(value: &str) -> bool {
    !value.contains('/')
        && !value.contains('\\')
        && !value.contains('@')
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':' | '[' | ']'))
}
