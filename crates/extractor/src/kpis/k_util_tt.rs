//! K_UTIL (TT) extract: render e3a, run it, parse, upsert into raw_k_util_tt.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::{params, runner::Toolbox};

pub const KPI_KEY: &str = "K_UTIL_TT";
const SQL_TEMPLATE: &str = include_str!("../../sql/e3a_k_util_tt_merged.sql");

/// One output row of e3a (columns are UPPERCASE in the toolbox JSON).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct Row {
    pub machno: String,
    pub sessions_total: Option<i64>,
    pub interval_groups: Option<i64>,
    pub logout_anomaly: Option<i64>,
    pub active_min: Option<f64>,
    pub stop_min: Option<f64>,
    pub productive_min: Option<f64>,
    pub k_util_capped: Option<f64>,
    pub k_util_raw: Option<f64>,
}

/// Parse the toolbox stdout into typed rows.
pub fn parse(raw: &str) -> Result<Vec<Row>> {
    parse_rows(raw).context("parsing k_util_tt rows")
}

/// Idempotent upsert of one business day's rows into raw_k_util_tt.
pub async fn upsert(pool: &PgPool, date: NaiveDate, run_id: i64, rows: &[Row]) -> Result<u64> {
    let mut tx = pool.begin().await?;
    let mut n = 0u64;
    for r in rows {
        sqlx::query(
            "INSERT INTO raw_k_util_tt
               (snapshot_date, machno, sessions_total, interval_groups, logout_anomaly,
                active_min, stop_min, productive_min, k_util_capped, k_util_raw, run_id)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
             ON CONFLICT (snapshot_date, machno) DO UPDATE SET
               sessions_total = EXCLUDED.sessions_total,
               interval_groups= EXCLUDED.interval_groups,
               logout_anomaly = EXCLUDED.logout_anomaly,
               active_min     = EXCLUDED.active_min,
               stop_min       = EXCLUDED.stop_min,
               productive_min = EXCLUDED.productive_min,
               k_util_capped  = EXCLUDED.k_util_capped,
               k_util_raw     = EXCLUDED.k_util_raw,
               run_id         = EXCLUDED.run_id,
               extracted_at   = now()",
        )
        .bind(date)
        .bind(&r.machno)
        .bind(r.sessions_total.map(|v| v as i32))
        .bind(r.interval_groups.map(|v| v as i32))
        .bind(r.logout_anomaly.map(|v| v as i32))
        .bind(r.active_min)
        .bind(r.stop_min)
        .bind(r.productive_min)
        .bind(r.k_util_capped)
        .bind(r.k_util_raw)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .context("upserting raw_k_util_tt row")?;
        n += 1;
    }
    tx.commit().await?;
    Ok(n)
}

/// Full extract: render -> run on Oracle -> parse -> upsert, with run logging.
pub async fn extract(pool: &PgPool, date: NaiveDate, target: &str) -> Result<u64> {
    let sql = params::render_day(SQL_TEMPLATE, date)?;
    run_logged(pool, KPI_KEY, date, |run_id| async move {
        let raw = Toolbox::from_env(target)?.run_sql(&sql).await?;
        upsert(pool, date, run_id, &parse(&raw)?).await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_golden_fixture() {
        let raw = include_str!("../../tests/fixtures/k_util_tt.stdout.json");
        let rows = parse(raw).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].machno, "TT602");
        assert_eq!(rows[1].machno, "TT1281");
        assert_eq!(rows[1].logout_anomaly, Some(1)); // overlap/logout anomaly flagged
        assert_eq!(rows[1].k_util_capped, Some(1.0)); // capped
        assert!((rows[2].k_util_raw.unwrap() - 0.8790).abs() < 1e-9);
    }

    #[test]
    fn template_renders_for_date() {
        let date = NaiveDate::from_ymd_opt(2026, 6, 4).unwrap();
        let sql = params::render_day(SQL_TEMPLATE, date).unwrap();
        assert!(sql.contains("'20260604'"));
        assert!(!sql.contains("{{DAY_STR}}"));
        assert!(sql.contains("FETCH FIRST 50 ROWS ONLY"));
    }
}
