use crate::db::PgPool;
use anyhow::Context;
use serde_json::Value;
use sqlx::types::Json;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct OutboxInsert {
    pub job_id: Uuid,
    pub topic: String,
    pub payload: Value,
}

#[derive(Debug, Clone, FromRow)]
pub struct OutboxMessage {
    pub id: Uuid,
    pub job_id: Uuid,
    pub topic: String,
    pub payload: Json<Value>,
    pub attempts: i32,
}

pub async fn insert_outbox<'e, E>(exec: E, data: OutboxInsert) -> anyhow::Result<Uuid>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let id = Uuid::new_v4();

    let rec: (Uuid,) = sqlx::query_as(
        r#"
        INSERT INTO outbox_messages (
            id, job_id, topic, payload, status,
            attempts, next_attempt_at, last_error, locked_until, locked_by
        )
        VALUES (
            $1, $2, $3, $4, 'pending',
            0, NOW(), NULL, NULL, NULL
        )
        ON CONFLICT (job_id, topic)
        DO UPDATE SET
            payload = EXCLUDED.payload,
            status = 'pending',
            attempts = 0,
            next_attempt_at = NOW(),
            last_error = NULL,
            locked_until = NULL,
            locked_by = NULL,
            updated_at = NOW()
        RETURNING id
        "#,
    )
    .bind(id)
    .bind(data.job_id)
    .bind(data.topic)
    .bind(Json(data.payload))
    .fetch_one(exec)
    .await
    .context("failed to insert outbox message")?;

    Ok(rec.0)
}

pub async fn claim_batch(
    pool: &PgPool,
    publisher_id: &str,
    limit: i64,
    lock_seconds: i32,
) -> anyhow::Result<Vec<OutboxMessage>> {
    // Claim pending rows safely with SKIP LOCKED (multi-worker safe)
    let rows = sqlx::query_as::<sqlx::Postgres, OutboxMessage>(
        r#"
    WITH cte AS (
        SELECT id
        FROM outbox_messages
        WHERE status = 'pending'
          AND next_attempt_at <= NOW()
          AND (locked_until IS NULL OR locked_until < NOW())
        ORDER BY created_at
        LIMIT $1
        FOR UPDATE SKIP LOCKED
    )
    UPDATE outbox_messages o
    SET locked_until = NOW() + ($2 * INTERVAL '1 second'),
        locked_by    = $3,
        updated_at   = NOW()
    FROM cte
    WHERE o.id = cte.id
    RETURNING o.id, o.job_id, o.topic, o.payload, o.attempts
    "#,
    )
    .bind(limit)
    .bind(lock_seconds)
    .bind(publisher_id)
    .fetch_all(pool)
    .await
    .context("failed to claim outbox batch")?;

    Ok(rows)
}

pub async fn mark_sent<'e, E>(exec: E, id: Uuid) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        r#"
        UPDATE outbox_messages
        SET status = 'sent',
            locked_until = NULL,
            locked_by = NULL,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(id)
    .execute(exec)
    .await
    .context("failed to mark outbox sent")?;
    Ok(())
}

pub async fn mark_retry(
    pool: &PgPool,
    id: Uuid,
    err: &str,
    next_attempt_seconds: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE outbox_messages
        SET status = 'pending',
            attempts = attempts + 1,
            last_error = $2,
            next_attempt_at = NOW() + ($3 * INTERVAL '1 second'),
            locked_until = NULL,
            locked_by = NULL,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(err)
    .bind(next_attempt_seconds)
    .execute(pool)
    .await
    .context("failed to mark outbox retry")?;

    Ok(())
}

pub fn backoff_seconds(attempt: i32) -> i64 {
    // exponential backoff with cap: 1,2,4,8,16,32,60,60,...
    let base = 2_i64.pow(attempt.min(6) as u32); // cap growth
    base.min(60)
}

pub async fn count_pending(pool: &PgPool) -> anyhow::Result<i64> {
    let count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM outbox_messages
        WHERE status = 'pending'
        "#,
    )
    .fetch_one(pool)
    .await
    .context("failed to count pending outbox messages")?;

    Ok(count)
}
