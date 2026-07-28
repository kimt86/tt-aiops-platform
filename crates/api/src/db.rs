//! Read-only PostgreSQL pool. This crate has NO Oracle access by construction.

use std::time::Duration;

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

/// Detection ceilings for the growth watchdog. These are NOT policy — they sit far above
/// anything normal, so crossing one means a write went wrong, not that we got busy.
/// Measured 2026-07-28: whole DB 5.7GB, largest table (truck_pos_hifreq) 830MB.
const DB_WARN_BYTES: i64 = 50 * 1024 * 1024 * 1024; // ~9x today
const TABLE_WARN_BYTES: i64 = 10 * 1024 * 1024 * 1024; // ~12x the largest table today
const SIZE_CHECK_EVERY: Duration = Duration::from_secs(1800);

/// Watchdog for the failure class a memory cap cannot contain: a write whose ROW COUNT is
/// decided by unvalidated input.
///
/// 2026-07-28 produced both twins of this class. The in-memory twin — an array sized by the
/// farthest GPS fix — OOM-killed the box, and `MemoryMax=` on every unit now bounds it. The DB
/// twin — `generate_series(1, tier)` with `tier` straight off Oracle — is not bounded by any of
/// that: the insert is executed by Postgres, which lives in a different cgroup, so a process
/// memory cap does nothing; the damage lands on disk and WAL; and it is merely large rather than
/// slow, so a statement timeout would not catch it either.
///
/// Physical bounds at the write site are the fix — MAX_TIER in code, CHECK constraints in the
/// schema (mig 0106) so backfills and manual SQL are bound too. This is the detector for the
/// site nobody has bounded yet. It cannot prevent the write; it makes the growth impossible to
/// miss, which is precisely what failed on 2026-07-28 — the road graph ran degraded for five
/// days and nothing said so.
pub fn spawn_size_watchdog(pool: PgPool) {
    tokio::spawn(async move {
        loop {
            match sqlx::query_as::<_, (i64,)>("SELECT pg_database_size(current_database())")
                .fetch_one(&pool)
                .await
            {
                Ok((n,)) if n > DB_WARN_BYTES => tracing::warn!(
                    gb = n / 1_073_741_824,
                    ceiling_gb = DB_WARN_BYTES / 1_073_741_824,
                    "DATABASE OVER SIZE CEILING — suspect an unbounded write"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "size watchdog: database size query failed"),
            }

            let big: Vec<(String, i64)> = sqlx::query_as(
                "SELECT schemaname || '.' || relname, pg_total_relation_size(relid)
                   FROM pg_stat_all_tables
                  WHERE schemaname NOT IN ('pg_catalog', 'information_schema')
                    AND pg_total_relation_size(relid) > $1
                  ORDER BY 2 DESC",
            )
            .bind(TABLE_WARN_BYTES)
            .fetch_all(&pool)
            .await
            .unwrap_or_default();
            for (t, n) in big {
                tracing::warn!(
                    table = %t,
                    gb = n / 1_073_741_824,
                    ceiling_gb = TABLE_WARN_BYTES / 1_073_741_824,
                    "TABLE OVER SIZE CEILING — suspect an unbounded write"
                );
            }

            tokio::time::sleep(SIZE_CHECK_EVERY).await;
        }
    });
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
