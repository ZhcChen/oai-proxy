CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE upstreams (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    base_url TEXT NOT NULL,
    api_key TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    response_header_timeout_ms INTEGER,
    first_token_timeout_ms INTEGER,
    max_attempts INTEGER,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE proxy_keys (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    key_secret TEXT NOT NULL UNIQUE,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE request_records (
    id TEXT PRIMARY KEY,
    method TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    model TEXT,
    status TEXT NOT NULL,
    upstream_name TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    final_http_status INTEGER,
    error_message TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    completed_at TEXT,
    duration_ms INTEGER
);

CREATE TABLE attempt_records (
    id TEXT PRIMARY KEY,
    request_id TEXT NOT NULL,
    attempt_index INTEGER NOT NULL,
    upstream_id INTEGER,
    upstream_name TEXT NOT NULL,
    status TEXT NOT NULL,
    http_status INTEGER,
    response_header_ms INTEGER,
    first_token_ms INTEGER,
    timeout_reason TEXT,
    error_message TEXT,
    emitted_to_client INTEGER NOT NULL DEFAULT 0 CHECK (emitted_to_client IN (0, 1)),
    started_at TEXT NOT NULL,
    completed_at TEXT,
    duration_ms INTEGER,
    FOREIGN KEY(request_id) REFERENCES request_records(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_request_records_created_at ON request_records(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_request_records_status ON request_records(status);
CREATE INDEX IF NOT EXISTS idx_attempt_records_request_id ON attempt_records(request_id);
CREATE INDEX IF NOT EXISTS idx_attempt_records_timeout_reason ON attempt_records(timeout_reason);
