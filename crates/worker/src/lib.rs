pub mod mq;

use anyhow::Context;
use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use common::config::LogSettings;
use mq::{ConsumeAction, RabbitConfig};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tracing::{info, info_span, warn};
use uuid::Uuid;

type Db = sqlx::Postgres;
pub type Pool = sqlx::Pool<Db>;

#[derive(Debug, Clone)]
enum FailureMode {
    Transient,
    Permanent,
}

pub async fn run_from_env() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let log = LogSettings {
        filter: std::env::var("LOG__FILTER").unwrap_or_else(|_| "info".to_string()),
        json: parse_env_bool("LOG__JSON", false),
    };
    common::observability::init_tracing("relayforge-worker", &log)?;

    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL missing")?;
    let amqp_addr =
        std::env::var("AMQP_ADDR").unwrap_or_else(|_| "amqp://127.0.0.1:5672/%2f".into());

    let rabbit_cfg = RabbitConfig {
        addr: amqp_addr,
        exchange: std::env::var("RABBIT_EXCHANGE").unwrap_or_else(|_| "relayforge.events".into()),
        dlx: std::env::var("RABBIT_DLX").unwrap_or_else(|_| "relayforge.dlx".into()),
        queue: std::env::var("RABBIT_QUEUE").unwrap_or_else(|_| "relayforge.jobs".into()),
        retry_queue: std::env::var("RABBIT_RETRY_QUEUE")
            .unwrap_or_else(|_| "relayforge.jobs.retry".into()),
        dlq: std::env::var("RABBIT_DLQ").unwrap_or_else(|_| "relayforge.jobs.dlq".into()),
        prefetch: std::env::var("RABBIT_PREFETCH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50),
        retry_ttl_ms: std::env::var("RABBIT_RETRY_TTL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5000),
    };

    let max_attempts: i32 = std::env::var("WORKER_MAX_ATTEMPTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let max_concurrency: usize = std::env::var("WORKER_MAX_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32);

    let mode = std::env::var("WORKER_MODE").unwrap_or_else(|_| "both".into());
    let metrics_addr =
        std::env::var("WORKER_METRICS_ADDR").unwrap_or_else(|_| "0.0.0.0:9091".to_string());

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .context("db connect failed")?;

    let rabbit = match mode.as_str() {
        "consumer" => Arc::new(mq::Rabbit::connect_consumer(rabbit_cfg).await?),
        _ => Arc::new(mq::Rabbit::connect_publisher(rabbit_cfg).await?),
    };

    let metrics_task = tokio::spawn(run_metrics_server(metrics_addr));
    let run_result = run(pool, rabbit, &mode, max_attempts, max_concurrency).await;
    metrics_task.abort();
    let _ = metrics_task.await;
    run_result
}

pub async fn run(
    pool: Pool,
    rabbit: Arc<mq::Rabbit>,
    mode: &str,
    max_attempts: i32,
    max_concurrency: usize,
) -> anyhow::Result<()> {
    info!(mode, "worker started");

    match mode {
        "publisher" => {
            publisher_loop(pool.clone(), rabbit.clone()).await?;
        }
        "consumer" => {
            consumer_loop(pool.clone(), rabbit.clone(), max_attempts, max_concurrency).await?;
        }
        _ => {
            let pool_p = pool.clone();
            let rabbit_p = rabbit.clone();
            let p = tokio::spawn(async move { publisher_loop(pool_p, rabbit_p).await });

            let pool_c = pool.clone();
            // Use a separate channel/connection for consumer to avoid
            // head-of-line and delivery issues when sharing one channel.
            let rabbit_c = Arc::new(mq::Rabbit::connect_consumer(rabbit.cfg.clone()).await?);
            let c = tokio::spawn(async move {
                consumer_loop(pool_c, rabbit_c, max_attempts, max_concurrency).await
            });

            tokio::select! {
                res = p => { res??; }
                res = c => { res??; }
                _ = tokio::signal::ctrl_c() => {
                    info!("ctrl-c received, shutting down");
                }
            }
        }
    }

    Ok(())
}

fn parse_env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(default)
}

async fn metrics_handler() -> Response {
    match common::metrics::render_metrics() {
        Ok(body) => {
            let mut response = Response::new(body.into());
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
            );
            response
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to render metrics: {err}"),
        )
            .into_response(),
    }
}

async fn run_metrics_server(metrics_addr: String) -> anyhow::Result<()> {
    let app = Router::new().route("/metrics", get(metrics_handler));
    let listener = tokio::net::TcpListener::bind(&metrics_addr)
        .await
        .with_context(|| format!("failed to bind metrics endpoint on {metrics_addr}"))?;
    info!(metrics_addr = %metrics_addr, "worker metrics endpoint listening");
    axum::serve(listener, app)
        .await
        .context("worker metrics server failed")
}

async fn refresh_outbox_pending_gauge(pool: &Pool) {
    match infra::outbox::count_pending(pool).await {
        Ok(count) => common::metrics::set_outbox_pending_count(count),
        Err(err) => warn!(error = %err, "failed to update outbox_pending_count gauge"),
    }
}

fn extract_trace_context(payload: &Value) -> HashMap<String, String> {
    const KEYS: [&str; 3] = ["traceparent", "tracestate", "baggage"];
    let mut carrier = HashMap::new();

    if let Some(obj) = payload.get("trace_context").and_then(Value::as_object) {
        for key in KEYS {
            if let Some(v) = obj.get(key).and_then(Value::as_str) {
                carrier.insert(key.to_string(), v.to_string());
            }
        }
    }

    for key in KEYS {
        if carrier.contains_key(key) {
            continue;
        }
        if let Some(v) = payload.get(key).and_then(Value::as_str) {
            carrier.insert(key.to_string(), v.to_string());
        }
    }

    carrier
}

struct FailureContext<'a> {
    pool: &'a Pool,
    rabbit: &'a mq::Rabbit,
    job_id: Uuid,
    correlation_id: &'a str,
    trace_context: &'a HashMap<String, String>,
    max_attempts: i32,
    processing_started_at: std::time::Instant,
}

/// Publish one outbox batch and return processed count.
pub async fn publish_outbox_once(
    pool: &Pool,
    rabbit: &mq::Rabbit,
    publisher_id: &str,
    limit: i64,
    lock_seconds: i32,
) -> anyhow::Result<usize> {
    let batch = infra::outbox::claim_batch(pool, publisher_id, limit, lock_seconds)
        .await
        .context("claim_batch failed")?;

    if batch.is_empty() {
        return Ok(0);
    }

    let processed_count = batch.len();

    for msg in batch {
        let routing_key = msg.topic.clone();
        let correlation_id = msg
            .payload
            .0
            .get("correlation_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let trace_context = extract_trace_context(&msg.payload.0);
        let publish_span = info_span!(
            "publish_outbox_message",
            correlation_id = %correlation_id,
            outbox_id = %msg.id,
            job_id = %msg.job_id,
            routing_key = %routing_key
        );
        common::observability::set_parent_from_carrier(&publish_span, &trace_context);
        let _publish_span = publish_span.enter();

        let mut publish_trace_context = trace_context;
        common::observability::inject_current_context(&mut publish_trace_context);
        let mut payload_to_publish = msg.payload.0.clone();
        if let Some(obj) = payload_to_publish.as_object_mut() {
            obj.insert(
                "trace_context".to_string(),
                serde_json::to_value(publish_trace_context)
                    .context("serialize trace context for outgoing message failed")?,
            );
        }
        let payload_bytes =
            serde_json::to_vec(&payload_to_publish).context("serialize outbox payload failed")?;

        rabbit
            .publish(&routing_key, &payload_bytes)
            .await
            .with_context(|| {
                format!(
                    "publish failed routing_key={routing_key} outbox_id={}",
                    msg.id
                )
            })?;

        let mut tx = pool.begin().await.context("begin tx failed")?;
        infra::outbox::mark_sent(&mut *tx, msg.id)
            .await
            .context("mark_sent failed")?;
        infra::jobs::mark_job_queued(&mut *tx, msg.job_id)
            .await
            .context("mark_job_queued failed")?;
        tx.commit().await.context("commit failed")?;

        info!(
            correlation_id = %correlation_id,
            outbox_id = %msg.id,
            job_id = %msg.job_id,
            %routing_key,
            "published outbox -> rabbit"
        );
    }

    Ok(processed_count)
}

/// Outbox Publisher:
/// - claim_batch from outbox
/// - publish to Rabbit
/// - mark_sent + job_queued in a tx
pub async fn publisher_loop(pool: Pool, rabbit: Arc<mq::Rabbit>) -> anyhow::Result<()> {
    let publisher_id = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "publisher".into());

    loop {
        refresh_outbox_pending_gauge(&pool).await;
        let processed =
            publish_outbox_once(&pool, rabbit.as_ref(), &publisher_id, 50_i64, 30).await?;
        refresh_outbox_pending_gauge(&pool).await;
        if processed == 0 {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
}

pub async fn process_job_message(
    pool: &Pool,
    rabbit: &mq::Rabbit,
    bytes: &[u8],
    max_attempts: i32,
) -> anyhow::Result<ConsumeAction> {
    let v: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(err) => {
            common::metrics::inc_dlq_messages();
            warn!(
                error = %err,
                payload_len = bytes.len(),
                "poison message detected: invalid JSON payload"
            );
            return Ok(ConsumeAction::DeadLetter);
        }
    };
    let correlation_id = v
        .get("correlation_id")
        .and_then(|x| x.as_str())
        .unwrap_or("unknown")
        .to_string();
    let trace_context = extract_trace_context(&v);

    let Some(job_id_raw) = v.get("job_id").and_then(|x| x.as_str()) else {
        common::metrics::inc_dlq_messages();
        warn!(
            correlation_id = %correlation_id,
            "poison message detected: missing job_id"
        );
        return Ok(ConsumeAction::DeadLetter);
    };

    let job_id = match Uuid::parse_str(job_id_raw) {
        Ok(v) => v,
        Err(err) => {
            common::metrics::inc_dlq_messages();
            warn!(
                correlation_id = %correlation_id,
                error = %err,
                "poison message detected: malformed job_id"
            );
            return Ok(ConsumeAction::DeadLetter);
        }
    };
    let process_span = info_span!(
        "consume_job_message",
        correlation_id = %correlation_id,
        job_id = %job_id
    );
    common::observability::set_parent_from_carrier(&process_span, &trace_context);
    let _process_span = process_span.enter();

    let mut tx = pool.begin().await.context("begin tx failed")?;
    let can_process = infra::jobs::try_mark_processing(&mut *tx, job_id)
        .await
        .context("try_mark_processing failed")?;
    tx.commit().await.context("commit failed")?;

    if !can_process {
        let current_status = infra::jobs::get_job(pool, job_id)
            .await
            .context("failed to load job when processing gate blocked")?
            .map(|j| j.status);

        if matches!(current_status.as_deref(), Some("validated" | "queued")) {
            // Publisher may have published before committing mark_job_queued.
            // Push to retry queue so the message is retried after a short TTL.
            let mut retry_trace_context = trace_context.clone();
            common::observability::inject_current_context(&mut retry_trace_context);
            let retry_payload = json!({
                "job_id": job_id.to_string(),
                "correlation_id": correlation_id,
                "trace_context": retry_trace_context,
            });
            let bytes = serde_json::to_vec(&retry_payload)
                .context("serialize processing-gate retry failed")?;
            rabbit
                .publish("job.retry", &bytes)
                .await
                .context("publish processing-gate retry failed")?;
            common::metrics::inc_job_retries();
            return Ok(ConsumeAction::Retry);
        }

        return Ok(ConsumeAction::Ack);
    }

    let processing_started_at = std::time::Instant::now();

    let failure_ctx = FailureContext {
        pool,
        rabbit,
        job_id,
        correlation_id: &correlation_id,
        trace_context: &trace_context,
        max_attempts,
        processing_started_at,
    };

    let input = match infra::jobs::get_job_processing_input(pool, job_id)
        .await
        .context("load processing payload from db failed")?
    {
        Some(v) => v,
        None => {
            common::metrics::inc_dlq_messages();
            warn!(
                correlation_id = %correlation_id,
                %job_id,
                "poison message detected: job has no processing payload"
            );
            return Ok(ConsumeAction::DeadLetter);
        }
    };

    let payload: Value = match serde_json::from_slice(&input.raw_body) {
        Ok(v) => v,
        Err(err) => {
            let e = anyhow::anyhow!("invalid inbox raw_body json: {err}");
            return handle_failure(&failure_ctx, e, FailureMode::Permanent).await;
        }
    };

    let failure_mode = tokio::time::timeout(Duration::from_secs(5), async {
        if payload
            .get("force_permanent_fail")
            .and_then(|x| x.as_bool())
            .unwrap_or(false)
        {
            anyhow::bail!("permanent failure requested by payload");
        }

        let fail_until_attempt = payload
            .get("fail_until_attempt")
            .and_then(|x| x.as_i64())
            .unwrap_or(0)
            .max(0) as i32;

        if input.attempts <= fail_until_attempt {
            anyhow::bail!(
                "transient failure requested: attempt={} fail_until_attempt={}",
                input.attempts,
                fail_until_attempt
            );
        }

        Ok::<(), anyhow::Error>(())
    })
    .await;

    match failure_mode {
        Ok(Ok(())) => {
            let mut tx = pool.begin().await.context("begin tx failed")?;
            infra::jobs::mark_succeeded(&mut *tx, job_id)
                .await
                .context("mark_succeeded failed")?;
            tx.commit().await.context("commit failed")?;
            common::metrics::inc_jobs_processed("succeeded");
            common::metrics::observe_job_processing_duration(
                "succeeded",
                processing_started_at.elapsed(),
            );
            Ok(ConsumeAction::Ack)
        }
        Ok(Err(e)) => {
            let mode = if payload
                .get("force_permanent_fail")
                .and_then(|x| x.as_bool())
                .unwrap_or(false)
            {
                FailureMode::Permanent
            } else {
                FailureMode::Transient
            };
            handle_failure(&failure_ctx, e, mode).await
        }
        Err(_) => {
            let e = anyhow::anyhow!("processing timeout");
            handle_failure(&failure_ctx, e, FailureMode::Transient).await
        }
    }
}

/// Consumer
pub async fn consumer_loop(
    pool: Pool,
    rabbit: Arc<mq::Rabbit>,
    max_attempts: i32,
    max_concurrency: usize,
) -> anyhow::Result<()> {
    let consumer_tag = "relayforge-consumer";

    let pool2 = pool.clone();
    let rabbit2 = rabbit.clone();

    rabbit
        .consume_loop(consumer_tag, max_concurrency, move |bytes| {
            let pool = pool2.clone();
            let rabbit = rabbit2.clone();
            async move { process_job_message(&pool, rabbit.as_ref(), &bytes, max_attempts).await }
        })
        .await?;

    Ok(())
}

async fn handle_failure(
    ctx: &FailureContext<'_>,
    err: anyhow::Error,
    mode: FailureMode,
) -> anyhow::Result<ConsumeAction> {
    let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM jobs WHERE id = $1")
        .bind(ctx.job_id)
        .fetch_one(ctx.pool)
        .await
        .context("read attempts failed")?;

    let err_str = err.to_string();
    let should_dead_letter = matches!(mode, FailureMode::Permanent) || attempts >= ctx.max_attempts;

    if should_dead_letter {
        let mut tx = ctx.pool.begin().await.context("begin tx failed")?;
        infra::jobs::mark_dead_lettered(&mut *tx, ctx.job_id, &err_str)
            .await
            .context("mark_dead_lettered failed")?;
        tx.commit().await.context("commit failed")?;

        common::metrics::inc_jobs_processed("dead_lettered");
        common::metrics::observe_job_processing_duration(
            "dead_lettered",
            ctx.processing_started_at.elapsed(),
        );
        common::metrics::inc_dlq_messages();

        warn!(
            correlation_id = %ctx.correlation_id,
            job_id = %ctx.job_id,
            attempts,
            %err_str,
            "job dead-lettered"
        );
        return Ok(ConsumeAction::DeadLetter);
    }

    let mut retry_trace_context = ctx.trace_context.clone();
    common::observability::inject_current_context(&mut retry_trace_context);

    let retry_payload = json!({
        "job_id": ctx.job_id.to_string(),
        "correlation_id": ctx.correlation_id,
        "trace_context": retry_trace_context
    });
    let bytes = serde_json::to_vec(&retry_payload).context("serialize retry payload failed")?;
    ctx.rabbit
        .publish("job.retry", &bytes)
        .await
        .context("publish retry failed")?;

    let mut tx = ctx.pool.begin().await.context("begin tx failed")?;
    infra::jobs::mark_failed_and_requeue(&mut *tx, ctx.job_id, &err_str)
        .await
        .context("mark_failed_and_requeue failed")?;
    tx.commit().await.context("commit failed")?;

    common::metrics::inc_job_retries();
    common::metrics::inc_jobs_processed("retry");
    common::metrics::observe_job_processing_duration("retry", ctx.processing_started_at.elapsed());

    warn!(
        correlation_id = %ctx.correlation_id,
        job_id = %ctx.job_id,
        attempts,
        %err_str,
        "job failed -> retry scheduled"
    );
    Ok(ConsumeAction::Retry)
}
