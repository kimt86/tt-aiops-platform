//! Shared run-logging wrapper for KPI extracts.

use anyhow::Result;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use sqlx::PgPool;

use crate::db;

/// `KPI_NIGHTLY_SRC=oracle` reverts nightly/day-mode KPI extracts to their Oracle
/// path unchanged; any other value (including unset) = local (PLAN-extractor.md
/// CHUNK6 6-3 kill switch, same convention as shift.rs's `KPI_T1_SRC`).
pub fn nightly_src_local() -> bool {
    std::env::var("KPI_NIGHTLY_SRC").map(|v| v != "oracle").unwrap_or(true)
}

/// MYT calendar-day bounds as UTC timestamptz, for local tables that carry no
/// `business_date` column (tos_handover_label) and so must be windowed on their
/// event timestamp instead.
pub fn day_bounds_utc(date: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let start = date.and_hms_opt(0, 0, 0).unwrap();
    let end = (date + Duration::days(1)).and_hms_opt(0, 0, 0).unwrap();
    (tt_core::shift::terminal_to_utc(start), tt_core::shift::terminal_to_utc(end))
}

/// Run `work` under an etl_run_log entry: insert RUNNING, then mark OK/FAILED and
/// update freshness based on the outcome. `work` receives the run_id and returns
/// the number of rows written.
pub async fn run_logged<F, Fut>(
    pool: &PgPool,
    kpi_key: &str,
    date: NaiveDate,
    work: F,
) -> Result<u64>
where
    F: FnOnce(i64) -> Fut,
    Fut: std::future::Future<Output = Result<u64>>,
{
    let run_id = db::start_run(pool, kpi_key, date).await?;
    match work(run_id).await {
        Ok(n) => {
            db::finish_run(pool, run_id, kpi_key, date, "OK", Some(n as i64), None).await?;
            tracing::info!(kpi = kpi_key, %date, rows = n, "extract OK");
            Ok(n)
        }
        Err(e) => {
            db::finish_run(pool, run_id, kpi_key, date, "FAILED", None, Some(&e.to_string()))
                .await?;
            Err(e)
        }
    }
}
