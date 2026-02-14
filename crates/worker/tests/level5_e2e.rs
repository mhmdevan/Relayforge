use anyhow::Context;
use api::{build_app, AppState};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::{postgres::PgPoolOptions, Pool, Postgres};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use testcontainers::{clients::Cli, core::WaitFor, GenericImage};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use uuid::Uuid;
use worker::{
    consumer_loop,
    mq::{Rabbit, RabbitConfig},
    publisher_loop,
};

type HmacSha256 = Hmac<Sha256>;

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn level5_e2e_webhook_retry_dead_letter_and_reprocess() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt::try_init();

    let docker = Cli::default();

    let pg = docker.run(
        GenericImage::new("postgres", "16-alpine")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_DB", "relayforge_test")
            .with_wait_for(WaitFor::seconds(3)),
    );

    let rabbit = docker.run(
        GenericImage::new("rabbitmq", "3.13-management-alpine")
            .with_env_var("RABBITMQ_DEFAULT_USER", "relay")
            .with_env_var("RABBITMQ_DEFAULT_PASS", "relay")
            .with_wait_for(WaitFor::seconds(15)),
    );

    let pg_port = pg.get_host_port_ipv4(5432);
    let rabbit_port = rabbit.get_host_port_ipv4(5672);

    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/relayforge_test",
        pg_port
    );
    let amqp_addr = format!("amqp://relay:relay@127.0.0.1:{}/%2f", rabbit_port);

    let pool = wait_for_db_pool(&database_url, Duration::from_secs(40)).await?;
    infra::db::run_migrations(&pool).await?;

    let queue_suffix = Uuid::new_v4().simple().to_string();
    let rabbit_cfg = RabbitConfig {
        addr: amqp_addr,
        exchange: format!("relayforge.events.{}", queue_suffix),
        dlx: format!("relayforge.dlx.{}", queue_suffix),
        queue: format!("relayforge.jobs.{}", queue_suffix),
        retry_queue: format!("relayforge.jobs.retry.{}", queue_suffix),
        dlq: format!("relayforge.jobs.dlq.{}", queue_suffix),
        prefetch: 20,
        retry_ttl_ms: 300,
    };

    let rabbit_publisher =
        connect_publisher_with_retry(rabbit_cfg.clone(), Duration::from_secs(90)).await?;
    let rabbit_consumer = connect_consumer_with_retry(rabbit_cfg, Duration::from_secs(90)).await?;

    let (api_server, api_shutdown, base_url) = spawn_api(pool.clone()).await?;
    let publisher_task = tokio::spawn(publisher_loop(pool.clone(), rabbit_publisher));
    let consumer_task = tokio::spawn(consumer_loop(pool.clone(), rabbit_consumer, 3, 16));

    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("failed to build reqwest client")?;

    let dedup_body = json!({ "event": "ok" });
    let first = post_generic_webhook(&client, &base_url, "evt-dedup-1", &dedup_body).await?;
    let first_job_id = parse_job_id(&first)?;
    assert_eq!(first["job_created"], json!(true));
    assert_eq!(first["deduped"], json!(false));

    let second = post_generic_webhook(&client, &base_url, "evt-dedup-1", &dedup_body).await?;
    let second_job_id = parse_job_id(&second)?;
    assert_eq!(second["job_created"], json!(false));
    assert_eq!(second["deduped"], json!(true));
    assert_eq!(first_job_id, second_job_id);

    wait_for_job_status(&pool, first_job_id, "succeeded", Duration::from_secs(20)).await?;
    let dedup_job = infra::jobs::get_job(&pool, first_job_id)
        .await?
        .context("dedup job missing")?;
    assert_eq!(dedup_job.attempts, 1);

    let dedup_outbox_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outbox_messages WHERE job_id = $1 AND topic = 'job.created'",
    )
    .bind(first_job_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(dedup_outbox_count, 1);

    let retry_body = json!({ "event": "retry-then-succeed", "fail_until_attempt": 2 });
    let retry_resp = post_generic_webhook(&client, &base_url, "evt-retry-1", &retry_body).await?;
    let retry_job_id = parse_job_id(&retry_resp)?;

    wait_for_job_status(&pool, retry_job_id, "succeeded", Duration::from_secs(25)).await?;
    let retry_job = infra::jobs::get_job(&pool, retry_job_id)
        .await?
        .context("retry job missing")?;
    assert_eq!(retry_job.attempts, 3);
    assert!(retry_job.last_error.is_none());

    let dlq_body = json!({ "event": "dead-letter", "fail_until_attempt": 99 });
    let dlq_resp = post_generic_webhook(&client, &base_url, "evt-dlq-1", &dlq_body).await?;
    let dlq_job_id = parse_job_id(&dlq_resp)?;

    wait_for_job_status(&pool, dlq_job_id, "dead_lettered", Duration::from_secs(25)).await?;
    let dlq_job = infra::jobs::get_job(&pool, dlq_job_id)
        .await?
        .context("dlq job missing")?;
    assert_eq!(dlq_job.attempts, 3);
    assert!(dlq_job.last_error.is_some());

    let reprocess_resp = client
        .post(format!("{}/admin/jobs/{}/reprocess", base_url, dlq_job_id))
        .bearer_auth("operator-key")
        .send()
        .await
        .context("reprocess request failed")?;
    assert_eq!(reprocess_resp.status(), reqwest::StatusCode::ACCEPTED);
    let reprocess_json: Value = reprocess_resp.json().await?;
    assert_eq!(reprocess_json["reprocessed"], json!(true));

    wait_for_job_status(&pool, dlq_job_id, "dead_lettered", Duration::from_secs(15)).await?;
    let dlq_after_reprocess = infra::jobs::get_job(&pool, dlq_job_id)
        .await?
        .context("dlq job missing after reprocess")?;
    assert_eq!(dlq_after_reprocess.attempts, 4);

    let succeeded_reprocess_resp = client
        .post(format!(
            "{}/admin/jobs/{}/reprocess",
            base_url, first_job_id
        ))
        .bearer_auth("operator-key")
        .send()
        .await
        .context("reprocess request for succeeded job failed")?;
    assert_eq!(
        succeeded_reprocess_resp.status(),
        reqwest::StatusCode::CONFLICT
    );

    cleanup(api_shutdown, api_server, publisher_task, consumer_task).await;

    Ok(())
}

async fn spawn_api(
    pool: Pool<Postgres>,
) -> anyhow::Result<(JoinHandle<()>, oneshot::Sender<()>, String)> {
    let state = AppState {
        started_at: Instant::now(),
        pool,
        default_tenant_id: Uuid::nil(),
        connector_registry: api::connectors::ConnectorRegistry::default(),
        tenant_secret_store: api::tenant_secrets::TenantSecretStore::from_entries(
            {
                let mut m = std::collections::HashMap::new();
                m.insert(
                    api::connectors::Provider::Github,
                    "dev-github-secret".to_string(),
                );
                m.insert(
                    api::connectors::Provider::Generic,
                    "dev-generic-secret".to_string(),
                );
                m.insert(
                    api::connectors::Provider::Stripe,
                    "dev-stripe-secret".to_string(),
                );
                m
            },
            vec![],
        )
        .expect("tenant secret store should build"),
        admin_access_control: api::admin_auth::AdminAccessControl::from_json(Some(
            r#"[{"key":"operator-key","role":"operator"}]"#,
        ))
        .expect("admin access control should build"),
        tenant_rate_limiter: api::tenant_limits::TenantRateLimiter::new(20_000, 20_000),
        tenant_daily_quota: 1_000_000,
        stripe_signature_tolerance_seconds: 300,
        ingress_rate_limiter: api::hardening::IngressRateLimiter::new(20_000, 20_000),
        max_body_bytes: 256 * 1024,
    };
    let app = build_app(state);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind api listener")?;
    let addr = listener
        .local_addr()
        .context("failed to get api local addr")?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    Ok((server, shutdown_tx, format!("http://{}", addr)))
}

async fn cleanup(
    api_shutdown: oneshot::Sender<()>,
    api_server: JoinHandle<()>,
    publisher_task: JoinHandle<anyhow::Result<()>>,
    consumer_task: JoinHandle<anyhow::Result<()>>,
) {
    let _ = api_shutdown.send(());
    let _ = api_server.await;

    publisher_task.abort();
    consumer_task.abort();

    let _ = publisher_task.await;
    let _ = consumer_task.await;
}

async fn wait_for_db_pool(url: &str, timeout: Duration) -> anyhow::Result<Pool<Postgres>> {
    let started = Instant::now();

    loop {
        match PgPoolOptions::new().max_connections(8).connect(url).await {
            Ok(pool) => return Ok(pool),
            Err(e) => {
                if started.elapsed() >= timeout {
                    anyhow::bail!("timed out waiting for postgres: {}", e);
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

async fn connect_publisher_with_retry(
    cfg: RabbitConfig,
    timeout: Duration,
) -> anyhow::Result<Arc<Rabbit>> {
    connect_rabbit_with_retry(cfg, timeout, false).await
}

async fn connect_consumer_with_retry(
    cfg: RabbitConfig,
    timeout: Duration,
) -> anyhow::Result<Arc<Rabbit>> {
    connect_rabbit_with_retry(cfg, timeout, true).await
}

async fn connect_rabbit_with_retry(
    cfg: RabbitConfig,
    timeout: Duration,
    consumer: bool,
) -> anyhow::Result<Arc<Rabbit>> {
    let started = Instant::now();

    loop {
        let res = if consumer {
            Rabbit::connect_consumer(cfg.clone()).await
        } else {
            Rabbit::connect_publisher(cfg.clone()).await
        };

        match res {
            Ok(r) => return Ok(Arc::new(r)),
            Err(e) => {
                if started.elapsed() >= timeout {
                    anyhow::bail!("timed out waiting for rabbitmq: {}", e);
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

async fn post_generic_webhook(
    client: &Client,
    base_url: &str,
    event_id: &str,
    payload: &Value,
) -> anyhow::Result<Value> {
    let body = serde_json::to_vec(payload)?;
    let signature = sign_generic("dev-generic-secret", &body)?;

    let response = client
        .post(format!("{}/webhooks/generic", base_url))
        .header("content-type", "application/json")
        .header("x-event-id", event_id)
        .header("x-signature", signature)
        .body(body)
        .send()
        .await
        .context("webhook request failed")?;

    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    let json = response.json::<Value>().await?;
    Ok(json)
}

fn sign_generic(secret: &str, body: &[u8]) -> anyhow::Result<String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .context("failed to initialize hmac signer")?;
    mac.update(body);
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn parse_job_id(body: &Value) -> anyhow::Result<Uuid> {
    let raw = body
        .get("job_id")
        .and_then(|v| v.as_str())
        .context("job_id missing in webhook response")?;
    Uuid::parse_str(raw).context("job_id is not a valid uuid")
}

async fn wait_for_job_status(
    pool: &Pool<Postgres>,
    job_id: Uuid,
    expected_status: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    let started = Instant::now();

    loop {
        if let Some(job) = infra::jobs::get_job(pool, job_id).await? {
            if job.status == expected_status {
                return Ok(());
            }
        }

        if started.elapsed() >= timeout {
            let current = infra::jobs::get_job(pool, job_id)
                .await?
                .map(|j| format!("{} (attempts={})", j.status, j.attempts))
                .unwrap_or_else(|| "missing".to_string());
            anyhow::bail!(
                "timed out waiting job {} to become {} (current: {})",
                job_id,
                expected_status,
                current
            );
        }

        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}
