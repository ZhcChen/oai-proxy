use axum::http::HeaderMap;

use crate::config::AppConfig;

pub const ADMIN_COOKIE: &str = "oai_proxy_admin";

pub fn is_authenticated(headers: &HeaderMap, config: &AppConfig) -> bool {
    let Some(cookie_header) = headers.get(axum::http::header::COOKIE) else {
        return false;
    };

    let Ok(cookie_text) = cookie_header.to_str() else {
        return false;
    };

    cookie_text.split(';').any(|part| {
        let mut pieces = part.trim().splitn(2, '=');
        matches!(
            (pieces.next(), pieces.next()),
            (Some(name), Some(value)) if name == ADMIN_COOKIE && value == config.admin_session_token
        )
    })
}

pub fn login_cookie(config: &AppConfig) -> String {
    format!(
        "{}={}; HttpOnly; SameSite=Lax; Path=/admin; Max-Age=604800",
        ADMIN_COOKIE, config.admin_session_token
    )
}

pub fn logout_cookie() -> String {
    format!("{ADMIN_COOKIE}=; HttpOnly; SameSite=Lax; Path=/admin; Max-Age=0")
}
