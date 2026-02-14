use crate::db::PgPool;
use anyhow::Context;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct AdminApiKeyRow {
    pub id: Uuid,
    pub role: String,
}

pub async fn find_active_by_hash(
    pool: &PgPool,
    key_hash: &str,
) -> anyhow::Result<Option<AdminApiKeyRow>> {
    sqlx::query_as::<_, AdminApiKeyRow>(
        r#"
        SELECT id, role
        FROM admin_api_keys
        WHERE key_hash = $1
          AND is_active = TRUE
        LIMIT 1
        "#,
    )
    .bind(key_hash)
    .fetch_optional(pool)
    .await
    .context("failed to load admin api key")
}
