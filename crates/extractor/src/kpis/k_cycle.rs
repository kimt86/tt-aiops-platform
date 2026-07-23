//! K_CYCLE extract: e3b v2 -> raw_k_cycle (per jobtype).

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::{params, runner::Toolbox};

pub const KPI_KEY: &str = "K_CYCLE";
const SQL: &str = include_str!("../../sql/e3b_k_cycle_refined_v2.sql");

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct Row {
    pub jobtype: String,
    pub jobs: Option<f64>,
    pub avg_sec: Option<f64>,
    pub med_sec: Option<f64>,
    pub std_sec: Option<f64>,
    pub p25_sec: Option<f64>,
    pub p75_sec: Option<f64>,
    pub p95_sec: Option<f64>,
    pub outlier_threshold_sec: Option<f64>,
    pub outlier_n: Option<f64>,
    pub avg_transitions: Option<f64>,
}

pub fn parse(raw: &str) -> Result<Vec<Row>> {
    parse_rows(raw).context("parsing k_cycle rows")
}

pub async fn upsert(pool: &PgPool, date: NaiveDate, run_id: i64, rows: &[Row]) -> Result<u64> {
    let mut tx = pool.begin().await?;
    for r in rows {
        sqlx::query(
            "INSERT INTO raw_k_cycle
               (snapshot_date, jobtype, jobs, avg_sec, med_sec, std_sec, p25_sec, p75_sec,
                p95_sec, outlier_threshold_sec, outlier_n, avg_transitions, run_id)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
             ON CONFLICT (snapshot_date, jobtype) DO UPDATE SET
               jobs=EXCLUDED.jobs, avg_sec=EXCLUDED.avg_sec, med_sec=EXCLUDED.med_sec,
               std_sec=EXCLUDED.std_sec, p25_sec=EXCLUDED.p25_sec, p75_sec=EXCLUDED.p75_sec,
               p95_sec=EXCLUDED.p95_sec, outlier_threshold_sec=EXCLUDED.outlier_threshold_sec,
               outlier_n=EXCLUDED.outlier_n, avg_transitions=EXCLUDED.avg_transitions,
               run_id=EXCLUDED.run_id, extracted_at=now()",
        )
        .bind(date)
        .bind(&r.jobtype)
        .bind(r.jobs)
        .bind(r.avg_sec)
        .bind(r.med_sec)
        .bind(r.std_sec)
        .bind(r.p25_sec)
        .bind(r.p75_sec)
        .bind(r.p95_sec)
        .bind(r.outlier_threshold_sec)
        .bind(r.outlier_n)
        .bind(r.avg_transitions)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .context("upserting raw_k_cycle")?;
    }
    tx.commit().await?;
    Ok(rows.len() as u64)
}

pub async fn extract(pool: &PgPool, date: NaiveDate, target: &str) -> Result<u64> {
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
        let raw = r#"{"result":"[{\"JOBTYPE\":\"LD\",\"JOBS\":10111,\"AVG_SEC\":3720.0,\"MED_SEC\":3720.0,\"STD_SEC\":640.2,\"P25_SEC\":3000.0,\"P75_SEC\":4200.0,\"P95_SEC\":7920.0,\"OUTLIER_THRESHOLD_SEC\":8640.0,\"OUTLIER_N\":348,\"AVG_TRANSITIONS\":4.2}]"}"#;
        let rows = parse(raw).unwrap();
        assert_eq!(rows[0].jobtype, "LD");
        assert_eq!(rows[0].outlier_n, Some(348.0));
    }
}
