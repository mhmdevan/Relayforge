use crate::db::PgPool;
use anyhow::Context;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct TenantProviderSecretRow {
    pub tenant_id: Uuid,
    pub provider: String,
    pub secret: String,
}

pub async fn list_active(pool: &PgPool) -> anyhow::Result<Vec<TenantProviderSecretRow>> {
    sqlx::query_as::<_, TenantProviderSecretRow>(
        r#"
        SELECT tenant_id, provider, secret
        FROM tenant_provider_secrets
        WHERE is_active = TRUE
        ORDER BY tenant_id, provider
        "#,
    )
    .fetch_all(pool)
    .await
    .context("failed to load tenant provider secrets")
}
