CREATE TABLE IF NOT EXISTS tenant_provider_secrets (
    id          UUID PRIMARY KEY,
    tenant_id   UUID NOT NULL,
    provider    TEXT NOT NULL CHECK (provider IN ('generic','github','stripe')),
    secret      TEXT NOT NULL,
    is_active   BOOLEAN NOT NULL DEFAULT TRUE,

    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT uq_tenant_provider UNIQUE (tenant_id, provider)
);

CREATE INDEX IF NOT EXISTS idx_tenant_provider_secrets_active
    ON tenant_provider_secrets (tenant_id, provider)
    WHERE is_active = TRUE;
