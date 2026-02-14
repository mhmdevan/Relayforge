use anyhow::Context;
use chrono::NaiveDate;
use sqlx::{Executor, Postgres};
use uuid::Uuid;

pub async fn try_consume_daily_quota<'e, E>(
    exec: E,
    tenant_id: Uuid,
    quota_date: NaiveDate,
    daily_quota: i64,
) -> anyhow::Result<Option<i64>>
where
    E: Executor<'e, Database = Postgres>,
{
    if daily_quota <= 0 {
        anyhow::bail!("daily_quota must be > 0");
    }

    let row: Option<(i64,)> = sqlx::query_as(
        r#"
        INSERT INTO tenant_daily_quota_usage (
            tenant_id, quota_date, used_count, updated_at
        )
        VALUES ($1, $2, 1, NOW())
        ON CONFLICT (tenant_id, quota_date)
        DO UPDATE
            SET used_count = tenant_daily_quota_usage.used_count + 1,
                updated_at = NOW()
        WHERE tenant_daily_quota_usage.used_count < $3
        RETURNING used_count
        "#,
    )
    .bind(tenant_id)
    .bind(quota_date)
    .bind(daily_quota)
    .fetch_optional(exec)
    .await
    .context("failed to consume tenant daily quota")?;

    Ok(row.map(|v| v.0))
}
