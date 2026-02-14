CREATE TABLE IF NOT EXISTS webhook_inbox (
    id                UUID PRIMARY KEY,
    tenant_id          UUID NOT NULL,
    source             TEXT NOT NULL,
    provider_event_id  TEXT NULL,
    event_type         TEXT NULL,

    signature_valid    BOOLEAN NOT NULL,
    accepted           BOOLEAN NOT NULL,
    rejected_reason    TEXT NULL,

    payload_hash       TEXT NOT NULL,
    raw_body           BYTEA NOT NULL,
    headers            JSONB NOT NULL DEFAULT '{}'::jsonb,

    received_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_webhook_inbox_tenant_source_time
    ON webhook_inbox (tenant_id, source, received_at DESC);

CREATE INDEX IF NOT EXISTS idx_webhook_inbox_source_time
    ON webhook_inbox (source, received_at DESC);
