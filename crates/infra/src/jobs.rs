use crate::db::PgPool;
use anyhow::Context;
use uuid::Uuid;

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use sqlx::{Executor, Postgres};

#[derive(Debug, Clone)]
pub struct JobUpsertResult {
    pub job_id: Uuid,
    pub inserted: bool,
}

#[derive(Debug, Clone, FromRow)]
pub struct JobRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub source: String,
    pub dedup_key: String,
    pub provider_event_id: Option<String>,
    pub event_type: Option<String>,
    pub payload_hash: String,
    pub status: String,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub first_inbox_id: Uuid,
    pub last_inbox_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct JobProcessingInput {
    pub attempts: i32,
    pub raw_body: Vec<u8>,
}

pub fn build_dedup_key(provider_event_id: Option<&str>, payload_hash: &str) -> String {
    if let Some(id) = provider_event_id {
        format!("provider:{id}")
    } else {
        format!("hash:{payload_hash}")
    }
}

pub async fn upsert_job_from_inbox<'e, E>(
    exec: E,
    tenant_id: Uuid,
    source: &str,
    provider_event_id: Option<String>,
    event_type: Option<String>,
    payload_hash: &str,
    inbox_id: Uuid,
) -> anyhow::Result<JobUpsertResult>
where
    E: Executor<'e, Database = Postgres>,
{
    let dedup_key = build_dedup_key(provider_event_id.as_deref(), payload_hash);

    // inserted detection (xmax=0)
    let rec: (Uuid, bool) = sqlx::query_as(
        r#"
        INSERT INTO jobs (
            id, tenant_id, source, dedup_key,
            provider_event_id, event_type, payload_hash,
            status, attempts, last_error,
            first_inbox_id, last_inbox_id
        )
        VALUES (
            $1,$2,$3,$4,
            $5,$6,$7,
            'validated', 0, NULL,
            $8,$9
        )
        ON CONFLICT (tenant_id, source, dedup_key)
        DO UPDATE SET
            last_inbox_id = EXCLUDED.last_inbox_id,
            updated_at = NOW()
        RETURNING id, (xmax = 0) AS inserted
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(source)
    .bind(dedup_key)
    .bind(provider_event_id)
    .bind(event_type)
    .bind(payload_hash)
    .bind(inbox_id)
    .bind(inbox_id)
    .fetch_one(exec)
    .await
    .context("failed to upsert job")?;

    Ok(JobUpsertResult {
        job_id: rec.0,
        inserted: rec.1,
    })
}
pub async fn get_job(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<JobRow>> {
    let row = sqlx::query_as::<_, JobRow>(
        r#"
        SELECT
            id, tenant_id, source, dedup_key,
            provider_event_id, event_type, payload_hash,
            status, attempts, last_error,
            first_inbox_id, last_inbox_id,
            created_at, updated_at
        FROM jobs
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("failed to fetch job")?;

    Ok(row)
}

pub async fn get_job_for_update<'e, E>(exec: E, id: Uuid) -> anyhow::Result<Option<JobRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as::<_, JobRow>(
        r#"
        SELECT
            id, tenant_id, source, dedup_key,
            provider_event_id, event_type, payload_hash,
            status, attempts, last_error,
            first_inbox_id, last_inbox_id,
            created_at, updated_at
        FROM jobs
        WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(id)
    .fetch_optional(exec)
    .await
    .context("failed to fetch job for update")?;

    Ok(row)
}

pub async fn get_job_processing_input(
    pool: &PgPool,
    job_id: Uuid,
) -> anyhow::Result<Option<JobProcessingInput>> {
    let row = sqlx::query_as::<_, JobProcessingInput>(
        r#"
        SELECT
            j.attempts,
            i.raw_body
        FROM jobs j
        JOIN webhook_inbox i
          ON i.id = j.last_inbox_id
        WHERE j.id = $1
        "#,
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await
    .context("failed to fetch job processing input")?;

    Ok(row)
}

pub async fn mark_job_queued<'e, E>(exec: E, job_id: Uuid) -> anyhow::Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query(
        r#"
        UPDATE jobs
        SET status = 'queued', updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .execute(exec)
    .await
    .context("mark_job_queued failed")?;
    Ok(())
}

pub async fn try_mark_processing<'e, E>(exec: E, job_id: Uuid) -> anyhow::Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let res = sqlx::query(
        r#"
        UPDATE jobs
        SET status = 'processing',
            attempts = attempts + 1,
            updated_at = NOW()
        WHERE id = $1
          AND status = 'queued'
        "#,
    )
    .bind(job_id)
    .execute(exec)
    .await
    .context("try_mark_processing failed")?;

    Ok(res.rows_affected() == 1)
}

pub async fn mark_succeeded<'e, E>(exec: E, job_id: Uuid) -> anyhow::Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query(
        r#"
        UPDATE jobs
        SET status = 'succeeded',
            last_error = NULL,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .execute(exec)
    .await
    .context("mark_succeeded failed")?;
    Ok(())
}

pub async fn mark_failed_and_requeue<'e, E>(exec: E, job_id: Uuid, err: &str) -> anyhow::Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query(
        r#"
        UPDATE jobs
        SET status = 'queued',
            last_error = $2,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .bind(err)
    .execute(exec)
    .await
    .context("mark_failed_and_requeue failed")?;
    Ok(())
}

pub async fn mark_dead_lettered<'e, E>(exec: E, job_id: Uuid, err: &str) -> anyhow::Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query(
        r#"
        UPDATE jobs
        SET status = 'dead_lettered',
            last_error = $2,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .bind(err)
    .execute(exec)
    .await
    .context("mark_dead_lettered failed")?;
    Ok(())
}

pub async fn mark_reprocess_requested<'e, E>(exec: E, job_id: Uuid) -> anyhow::Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query(
        r#"
        UPDATE jobs
        SET status = 'validated',
            last_error = NULL,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .execute(exec)
    .await
    .context("mark_reprocess_requested failed")?;
    Ok(())
}
