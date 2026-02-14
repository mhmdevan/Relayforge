// use crate::db::PgPool;
use anyhow::Context;
use serde_json::Value;
use sqlx::{Executor, Postgres};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct InboxInsert {
    pub tenant_id: Uuid,
    pub source: String,
    pub provider_event_id: Option<String>,
    pub event_type: Option<String>,

    pub signature_valid: bool,
    pub accepted: bool,
    pub rejected_reason: Option<String>,

    pub payload_hash: String,
    pub raw_body: Vec<u8>,
    pub headers: Value, // JSON object
}

#[derive(Debug, Clone)]
pub struct InboxRow {
    pub id: Uuid,
}

pub async fn insert_inbox<'e, E>(exec: E, data: InboxInsert) -> anyhow::Result<InboxRow>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::new_v4();

    let rec_id: (Uuid,) = sqlx::query_as(
        r#"
        INSERT INTO webhook_inbox (
            id, tenant_id, source, provider_event_id, event_type,
            signature_valid, accepted, rejected_reason,
            payload_hash, raw_body, headers
        )
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
        RETURNING id
        "#,
    )
    .bind(id)
    .bind(data.tenant_id)
    .bind(data.source)
    .bind(data.provider_event_id)
    .bind(data.event_type)
    .bind(data.signature_valid)
    .bind(data.accepted)
    .bind(data.rejected_reason)
    .bind(data.payload_hash)
    .bind(data.raw_body)
    .bind(data.headers)
    .fetch_one(exec)
    .await
    .context("failed to insert webhook_inbox row")?;

    Ok(InboxRow { id: rec_id.0 })
}
