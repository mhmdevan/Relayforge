CREATE TABLE IF NOT EXISTS admin_api_keys (
    id           UUID PRIMARY KEY,
    key_hash     TEXT NOT NULL UNIQUE,
    role         TEXT NOT NULL CHECK (role IN ('viewer','operator','admin')),
    description  TEXT NULL,
    is_active    BOOLEAN NOT NULL DEFAULT TRUE,

    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_admin_api_keys_active
    ON admin_api_keys (key_hash)
    WHERE is_active = TRUE;
