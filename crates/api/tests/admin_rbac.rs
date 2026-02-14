use api::{
    admin_auth::{require_operator_or_admin, AdminAccessControl},
    connectors::Provider,
    hardening::IngressRateLimiter,
    tenant_secrets::TenantSecretStore,
    AppState,
};
use axum::{http::StatusCode, middleware, response::IntoResponse, routing::post, Router};
use reqwest::Client;
use sqlx::postgres::PgPoolOptions;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_route_requires_bearer_token_and_role() -> anyhow::Result<()> {
    let state = test_state(
        r#"[
            {"key":"viewer-key","role":"viewer"},
            {"key":"operator-key","role":"operator"},
            {"key":"admin-key","role":"admin"}
        ]"#,
    )?;

    let app = Router::new()
        .route("/admin/protected", post(protected_handler))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_operator_or_admin,
        ))
        .with_state(state);

    let (server_task, shutdown_tx, base_url) = spawn_app(app).await?;
    let client = Client::builder().timeout(Duration::from_secs(5)).build()?;

    let missing = client
        .post(format!("{}/admin/protected", base_url))
        .send()
        .await?;
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

    let viewer = client
        .post(format!("{}/admin/protected", base_url))
        .bearer_auth("viewer-key")
        .send()
        .await?;
    assert_eq!(viewer.status(), StatusCode::FORBIDDEN);

    let operator = client
        .post(format!("{}/admin/protected", base_url))
        .bearer_auth("operator-key")
        .send()
        .await?;
    assert_eq!(operator.status(), StatusCode::OK);

    let admin = client
        .post(format!("{}/admin/protected", base_url))
        .bearer_auth("admin-key")
        .send()
        .await?;
    assert_eq!(admin.status(), StatusCode::OK);

    cleanup(server_task, shutdown_tx).await;
    Ok(())
}

fn test_state(fallback_admin_keys_json: &str) -> anyhow::Result<AppState> {
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
        admin_access_control: AdminAccessControl::from_json(Some(fallback_admin_keys_json))?,
        tenant_rate_limiter: api::tenant_limits::TenantRateLimiter::new(50_000, 50_000),
        tenant_daily_quota: 1_000_000,
        stripe_signature_tolerance_seconds: 300,
        ingress_rate_limiter: IngressRateLimiter::new(50_000, 50_000),
        max_body_bytes: 1024 * 1024,
    })
}

async fn protected_handler() -> impl IntoResponse {
    StatusCode::OK
}

async fn spawn_app(app: Router) -> anyhow::Result<(JoinHandle<()>, oneshot::Sender<()>, String)> {
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
