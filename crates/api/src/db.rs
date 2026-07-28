//! Read-only PostgreSQL pool. This crate has NO Oracle access by construction.

use anyhow::{Context, Result};
use sqlx::postgres::{PgPool, PgPoolOptions};

pub async fn pool() -> Result<PgPool> {
    let url = std::env::var("DATABASE_URL").context("DATABASE_URL not set")?;
    PgPoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await
        .context("connecting to PostgreSQL")
}

/// Run a retention prune, surfacing failure instead of swallowing it.
///
/// These pruners are the only thing keeping every rolling table bounded, and they used to be
/// written `let _ = sqlx::query("DELETE ...").execute(&pool).await;` — which discards the error
/// completely. A prune could then fail every tick for weeks with nothing logged and nothing
/// alerting; the first symptom would be a downstream job reading a table far past its design
/// size. That is the shape of the 2026-07-28 OOM (an array sized by its input), so failure is
/// logged loudly here and the deleted count is available at debug level.
pub async fn prune(pool: &PgPool, table: &str, sql: &str) {
    match sqlx::query(sql).execute(pool).await {
        Ok(r) => tracing::debug!(table, deleted = r.rows_affected(), "retention prune"),
        Err(e) => {
            tracing::warn!(table, error = %e, "RETENTION PRUNE FAILED — table will grow unbounded")
        }
    }
}

/// Latest business date present in kpi_daily (used when ?as_of is omitted).
pub async fn latest_as_of(pool: &PgPool) -> Result<Option<chrono::NaiveDate>> {
    let row: Option<(chrono::NaiveDate,)> =
        sqlx::query_as("SELECT max(snapshot_date) FROM kpi_daily")
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|r| r.0))
}
