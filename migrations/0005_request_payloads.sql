CREATE TABLE request_payloads (
    request_id TEXT PRIMARY KEY,
    request_body BLOB NOT NULL DEFAULT X'',
    request_body_bytes INTEGER NOT NULL DEFAULT 0,
    request_body_complete INTEGER NOT NULL DEFAULT 0 CHECK (request_body_complete IN (0, 1)),
    request_body_error TEXT,
    response_body BLOB NOT NULL DEFAULT X'',
    response_body_bytes INTEGER NOT NULL DEFAULT 0,
    response_body_complete INTEGER NOT NULL DEFAULT 0 CHECK (response_body_complete IN (0, 1)),
    response_body_error TEXT,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(request_id) REFERENCES request_records(id) ON DELETE CASCADE
);
