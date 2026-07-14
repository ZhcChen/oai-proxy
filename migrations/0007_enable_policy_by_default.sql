UPDATE settings
SET
    value = 'true',
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE key = 'policy_enabled'
  AND value IN ('false', '0', 'off', 'no');
