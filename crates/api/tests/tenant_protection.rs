use anyhow::Context;
use api::{
    admin_auth::AdminAccessControl, build_app, connectors::Provider, hardening::IngressRateLimiter,
    tenant_limits::TenantRateLimiter, tenant_secrets::TenantSecretStore, AppState,
};
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::{Client, StatusCode};
use serde_json::json;
use sha2::Sha256;
use sqlx::{postgres::PgPoolOptions, Pool, Postgres};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use testcontainers::{clients::Cli, core::WaitFor, GenericImage};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stripe_anti_replay_rejects_stale_and_replayed_events() -> anyhow::Result<()> {
    let docker = Cli::default();
    let pg = docker.run(
        GenericImage::new("postgres", "16-alpine")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_DB", "relayforge_api_test")
            .with_wait_for(WaitFor::seconds(3)),
    );
    let pg_port = pg.get_host_port_ipv4(5432);
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/relayforge_api_test",
        pg_port
    );

    let pool = wait_for_db_pool(&database_url, Duration::from_secs(40)).await?;
    infra::db::run_migrations(&pool).await?;

    let (server_task, shutdown_tx, base_url) =
        spawn_api(pool, TenantRateLimiter::new(50_000, 50_000), 1_000_000, 300).await?;
    let client = Client::builder().timeout(Duration::from_secs(5)).build()?;

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();

    let stale_ts = Utc::now().timestamp() - 3600;
    let stale_payload = json!({
        "id":"evt_stale_1",
        "type":"payment_intent.succeeded",
        "created":stale_ts,
        "data":{"object":{"amount":1200}}
    });
    let stale = post_stripe(
        &client,
        &base_url,
        tenant_a,
        stale_ts,
        &stale_payload,
        "dev-stripe-secret",
    )
    .await?;
    ensure_status(stale, StatusCode::UNAUTHORIZED).await?;

    let fresh_ts = Utc::now().timestamp();
    let replay_payload = json!({
        "id":"evt_replay_1",
        "type":"payment_intent.succeeded",
        "created":fresh_ts,
        "data":{"object":{"amount":1300}}
    });

    let first = post_stripe(
        &client,
        &base_url,
        tenant_a,
        fresh_ts,
        &replay_payload,
        "dev-stripe-secret",
    )
    .await?;
    ensure_status(first, StatusCode::ACCEPTED).await?;

    let replay = post_stripe(
        &client,
        &base_url,
        tenant_a,
        fresh_ts,
        &replay_payload,
        "dev-stripe-secret",
    )
    .await?;
    ensure_status(replay, StatusCode::CONFLICT).await?;

    let other_tenant = post_stripe(
        &client,
        &base_url,
        tenant_b,
        fresh_ts,
        &replay_payload,
        "dev-stripe-secret",
    )
    .await?;
    ensure_status(other_tenant, StatusCode::ACCEPTED).await?;

    cleanup(server_task, shutdown_tx).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tenant_daily_quota_is_enforced_per_tenant() -> anyhow::Result<()> {
    let docker = Cli::default();
    let pg = docker.run(
        GenericImage::new("postgres", "16-alpine")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_DB", "relayforge_api_test")
            .with_wait_for(WaitFor::seconds(3)),
    );
    let pg_port = pg.get_host_port_ipv4(5432);
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/relayforge_api_test",
        pg_port
    );

    let pool = wait_for_db_pool(&database_url, Duration::from_secs(40)).await?;
    infra::db::run_migrations(&pool).await?;

    let (server_task, shutdown_tx, base_url) =
        spawn_api(pool, TenantRateLimiter::new(50_000, 50_000), 1, 300).await?;
    let client = Client::builder().timeout(Duration::from_secs(5)).build()?;

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();

    let first_a = post_generic(
        &client,
        &base_url,
        tenant_a,
        "evt-quota-a-1",
        &json!({"event":"quota.a.1"}),
        "dev-generic-secret",
    )
    .await?;
    ensure_status(first_a, StatusCode::ACCEPTED).await?;

    let second_a = post_generic(
        &client,
        &base_url,
        tenant_a,
        "evt-quota-a-2",
        &json!({"event":"quota.a.2"}),
        "dev-generic-secret",
    )
    .await?;
    ensure_status(second_a, StatusCode::TOO_MANY_REQUESTS).await?;

    let first_b = post_generic(
        &client,
        &base_url,
        tenant_b,
        "evt-quota-b-1",
        &json!({"event":"quota.b.1"}),
        "dev-generic-secret",
    )
    .await?;
    ensure_status(first_b, StatusCode::ACCEPTED).await?;

    cleanup(server_task, shutdown_tx).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tenant_rate_limit_is_isolated_per_tenant() -> anyhow::Result<()> {
    let docker = Cli::default();
    let pg = docker.run(
        GenericImage::new("postgres", "16-alpine")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_DB", "relayforge_api_test")
            .with_wait_for(WaitFor::seconds(3)),
    );
    let pg_port = pg.get_host_port_ipv4(5432);
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/relayforge_api_test",
        pg_port
    );

    let pool = wait_for_db_pool(&database_url, Duration::from_secs(40)).await?;
    infra::db::run_migrations(&pool).await?;

    let (server_task, shutdown_tx, base_url) =
        spawn_api(pool, TenantRateLimiter::new(1, 1), 1_000_000, 300).await?;
    let client = Client::builder().timeout(Duration::from_secs(5)).build()?;

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();

    let first_a = post_generic(
        &client,
        &base_url,
        tenant_a,
        "evt-rate-a-1",
        &json!({"event":"rate.a.1"}),
        "dev-generic-secret",
    )
    .await?;
    ensure_status(first_a, StatusCode::ACCEPTED).await?;

    let second_a = post_generic(
        &client,
        &base_url,
        tenant_a,
        "evt-rate-a-2",
        &json!({"event":"rate.a.2"}),
        "dev-generic-secret",
    )
    .await?;
    ensure_status(second_a, StatusCode::TOO_MANY_REQUESTS).await?;

    let first_b = post_generic(
        &client,
        &base_url,
        tenant_b,
        "evt-rate-b-1",
        &json!({"event":"rate.b.1"}),
        "dev-generic-secret",
    )
    .await?;
    ensure_status(first_b, StatusCode::ACCEPTED).await?;

    cleanup(server_task, shutdown_tx).await;
    Ok(())
}

async fn spawn_api(
    pool: Pool<Postgres>,
    tenant_rate_limiter: TenantRateLimiter,
    tenant_daily_quota: i64,
    stripe_signature_tolerance_seconds: i64,
) -> anyhow::Result<(JoinHandle<()>, oneshot::Sender<()>, String)> {
    let mut fallback_secrets = HashMap::new();
    fallback_secrets.insert(Provider::Generic, "dev-generic-secret".to_string());
    fallback_secrets.insert(Provider::Github, "dev-github-secret".to_string());
    fallback_secrets.insert(Provider::Stripe, "dev-stripe-secret".to_string());

    let state = AppState {
        started_at: Instant::now(),
        pool,
        default_tenant_id: Uuid::nil(),
        connector_registry: api::connectors::ConnectorRegistry::default(),
        tenant_secret_store: TenantSecretStore::from_entries(fallback_secrets, vec![])
            .expect("tenant secret store should build"),
        admin_access_control: AdminAccessControl::from_json(None)?,
        tenant_rate_limiter,
        tenant_daily_quota,
        stripe_signature_tolerance_seconds,
        ingress_rate_limiter: IngressRateLimiter::new(50_000, 50_000),
        max_body_bytes: 256 * 1024,
    };

    let app = build_app(state);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind api listener")?;
    let addr = listener.local_addr()?;
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

async fn wait_for_db_pool(url: &str, timeout: Duration) -> anyhow::Result<Pool<Postgres>> {
    let started = Instant::now();
    loop {
        match PgPoolOptions::new().max_connections(8).connect(url).await {
            Ok(pool) => return Ok(pool),
            Err(err) => {
                if started.elapsed() >= timeout {
                    anyhow::bail!("timed out waiting for postgres: {err}");
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn post_generic(
    client: &Client,
    base_url: &str,
    tenant_id: Uuid,
    event_id: &str,
    payload: &serde_json::Value,
    secret: &str,
) -> anyhow::Result<reqwest::Response> {
    let body = serde_json::to_vec(payload)?;
    let signature = sign_hex(secret, &body)?;

    client
        .post(format!("{}/webhooks/generic", base_url))
        .header("content-type", "application/json")
        .header("x-tenant-id", tenant_id.to_string())
        .header("x-event-id", event_id)
        .header("x-signature", signature)
        .body(body)
        .send()
        .await
        .context("generic webhook request failed")
}

async fn post_stripe(
    client: &Client,
    base_url: &str,
    tenant_id: Uuid,
    timestamp: i64,
    payload: &serde_json::Value,
    secret: &str,
) -> anyhow::Result<reqwest::Response> {
    let body = serde_json::to_vec(payload)?;
    let body_str =
        std::str::from_utf8(&body).context("stripe payload must be valid UTF-8 for signing")?;
    let signed_payload = format!("{timestamp}.{body_str}");
    let signature = sign_hex(secret, signed_payload.as_bytes())?;

    client
        .post(format!("{}/webhooks/stripe", base_url))
        .header("content-type", "application/json")
        .header("x-tenant-id", tenant_id.to_string())
        .header("stripe-signature", format!("t={timestamp},v1={signature}"))
        .body(body)
        .send()
        .await
        .context("stripe webhook request failed")
}

fn sign_hex(secret: &str, body: &[u8]) -> anyhow::Result<String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .context("failed to initialize hmac signer")?;
    mac.update(body);
    Ok(hex::encode(mac.finalize().into_bytes()))
}

async fn ensure_status(response: reqwest::Response, expected: StatusCode) -> anyhow::Result<()> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status == expected {
        return Ok(());
    }

    anyhow::bail!(
        "expected status {} but got {} body={}",
        expected,
        status,
        body
    );
}

async fn cleanup(server_task: JoinHandle<()>, shutdown_tx: oneshot::Sender<()>) {
    let _ = shutdown_tx.send(());
    let _ = server_task.await;
}
