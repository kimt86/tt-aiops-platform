//! K_EMPTY / K_EMPTY_R extract: e4 -> raw_k_empty (per jobtype x shift).

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::{params, runner::Toolbox};

pub const KPI_KEY: &str = "K_EMPTY";
const SQL: &str = include_str!("../../sql/e4_k_empty_decomposition.sql");

// KPI_NIGHTLY_SRC kill switch (PLAN-extractor.md CHUNK7 7-2(d), same convention as CHUNK6
// 6-3): local Postgres (tos_handover_label) instead of the Oracle JOB_ORDER_HISTORY day scan.
// $1,$2 = the business day's terminal-local window, converted to UTC in extract() below.
const SQL_LOCAL: &str = include_str!("../../sql/local/l_empty.sql");

#[derive(Debug, Clone, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "UPPERCASE")]
pub struct Row {
    pub jobtype: String,
    pub shift: String,
    pub jobs: Option<f64>,
    pub k_empty_ratio: Option<f64>,
    pub avg_empty_m: Option<f64>,
    pub avg_laden_m: Option<f64>,
    pub total_empty_m: Option<f64>,
    pub total_laden_m: Option<f64>,
    pub distinct_blocks: Option<f64>,
}

pub fn parse(raw: &str) -> Result<Vec<Row>> {
    parse_rows(raw).context("parsing k_empty rows")
}

pub async fn upsert(pool: &PgPool, date: NaiveDate, run_id: i64, rows: &[Row]) -> Result<u64> {
    let mut tx = pool.begin().await?;
    for r in rows {
        sqlx::query(
            "INSERT INTO raw_k_empty
               (snapshot_date, jobtype, shift, jobs, k_empty_ratio, avg_empty_m, avg_laden_m,
                total_empty_m, total_laden_m, distinct_blocks, run_id)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
             ON CONFLICT (snapshot_date, jobtype, shift) DO UPDATE SET
               jobs=EXCLUDED.jobs, k_empty_ratio=EXCLUDED.k_empty_ratio,
               avg_empty_m=EXCLUDED.avg_empty_m, avg_laden_m=EXCLUDED.avg_laden_m,
               total_empty_m=EXCLUDED.total_empty_m, total_laden_m=EXCLUDED.total_laden_m,
               distinct_blocks=EXCLUDED.distinct_blocks, run_id=EXCLUDED.run_id, extracted_at=now()",
        )
        .bind(date)
        .bind(&r.jobtype)
        .bind(&r.shift)
        .bind(r.jobs)
        .bind(r.k_empty_ratio)
        .bind(r.avg_empty_m)
        .bind(r.avg_laden_m)
        .bind(r.total_empty_m)
        .bind(r.total_laden_m)
        .bind(r.distinct_blocks)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .context("upserting raw_k_empty")?;
    }
    tx.commit().await?;
    Ok(rows.len() as u64)
}

pub async fn extract(pool: &PgPool, date: NaiveDate, target: &str) -> Result<u64> {
    if crate::kpis::common::nightly_src_local() {
        // Full terminal-local business day -> UTC, same window shape the Oracle original
        // scans via JOB_HIST_DATE = date (params::render_day).
        let (ws, we) = crate::kpis::common::day_bounds_utc(date);
        return run_logged(pool, KPI_KEY, date, |run_id| async move {
            let rows: Vec<Row> = sqlx::query_as(SQL_LOCAL).bind(ws).bind(we).fetch_all(pool).await?;
            upsert(pool, date, run_id, &rows).await
        })
        .await;
    }
    let sql = params::render_day(SQL, date)?;
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
    fn parses_rows() {
        let raw = r#"{"result":"[{\"JOBTYPE\":\"MO\",\"SHIFT\":\"Night\",\"JOBS\":1200,\"K_EMPTY_RATIO\":0.566,\"AVG_EMPTY_M\":1230.5,\"AVG_LADEN_M\":1067.1,\"TOTAL_EMPTY_M\":1476600,\"TOTAL_LADEN_M\":1280520,\"DISTINCT_BLOCKS\":42}]"}"#;
        let rows = parse(raw).unwrap();
        assert_eq!(rows[0].jobtype, "MO");
        assert_eq!(rows[0].shift, "Night");
        assert!((rows[0].k_empty_ratio.unwrap() - 0.566).abs() < 1e-9);
    }
}
