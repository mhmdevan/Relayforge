use crate::AppState;
use axum::response::{IntoResponse, Response};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::Utc;
use domain::DomainEvent;
use serde::Serialize;
use sha2::Sha256;
use std::collections::HashMap;
use tracing::{info, info_span, warn};
use uuid::Uuid;

#[derive(Debug, Serialize)]
struct WebhookAccepted {
    inbox_id: String,
    signature_valid: bool,
    accepted: bool,

    job_id: Option<String>,
    job_created: Option<bool>,
    deduped: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

fn json_error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(ErrorBody { error: msg.into() })).into_response()
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

fn parse_tenant_id(headers: &HeaderMap, default_tenant: Uuid) -> Result<Uuid, String> {
    if let Some(raw) = header_str(headers, "x-tenant-id") {
        raw.parse::<Uuid>()
            .map_err(|_| "invalid X-Tenant-Id (expected UUID)".to_string())
    } else {
        Ok(default_tenant)
    }
}

fn extract_correlation_id(headers: &HeaderMap) -> String {
    header_str(headers, "x-correlation-id")
        .or_else(|| header_str(headers, "x-request-id"))
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

fn request_trace_context(headers: &HeaderMap) -> HashMap<String, String> {
    let mut carrier = HashMap::new();
    for key in ["traceparent", "tracestate", "baggage"] {
        if let Some(value) = header_str(headers, key) {
            carrier.insert(key.to_string(), value);
        }
    }
    carrier
}

fn json_headers_subset(headers: &HeaderMap) -> serde_json::Value {
    let keys = [
        "content-type",
        "user-agent",
        "x-request-id",
        "x-correlation-id",
        "x-tenant-id",
        "x-github-delivery",
        "x-github-event",
        "x-hub-signature-256",
        "x-signature",
        "stripe-signature",
        "x-event-id",
        "x-event-type",
        "traceparent",
        "tracestate",
        "baggage",
    ];

    let mut map = serde_json::Map::new();
    for k in keys {
        if let Some(v) = header_str(headers, k) {
            map.insert(k.to_string(), serde_json::Value::String(v));
        }
    }
    serde_json::Value::Object(map)
}

fn stripe_signature_timestamp(headers: &HeaderMap) -> Result<i64, String> {
    let raw = header_str(headers, "stripe-signature")
        .ok_or_else(|| "missing Stripe-Signature".to_string())?;

    for part in raw.split(',') {
        let piece = part.trim();
        if let Some(v) = piece.strip_prefix("t=") {
            return v
                .trim()
                .parse::<i64>()
                .map_err(|_| "invalid Stripe-Signature timestamp".to_string());
        }
    }

    Err("Stripe-Signature missing timestamp".to_string())
}

fn validate_stripe_signature_freshness(
    signature_timestamp: i64,
    tolerance_seconds: i64,
) -> Result<(), String> {
    let now = Utc::now().timestamp();
    let age = (now - signature_timestamp).abs();
    if age > tolerance_seconds {
        return Err(format!(
            "stripe signature timestamp outside tolerance (age={}s tolerance={}s)",
            age, tolerance_seconds
        ));
    }

    Ok(())
}

pub async fn handle_webhook(
    State(state): State<AppState>,
    Path(source): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let source = source.to_lowercase();
    let correlation_id = extract_correlation_id(&headers);
    let span = info_span!(
        "webhook_ingest",
        correlation_id = %correlation_id,
        source = %source
    );
    let _span = span.enter();
    let mut trace_context = request_trace_context(&headers);
    common::observability::inject_current_context(&mut trace_context);
    let trace_id = common::observability::current_trace_id();

    let tenant_id = match parse_tenant_id(&headers, state.default_tenant_id) {
        Ok(t) => t,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, e),
    };

    if !state.tenant_rate_limiter.try_acquire(tenant_id).await {
        return json_error(
            StatusCode::TOO_MANY_REQUESTS,
            format!("tenant rate limit exceeded for tenant={tenant_id}"),
        );
    }

    let connector = match state.connector_registry.connector_for_source(&source) {
        Some(connector) => connector,
        None => return json_error(StatusCode::NOT_FOUND, "unknown source"),
    };

    let provider = connector.provider();

    let secret = match state
        .tenant_secret_store
        .resolve_secret(tenant_id, provider)
    {
        Some(v) => v,
        None => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "missing configured secret for tenant={} provider={}",
                    tenant_id, source
                ),
            )
        }
    };

    // 1) Signature verification (provider-based + tenant-aware secret)
    let signature_result = connector.verify(&headers, &body, &secret);
    let mut signature_valid = signature_result.is_ok();
    let mut signature_err = signature_result.err();

    // Stripe anti-replay check #1: timestamp freshness
    let mut stripe_signature_ts: Option<i64> = None;
    if signature_valid && source == "stripe" {
        match stripe_signature_timestamp(&headers).and_then(|ts| {
            validate_stripe_signature_freshness(ts, state.stripe_signature_tolerance_seconds)?;
            Ok(ts)
        }) {
            Ok(ts) => stripe_signature_ts = Some(ts),
            Err(err) => {
                signature_valid = false;
                signature_err = Some(err);
            }
        }
    }

    // 2) Parse + normalize only if signature is valid
    let mut normalized_event: Option<DomainEvent> = None;
    let mut normalization_err: Option<String> = None;

    if signature_valid {
        match connector
            .parse(&body)
            .and_then(|parsed| connector.normalize(&parsed, &headers))
        {
            Ok(event) => normalized_event = Some(event),
            Err(err) => normalization_err = Some(err),
        }
    }

    let provider_event_id = normalized_event
        .as_ref()
        .and_then(|event| event.provider_event_id.clone());
    let event_type = normalized_event
        .as_ref()
        .map(|event| event.event_type.clone());

    let payload_hash = sha256_hex(&body);
    let headers_json = json_headers_subset(&headers);

    // Initial acceptance decision (may be tightened later by anti-replay/quota)
    let mut status = if !signature_valid {
        StatusCode::UNAUTHORIZED
    } else if normalization_err.is_some() {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::ACCEPTED
    };

    let mut accepted = status == StatusCode::ACCEPTED;
    let mut rejected_reason = if !signature_valid {
        Some(signature_err.unwrap_or_else(|| "invalid signature".to_string()))
    } else {
        normalization_err.clone()
    };

    let mut tx = match state.pool.begin().await {
        Ok(t) => t,
        Err(e) => {
            warn!(error=%e, "failed to begin transaction");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to start transaction",
            );
        }
    };

    if accepted && source == "stripe" {
        let stripe_event_id = match provider_event_id.as_deref() {
            Some(v) if !v.trim().is_empty() => v,
            _ => {
                accepted = false;
                status = StatusCode::BAD_REQUEST;
                rejected_reason = Some("stripe payload missing field 'id'".to_string());
                ""
            }
        };

        if accepted {
            let signature_ts = stripe_signature_ts.unwrap_or_default();
            match infra::stripe_event_idempotency::try_reserve_event(
                tx.as_mut(),
                tenant_id,
                stripe_event_id,
                signature_ts,
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => {
                    accepted = false;
                    status = StatusCode::CONFLICT;
                    rejected_reason = Some("stripe event replay detected".to_string());
                }
                Err(e) => {
                    warn!(error=%e, "failed to reserve stripe idempotency key");
                    let _ = tx.rollback().await;
                    return json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "failed to reserve stripe event idempotency",
                    );
                }
            }
        }
    }

    if accepted {
        let quota_date = Utc::now().date_naive();
        match infra::tenant_daily_quota_usage::try_consume_daily_quota(
            tx.as_mut(),
            tenant_id,
            quota_date,
            state.tenant_daily_quota,
        )
        .await
        {
            Ok(Some(_used_count)) => {}
            Ok(None) => {
                accepted = false;
                status = StatusCode::TOO_MANY_REQUESTS;
                rejected_reason = Some(format!(
                    "tenant daily quota exceeded (quota={})",
                    state.tenant_daily_quota
                ));
            }
            Err(e) => {
                warn!(error=%e, "failed to consume tenant daily quota");
                let _ = tx.rollback().await;
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "failed to check tenant quota",
                );
            }
        }
    }

    // Insert inbox (even if rejected)
    let inbox_insert = infra::webhook_inbox::InboxInsert {
        tenant_id,
        source: source.clone(),
        provider_event_id: provider_event_id.clone(),
        event_type: event_type.clone(),
        signature_valid,
        accepted,
        rejected_reason: rejected_reason.clone(),
        payload_hash: payload_hash.clone(),
        raw_body: body.to_vec(),
        headers: headers_json,
    };

    let inbox_row = match infra::webhook_inbox::insert_inbox(tx.as_mut(), inbox_insert).await {
        Ok(r) => r,
        Err(e) => {
            warn!(error=%e, "failed to persist webhook inbox");
            let _ = tx.rollback().await;
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to persist inbox");
        }
    };

    let mut job_id: Option<String> = None;
    let mut job_created: Option<bool> = None;
    let mut deduped: Option<bool> = None;

    if accepted {
        let res = match infra::jobs::upsert_job_from_inbox(
            tx.as_mut(),
            tenant_id,
            &source,
            provider_event_id.clone(),
            event_type.clone(),
            &payload_hash,
            inbox_row.id,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                warn!(error=%e, "failed to upsert job");
                let _ = tx.rollback().await;
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to create job");
            }
        };

        job_id = Some(res.job_id.to_string());
        job_created = Some(res.inserted);
        deduped = Some(!res.inserted);

        if res.inserted {
            common::metrics::inc_jobs_created(&source);
            let payload = serde_json::json!({
                "job_id": res.job_id.to_string(),
                "tenant_id": tenant_id.to_string(),
                "source": source,
                "event_type": event_type,
                "provider_event_id": provider_event_id,
                "payload_hash": payload_hash,
                "correlation_id": correlation_id,
                "trace_id": trace_id.clone(),
                "trace_context": trace_context.clone(),
                "domain_event": normalized_event,
            });

            if let Err(e) = infra::outbox::insert_outbox(
                tx.as_mut(),
                infra::outbox::OutboxInsert {
                    job_id: res.job_id,
                    topic: "job.created".to_string(),
                    payload,
                },
            )
            .await
            {
                warn!(error=%e, "failed to insert outbox message");
                let _ = tx.rollback().await;
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "failed to enqueue outbox",
                );
            }
        }
    }

    if let Err(e) = tx.commit().await {
        warn!(error=%e, "failed to commit transaction");
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to commit transaction",
        );
    }

    info!(
        correlation_id = %correlation_id,
        tenant_id = %tenant_id,
        source = %source,
        signature_valid = signature_valid,
        accepted = accepted,
        inbox_id = %inbox_row.id,
        job_id = job_id.as_deref().unwrap_or(""),
        event_type = %event_type.as_deref().unwrap_or(""),
        provider_event_id = %provider_event_id.as_deref().unwrap_or(""),
        trace_id = %trace_id.as_deref().unwrap_or(""),
        status_code = %status.as_u16(),
        "webhook ingested"
    );

    (
        status,
        Json(WebhookAccepted {
            inbox_id: inbox_row.id.to_string(),
            signature_valid,
            accepted,
            job_id,
            job_created,
            deduped,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stripe_signature_timestamp_parser_works() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "stripe-signature",
            "t=1710000000,v1=abc"
                .parse()
                .expect("static stripe signature header should parse"),
        );

        let ts = stripe_signature_timestamp(&headers)
            .expect("expected stripe signature parser to extract timestamp");
        assert_eq!(ts, 1710000000);
    }

    #[test]
    fn stripe_signature_freshness_rejects_old_timestamps() {
        let now = Utc::now().timestamp();
        let too_old = now - 500;
        let err = validate_stripe_signature_freshness(too_old, 300)
            .expect_err("expected stale signature to be rejected");
        assert!(err.contains("outside tolerance"));
    }
}
