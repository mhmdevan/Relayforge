use anyhow::Context;
use sqlx::{Executor, Postgres};
use uuid::Uuid;

pub async fn try_reserve_event<'e, E>(
    exec: E,
    tenant_id: Uuid,
    event_id: &str,
    signature_timestamp: i64,
) -> anyhow::Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    if event_id.trim().is_empty() {
        anyhow::bail!("stripe event_id must not be empty");
    }

    let inserted: Option<(Uuid,)> = sqlx::query_as(
        r#"
        INSERT INTO stripe_event_idempotency (
            id, tenant_id, event_id, signature_timestamp
        )
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (tenant_id, event_id) DO NOTHING
        RETURNING id
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(event_id)
    .bind(signature_timestamp)
    .fetch_optional(exec)
    .await
    .context("failed to reserve stripe event idempotency key")?;

    Ok(inserted.is_some())
}
