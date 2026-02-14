use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub server: ServerSettings,
    pub database: DatabaseSettings,
    pub webhooks: WebhookSettings,
    pub log: LogSettings,
    pub tenant: TenantSettings,
    pub admin: AdminSettings,
    pub hardening: HardeningSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseSettings {
    /// Example: postgres://app:app@127.0.0.1:5432/event_gateway
    pub url: String,
    pub max_connections: u32,
    pub connect_timeout_seconds: u64,
    /// Dev convenience: run migrations on startup (should be false in production)
    pub run_migrations: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebhookSettings {
    /// Secret used for GitHub-style signature:
    /// header: X-Hub-Signature-256 = "sha256=<hex>"
    pub github_secret: String,

    /// Generic HMAC secret:
    /// header: X-Signature = "<hex>"
    pub generic_hmac_secret: String,

    /// Stripe endpoint secret:
    /// header: Stripe-Signature = "t=<ts>,v1=<hex>"
    pub stripe_endpoint_secret: String,

    /// Maximum allowed clock skew for Stripe signature timestamp.
    pub stripe_signature_tolerance_seconds: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TenantSettings {
    /// If X-Tenant-Id header is missing, use this UUID as default (demo/dev).
    pub default_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdminSettings {
    /// Optional fallback API keys when DB key is not found.
    /// JSON array string example:
    /// [
    ///   {"key":"dev-admin-key","role":"admin"},
    ///   {"key":"dev-operator-key","role":"operator"}
    /// ]
    pub fallback_api_keys_json: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogSettings {
    pub filter: String,
    pub json: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HardeningSettings {
    pub ingress_rate_limit_per_second: u32,
    pub ingress_burst: u32,
    pub max_body_bytes: usize,
    pub tenant_rate_limit_per_second: u32,
    pub tenant_rate_limit_burst: u32,
    pub tenant_daily_quota: i64,
}

pub fn load_settings() -> anyhow::Result<Settings> {
    let cfg = config::Config::builder()
        // Defaults
        .set_default("server.host", "127.0.0.1")?
        .set_default("server.port", 8080)?
        .set_default("database.max_connections", 10)?
        .set_default("database.connect_timeout_seconds", 5)?
        .set_default("database.run_migrations", true)?
        .set_default("log.filter", "info")?
        .set_default("log.json", false)?
        // Dev defaults (DO NOT keep these in real prod)
        .set_default("webhooks.github_secret", "dev-github-secret")?
        .set_default("webhooks.generic_hmac_secret", "dev-generic-secret")?
        .set_default("webhooks.stripe_endpoint_secret", "dev-stripe-secret")?
        .set_default("webhooks.stripe_signature_tolerance_seconds", 300)?
        .set_default("tenant.default_id", "00000000-0000-0000-0000-000000000000")?
        .set_default("admin.fallback_api_keys_json", "")?
        .set_default("hardening.ingress_rate_limit_per_second", 200)?
        .set_default("hardening.ingress_burst", 400)?
        .set_default("hardening.max_body_bytes", 262144)?
        .set_default("hardening.tenant_rate_limit_per_second", 100)?
        .set_default("hardening.tenant_rate_limit_burst", 200)?
        .set_default("hardening.tenant_daily_quota", 100000)?
        // Env override (nested via __)
        .add_source(config::Environment::default().separator("__"))
        .build()
        .context("failed to build configuration")?;

    let settings = cfg
        .try_deserialize::<Settings>()
        .context("failed to deserialize configuration into Settings")?;

    validate_secret_settings(&settings)?;
    validate_limits(&settings)?;
    Ok(settings)
}

fn validate_secret_settings(settings: &Settings) -> anyhow::Result<()> {
    if settings.webhooks.github_secret.trim().is_empty() {
        anyhow::bail!("WEBHOOKS__GITHUB_SECRET must not be empty");
    }

    if settings.webhooks.generic_hmac_secret.trim().is_empty() {
        anyhow::bail!("WEBHOOKS__GENERIC_HMAC_SECRET must not be empty");
    }

    if settings.webhooks.stripe_endpoint_secret.trim().is_empty() {
        anyhow::bail!("WEBHOOKS__STRIPE_ENDPOINT_SECRET must not be empty");
    }

    let app_env = std::env::var("APP_ENV").unwrap_or_else(|_| "development".to_string());
    if app_env.eq_ignore_ascii_case("production")
        && (settings.webhooks.github_secret.starts_with("dev-")
            || settings.webhooks.generic_hmac_secret.starts_with("dev-")
            || settings.webhooks.stripe_endpoint_secret.starts_with("dev-"))
    {
        anyhow::bail!("refusing to boot in production with development webhook secrets");
    }

    Ok(())
}

fn validate_limits(settings: &Settings) -> anyhow::Result<()> {
    if settings.webhooks.stripe_signature_tolerance_seconds <= 0 {
        anyhow::bail!("WEBHOOKS__STRIPE_SIGNATURE_TOLERANCE_SECONDS must be > 0");
    }

    if settings.hardening.tenant_daily_quota <= 0 {
        anyhow::bail!("HARDENING__TENANT_DAILY_QUOTA must be > 0");
    }

    Ok(())
}
