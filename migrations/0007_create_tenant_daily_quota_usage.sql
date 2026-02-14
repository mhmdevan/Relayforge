CREATE TABLE IF NOT EXISTS tenant_daily_quota_usage (
    tenant_id   UUID NOT NULL,
    quota_date  DATE NOT NULL,
    used_count  BIGINT NOT NULL DEFAULT 0,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    PRIMARY KEY (tenant_id, quota_date)
);

CREATE INDEX IF NOT EXISTS idx_tenant_daily_quota_usage_date
    ON tenant_daily_quota_usage (quota_date);
