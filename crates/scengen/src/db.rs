//! PostgreSQL access for scengen. Small pool footprint — the scenario subsystem is
//! non-critical and must not starve the dashboard's connections.

use anyhow::{Context, Result};
use sqlx::postgres::{PgPool, PgPoolOptions};

pub async fn pool() -> Result<PgPool> {
    let url = std::env::var("DATABASE_URL").context("DATABASE_URL not set")?;
    PgPoolOptions::new()
        .max_connections(2) // deliberately small: non-critical subsystem
        .connect(&url)
        .await
        .context("connecting to PostgreSQL (scengen)")
}
