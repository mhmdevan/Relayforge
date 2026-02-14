use api::{
    admin_auth::AdminAccessControl, connectors::Provider, hardening::IngressRateLimiter,
    tenant_limits::TenantRateLimiter, tenant_secrets::TenantSecretStore, AppState,
};
use reqwest::Client;
use sqlx::postgres::PgPoolOptions;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn swagger_endpoints_are_served() -> anyhow::Result<()> {
    let state = test_state()?;
    let app = api::build_app(state);

    let (server_task, shutdown_tx, base_url) = spawn_app(app).await?;
    let client = Client::builder().timeout(Duration::from_secs(5)).build()?;

    let openapi = client
        .get(format!("{}/openapi.json", base_url))
        .send()
        .await?;
    assert_eq!(openapi.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = openapi.json().await?;
    assert_eq!(body["openapi"], "3.0.3");
    assert!(body["paths"]["/webhooks/{source}"]["post"].is_object());

    let docs = client.get(format!("{}/docs/", base_url)).send().await?;
    assert_eq!(docs.status(), reqwest::StatusCode::OK);
    let html = docs.text().await?;
    assert!(html.contains("SwaggerUIBundle"));
    assert!(html.contains("/openapi.json"));

    cleanup(server_task, shutdown_tx).await;
    Ok(())
}

fn test_state() -> anyhow::Result<AppState> {
    let pool =
        PgPoolOptions::new().connect_lazy("postgres://app:app@127.0.0.1:5432/event_gateway")?;

    let mut fallback_secrets = HashMap::new();
    fallback_secrets.insert(Provider::Generic, "dev-generic-secret".to_string());
    fallback_secrets.insert(Provider::Github, "dev-github-secret".to_string());
    fallback_secrets.insert(Provider::Stripe, "dev-stripe-secret".to_string());

    Ok(AppState {
        started_at: Instant::now(),
        pool,
        default_tenant_id: Uuid::nil(),
        connector_registry: api::connectors::ConnectorRegistry::default(),
        tenant_secret_store: TenantSecretStore::from_entries(fallback_secrets, vec![])
            .expect("tenant secret store should build"),
        admin_access_control: AdminAccessControl::from_json(None)?,
        tenant_rate_limiter: TenantRateLimiter::new(50_000, 50_000),
        tenant_daily_quota: 1_000_000,
        stripe_signature_tolerance_seconds: 300,
        ingress_rate_limiter: IngressRateLimiter::new(50_000, 50_000),
        max_body_bytes: 1024 * 1024,
    })
}

async fn spawn_app(
    app: axum::Router,
) -> anyhow::Result<(JoinHandle<()>, oneshot::Sender<()>, String)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    Ok((task, shutdown_tx, format!("http://{}", addr)))
}

async fn cleanup(server_task: JoinHandle<()>, shutdown_tx: oneshot::Sender<()>) {
    let _ = shutdown_tx.send(());
    let _ = server_task.await;
}
