use std::{env, net::SocketAddr, path::PathBuf};

pub const FIXED_PORT: u16 = 57_999;
pub const DEFAULT_ADMIN_TOKEN: &str = "admin";

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub bind_host: String,
    pub database_url: String,
    pub admin_token: String,
    pub admin_token_is_default: bool,
    pub admin_session_token: String,
    pub data_dir: PathBuf,
    pub default_max_body_bytes: i64,
    pub default_response_header_timeout_ms: i64,
    pub default_first_token_timeout_ms: i64,
    pub default_max_attempts: i64,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let data_dir = env::var("OAI_PROXY_DATA_DIR").unwrap_or_else(|_| "data".to_string());
        let data_dir = PathBuf::from(data_dir);
        let database_url = env::var("OAI_PROXY_DATABASE_URL").unwrap_or_else(|_| {
            format!("sqlite://{}/oai-proxy.sqlite3?mode=rwc", data_dir.display())
        });
        let admin_token =
            env::var("OAI_PROXY_ADMIN_TOKEN").unwrap_or_else(|_| DEFAULT_ADMIN_TOKEN.to_string());
        let admin_token_is_default = admin_token == DEFAULT_ADMIN_TOKEN;

        Self {
            bind_host: env::var("OAI_PROXY_BIND").unwrap_or_else(|_| "127.0.0.1".to_string()),
            database_url,
            admin_token,
            admin_token_is_default,
            admin_session_token: uuid::Uuid::new_v4().to_string(),
            data_dir,
            default_max_body_bytes: env_i64("OAI_PROXY_MAX_BODY_BYTES", 2 * 1024 * 1024),
            default_response_header_timeout_ms: env_i64(
                "OAI_PROXY_RESPONSE_HEADER_TIMEOUT_MS",
                15_000,
            ),
            default_first_token_timeout_ms: env_i64("OAI_PROXY_FIRST_TOKEN_TIMEOUT_MS", 20_000),
            default_max_attempts: env_i64("OAI_PROXY_MAX_ATTEMPTS", 3),
        }
    }

    pub fn listen_addr(&self) -> SocketAddr {
        format!("{}:{}", self.bind_host, FIXED_PORT)
            .parse()
            .expect("OAI_PROXY_BIND must be a valid host")
    }

    pub fn validate_startup(&self) -> Result<(), String> {
        if self.admin_token_is_default && !is_loopback_bind(&self.bind_host) {
            return Err(
                "OAI_PROXY_ADMIN_TOKEN must be set when OAI_PROXY_BIND is not a loopback address"
                    .to_string(),
            );
        }
        Ok(())
    }
}

fn is_loopback_bind(bind_host: &str) -> bool {
    matches!(bind_host, "127.0.0.1" | "localhost" | "::1")
}

fn env_i64(key: &str, default: i64) -> i64 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}
