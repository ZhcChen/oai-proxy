CREATE TABLE upstreams_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    base_url TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

INSERT INTO settings (key, value, updated_at)
SELECT
    'response_header_timeout_ms',
    CAST(response_header_timeout_ms AS TEXT),
    updated_at
FROM upstreams
WHERE enabled = 1
  AND response_header_timeout_ms IS NOT NULL
ORDER BY id ASC
LIMIT 1
ON CONFLICT(key) DO UPDATE SET
    value = excluded.value,
    updated_at = excluded.updated_at;

INSERT INTO settings (key, value, updated_at)
SELECT
    'first_token_timeout_ms',
    CAST(first_token_timeout_ms AS TEXT),
    updated_at
FROM upstreams
WHERE enabled = 1
  AND first_token_timeout_ms IS NOT NULL
ORDER BY id ASC
LIMIT 1
ON CONFLICT(key) DO UPDATE SET
    value = excluded.value,
    updated_at = excluded.updated_at;

INSERT INTO settings (key, value, updated_at)
SELECT
    'max_attempts',
    CAST(max_attempts AS TEXT),
    updated_at
FROM upstreams
WHERE enabled = 1
  AND max_attempts IS NOT NULL
ORDER BY id ASC
LIMIT 1
ON CONFLICT(key) DO UPDATE SET
    value = excluded.value,
    updated_at = excluded.updated_at;

INSERT INTO upstreams_new (
    id,
    name,
    base_url,
    created_at,
    updated_at
)
SELECT
    id,
    'default',
    base_url,
    created_at,
    updated_at
FROM upstreams
WHERE enabled = 1
ORDER BY id ASC
LIMIT 1;

DROP TABLE upstreams;
ALTER TABLE upstreams_new RENAME TO upstreams;
