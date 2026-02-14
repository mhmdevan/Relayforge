CREATE TABLE IF NOT EXISTS outbox_messages (
    id              UUID PRIMARY KEY,
    job_id          UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,

    topic           TEXT NOT NULL,
    payload         JSONB NOT NULL,

    status          TEXT NOT NULL CHECK (status IN ('pending','sent','failed')),
    attempts        INTEGER NOT NULL DEFAULT 0,

    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_error      TEXT NULL,

    locked_until    TIMESTAMPTZ NULL,
    locked_by       TEXT NULL,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Only one "job.created" message per job
CREATE UNIQUE INDEX IF NOT EXISTS uq_outbox_job_topic
    ON outbox_messages (job_id, topic);

CREATE INDEX IF NOT EXISTS idx_outbox_pending
    ON outbox_messages (status, next_attempt_at)
    WHERE status = 'pending';
