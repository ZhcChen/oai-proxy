UPDATE settings
SET
    value = '5000',
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE key = 'response_header_timeout_ms'
  AND value = '15000';

UPDATE settings
SET
    value = '10000',
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE key = 'first_token_timeout_ms'
  AND value = '20000';
