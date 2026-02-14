use api::{
    admin_auth::AdminAccessControl,
    build_app,
    connectors::Provider,
    tenant_limits::TenantRateLimiter,
    tenant_secrets::{TenantProviderSecret, TenantSecretStore},
    AppState,
};
use common::{config::load_settings, observability::init_tracing, shutdown::shutdown_signal};
use infra::db;
use std::{collections::HashMap, net::SocketAddr, time::Instant};
use tracing::{info, warn};
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let settings = load_settings()?;
    init_tracing("relayforge-api", &settings.log)?;

    let pool = db::create_pg_pool(&settings.database).await?;

    let default_tenant_id: Uuid = settings
        .tenant
        .default_id
        .parse()
        .map_err(|e| anyhow::anyhow!("TENANT__DEFAULT_ID is not a valid UUID: {e}"))?;

    let mut fallback_secrets = HashMap::new();
    fallback_secrets.insert(Provider::Github, settings.webhooks.github_secret.clone());
    fallback_secrets.insert(
        Provider::Generic,
        settings.webhooks.generic_hmac_secret.clone(),
    );
    fallback_secrets.insert(
        Provider::Stripe,
        settings.webhooks.stripe_endpoint_secret.clone(),
    );

    let db_secrets = infra::tenant_provider_secrets::list_active(&pool).await?;
    let mut tenant_entries = Vec::with_capacity(db_secrets.len());
    for row in db_secrets {
        let Some(provider) = Provider::from_source(&row.provider) else {
            warn!(provider = %row.provider, "ignoring unknown provider in tenant_provider_secrets");
            continue;
        };

        tenant_entries.push(TenantProviderSecret {
            tenant_id: row.tenant_id,
            provider,
            secret: row.secret,
        });
    }

    let tenant_secret_store = TenantSecretStore::from_entries(fallback_secrets, tenant_entries)?;
    let admin_access_control =
        AdminAccessControl::from_json(settings.admin.fallback_api_keys_json.as_deref())?;

    let state = AppState {
        started_at: Instant::now(),
        pool,
        default_tenant_id,
        connector_registry: api::connectors::ConnectorRegistry::default(),
        tenant_secret_store,
        admin_access_control,
        tenant_rate_limiter: TenantRateLimiter::new(
            settings.hardening.tenant_rate_limit_per_second,
            settings.hardening.tenant_rate_limit_burst,
        ),
        tenant_daily_quota: settings.hardening.tenant_daily_quota,
        stripe_signature_tolerance_seconds: settings.webhooks.stripe_signature_tolerance_seconds,
        ingress_rate_limiter: api::hardening::IngressRateLimiter::new(
            settings.hardening.ingress_rate_limit_per_second,
            settings.hardening.ingress_burst,
        ),
        max_body_bytes: settings.hardening.max_body_bytes,
    };

    let app = build_app(state);

    let addr: SocketAddr = format!("{}:{}", settings.server.host, settings.server.port)
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid server host/port: {}", e))?;

    info!("listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            shutdown_signal().await;
            warn!("shutdown signal received, stopping server...");
        })
        .await?;

    Ok(())
}
