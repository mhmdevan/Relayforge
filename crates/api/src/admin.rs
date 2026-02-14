use crate::AppState;
use axum::{
    extract::{Extension, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use std::collections::HashMap;
use tracing::info_span;
use uuid::Uuid;

#[derive(Debug, Serialize)]
struct ReprocessAccepted {
    job_id: String,
    outbox_id: String,
    reprocessed: bool,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

fn json_error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(ErrorBody { error: msg.into() })).into_response()
}

fn extract_correlation_id(headers: &HeaderMap) -> String {
    headers
        .get("x-correlation-id")
        .and_then(|v| v.to_str().ok())
        .or_else(|| headers.get("x-request-id").and_then(|v| v.to_str().ok()))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

fn request_trace_context(headers: &HeaderMap) -> HashMap<String, String> {
    let mut carrier = HashMap::new();
    for key in ["traceparent", "tracestate", "baggage"] {
        if let Some(value) = headers.get(key).and_then(|v| v.to_str().ok()) {
            carrier.insert(key.to_string(), value.to_string());
        }
    }
    carrier
}

pub async fn reprocess_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Extension(principal): Extension<crate::admin_auth::AdminPrincipal>,
) -> Response {
    let correlation_id = extract_correlation_id(&headers);
    let span = info_span!(
        "admin_reprocess_job",
        correlation_id = %correlation_id,
        job_id = %id,
        admin_role = %principal.role.as_str(),
        admin_key_id = %principal
            .key_id
            .map(|v| v.to_string())
            .unwrap_or_else(|| "fallback".to_string())
    );
    let _span = span.enter();
    let mut trace_context = request_trace_context(&headers);
    common::observability::inject_current_context(&mut trace_context);
    let trace_id = common::observability::current_trace_id();

    let job_id: Uuid = match id.parse() {
        Ok(v) => v,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid job id"),
    };

    let mut tx = match state.pool.begin().await {
        Ok(v) => v,
        Err(_) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to start transaction",
            )
        }
    };

    let job = match infra::jobs::get_job_for_update(tx.as_mut(), job_id).await {
        Ok(v) => v,
        Err(_) => {
            let _ = tx.rollback().await;
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to fetch job");
        }
    };

    let Some(job) = job else {
        let _ = tx.rollback().await;
        return json_error(StatusCode::NOT_FOUND, "job not found");
    };

    if job.status != "dead_lettered" {
        let _ = tx.rollback().await;
        return json_error(
            StatusCode::CONFLICT,
            format!("job status must be dead_lettered, got {}", job.status),
        );
    }

    let payload = serde_json::json!({
        "job_id": job.id.to_string(),
        "tenant_id": job.tenant_id.to_string(),
        "source": job.source,
        "event_type": job.event_type,
        "provider_event_id": job.provider_event_id,
        "payload_hash": job.payload_hash,
        "correlation_id": correlation_id,
        "trace_id": trace_id,
        "trace_context": trace_context,
    });

    let outbox_id = match infra::outbox::insert_outbox(
        tx.as_mut(),
        infra::outbox::OutboxInsert {
            job_id: job.id,
            topic: "job.created".to_string(),
            payload,
        },
    )
    .await
    {
        Ok(v) => v,
        Err(_) => {
            let _ = tx.rollback().await;
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to enqueue outbox",
            );
        }
    };

    if infra::jobs::mark_reprocess_requested(tx.as_mut(), job.id)
        .await
        .is_err()
    {
        let _ = tx.rollback().await;
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to update job status",
        );
    }

    if tx.commit().await.is_err() {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to commit transaction",
        );
    }

    (
        StatusCode::ACCEPTED,
        Json(ReprocessAccepted {
            job_id: job.id.to_string(),
            outbox_id: outbox_id.to_string(),
            reprocessed: true,
        }),
    )
        .into_response()
}
