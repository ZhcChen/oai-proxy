CREATE TABLE upstreams_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    base_url TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

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
