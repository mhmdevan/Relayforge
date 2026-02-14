CREATE TABLE IF NOT EXISTS jobs (
    id                UUID PRIMARY KEY,

    tenant_id          UUID NOT NULL,
    source             TEXT NOT NULL,

    -- dedup_key is the *only* dedup truth:
    -- if provider_event_id exists: "provider:<id>"
    -- else: "hash:<payload_hash>"
    dedup_key          TEXT NOT NULL,

    provider_event_id  TEXT NULL,
    event_type         TEXT NULL,

    payload_hash       TEXT NOT NULL,

    status             TEXT NOT NULL CHECK (status IN (
        'received',
        'validated',
        'deduped',
        'queued',
        'processing',
        'succeeded',
        'failed',
        'dead_lettered'
    )),

    attempts           INTEGER NOT NULL DEFAULT 0,
    last_error         TEXT NULL,

    first_inbox_id     UUID NOT NULL REFERENCES webhook_inbox(id),
    last_inbox_id      UUID NOT NULL REFERENCES webhook_inbox(id),

    created_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_jobs_dedup
    ON jobs (tenant_id, source, dedup_key);

CREATE INDEX IF NOT EXISTS idx_jobs_status_time
    ON jobs (status, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_jobs_tenant_time
    ON jobs (tenant_id, updated_at DESC);
