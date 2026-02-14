use anyhow::Context;
use common::config::DatabaseSettings;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;
use tracing::info;

pub type PgPool = sqlx::postgres::PgPool;

pub async fn create_pg_pool(db: &DatabaseSettings) -> anyhow::Result<PgPool> {
    let timeout = Duration::from_secs(db.connect_timeout_seconds);

    let pool = PgPoolOptions::new()
        .max_connections(db.max_connections)
        .acquire_timeout(timeout)
        .connect(&db.url)
        .await
        .context("failed to connect to Postgres")?;

    ping(&pool).await?;

    if db.run_migrations {
        run_migrations(&pool).await?;
    }

    info!("connected to Postgres");
    Ok(pool)
}

pub async fn ping(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::query("SELECT 1")
        .execute(pool)
        .await
        .context("failed to ping Postgres")?;
    Ok(())
}

pub async fn run_migrations(pool: &PgPool) -> anyhow::Result<()> {
    // NOTE: this path is relative to crates/infra
    sqlx::migrate!("../../migrations")
        .run(pool)
        .await
        .context("failed to run migrations")?;

    info!("migrations applied");
    Ok(())
}
