use std::{env, net::SocketAddr, path::PathBuf};

pub const FIXED_PORT: u16 = 57_999;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub bind_host: String,
    pub database_url: String,
    pub data_dir: PathBuf,
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

        Self {
            bind_host: env::var("OAI_PROXY_BIND").unwrap_or_else(|_| "127.0.0.1".to_string()),
            database_url,
            data_dir,
            default_response_header_timeout_ms: env_i64(
                "OAI_PROXY_RESPONSE_HEADER_TIMEOUT_MS",
                5_000,
            ),
            default_first_token_timeout_ms: env_i64("OAI_PROXY_FIRST_TOKEN_TIMEOUT_MS", 10_000),
            default_max_attempts: env_i64("OAI_PROXY_MAX_ATTEMPTS", 3),
        }
    }

    pub fn listen_addr(&self) -> SocketAddr {
        format!("{}:{}", self.bind_host, FIXED_PORT)
            .parse()
            .expect("OAI_PROXY_BIND must be a valid host")
    }
}

fn env_i64(key: &str, default: i64) -> i64 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}
