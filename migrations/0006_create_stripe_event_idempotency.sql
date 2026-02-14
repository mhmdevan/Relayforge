CREATE TABLE IF NOT EXISTS stripe_event_idempotency (
    id                  UUID PRIMARY KEY,
    tenant_id           UUID NOT NULL,
    event_id            TEXT NOT NULL,
    signature_timestamp BIGINT NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT uq_stripe_event_per_tenant UNIQUE (tenant_id, event_id)
);

CREATE INDEX IF NOT EXISTS idx_stripe_event_idempotency_tenant_time
    ON stripe_event_idempotency (tenant_id, created_at DESC);
