//! K_CRANE_Q daily extract: phase_c/08 -> raw_k_crane_q_daily (per work_date x jobtype).

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::{params, runner::Toolbox};

pub const KPI_KEY: &str = "K_CRANE_Q";
const SQL: &str = include_str!("../../sql/c08_k_crane_q.sql");

#[derive(Debug, Clone, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "UPPERCASE")]
pub struct Row {
    pub work_date: String, // YYYYMMDD
    pub jobtype: String,
    pub events_nn: Option<f64>,
    pub in_range: Option<f64>,
    pub k_crane_q_avg_sec: Option<f64>,
    pub k_crane_q_med_sec: Option<f64>,
    pub k_crane_q_std_sec: Option<f64>,
    pub min_sec: Option<f64>,
    pub max_sec: Option<f64>,
    pub anomaly_negative: Option<f64>,
    pub anomaly_over_30m: Option<f64>,
}

pub fn parse(raw: &str) -> Result<Vec<Row>> {
    parse_rows(raw).context("parsing k_crane_q_daily rows")
}

pub async fn upsert(pool: &PgPool, run_id: i64, rows: &[Row]) -> Result<u64> {
    let mut tx = pool.begin().await?;
    for r in rows {
        let work_date = NaiveDate::parse_from_str(r.work_date.trim(), "%Y%m%d")
            .with_context(|| format!("bad work_date '{}'", r.work_date))?;
        sqlx::query(
            "INSERT INTO raw_k_crane_q_daily
               (work_date, jobtype, events_nn, in_range, k_crane_q_avg_sec, k_crane_q_med_sec,
                k_crane_q_std_sec, min_sec, max_sec, anomaly_negative, anomaly_over_30m, run_id)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
             ON CONFLICT (work_date, jobtype) DO UPDATE SET
               events_nn=EXCLUDED.events_nn, in_range=EXCLUDED.in_range,
               k_crane_q_avg_sec=EXCLUDED.k_crane_q_avg_sec, k_crane_q_med_sec=EXCLUDED.k_crane_q_med_sec,
               k_crane_q_std_sec=EXCLUDED.k_crane_q_std_sec, min_sec=EXCLUDED.min_sec,
               max_sec=EXCLUDED.max_sec, anomaly_negative=EXCLUDED.anomaly_negative,
               anomaly_over_30m=EXCLUDED.anomaly_over_30m, run_id=EXCLUDED.run_id, extracted_at=now()",
        )
        .bind(work_date)
        .bind(&r.jobtype)
        .bind(r.events_nn)
        .bind(r.in_range)
        .bind(r.k_crane_q_avg_sec)
        .bind(r.k_crane_q_med_sec)
        .bind(r.k_crane_q_std_sec)
        .bind(r.min_sec)
        .bind(r.max_sec)
        .bind(r.anomaly_negative)
        .bind(r.anomaly_over_30m)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .context("upserting raw_k_crane_q_daily")?;
    }
    tx.commit().await?;
    Ok(rows.len() as u64)
}

// KPI_NIGHTLY_SRC kill switch (PLAN-extractor.md CHUNK6 6-3): local Postgres mirror
// (tos_handover_label, day window on comp_ts) instead of the Oracle
// JOB_ORDER_HISTORY day scan.
const SQL_LOCAL_DAY: &str = include_str!("../../sql/local/l_crane_q_day.sql");

pub async fn extract(pool: &PgPool, date: NaiveDate, target: &str) -> Result<u64> {
    if crate::kpis::common::nightly_src_local() {
        return run_logged(pool, KPI_KEY, date, |run_id| async move {
            let (ws, we) = crate::kpis::common::day_bounds_utc(date);
            let work_date = date.format("%Y%m%d").to_string();
            let rows: Vec<Row> = sqlx::query_as(SQL_LOCAL_DAY)
                .bind(ws)
                .bind(we)
                .bind(&work_date)
                .fetch_all(pool)
                .await?;
            upsert(pool, run_id, &rows).await
        })
        .await;
    }
    let sql = params::render_day(SQL, date)?;
    run_logged(pool, KPI_KEY, date, |run_id| async move {
        let raw = Toolbox::from_env(target)?.run_sql(&sql).await?;
        upsert(pool, run_id, &parse(&raw)?).await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rows() {
        let raw = r#"{"result":"[{\"WORK_DATE\":\"20260604\",\"JOBTYPE\":\"DS\",\"EVENTS_NN\":6821,\"IN_RANGE\":6500,\"K_CRANE_Q_AVG_SEC\":614.0,\"K_CRANE_Q_MED_SEC\":520.0,\"K_CRANE_Q_STD_SEC\":210.0,\"MIN_SEC\":0,\"MAX_SEC\":1799,\"ANOMALY_NEGATIVE\":null,\"ANOMALY_OVER_30M\":120}]"}"#;
        let rows = parse(raw).unwrap();
        assert_eq!(rows[0].work_date, "20260604");
        assert_eq!(rows[0].anomaly_negative, None);
        assert_eq!(rows[0].anomaly_over_30m, Some(120.0));
    }
}
