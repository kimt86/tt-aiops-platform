//! K_QC_Q extract: f2 (quay-crane idle gaps) -> raw_k_qc_q (per quay crane).

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::PgPool;
use wp_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::{params, runner::Toolbox};

pub const KPI_KEY: &str = "K_QC_Q";
const SQL: &str = include_str!("../../sql/f2_k_qc_q.sql");

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct Row {
    pub qc: String,
    pub idle_periods: Option<f64>,
    pub quick_under_1m: Option<f64>,
    pub normal_1_5m: Option<f64>,
    pub delayed_5_10m: Option<f64>,
    pub extended_10_30m: Option<f64>,
    pub over_30m: Option<f64>,
    pub avg_idle_sec: Option<f64>,
    pub med_idle_sec: Option<f64>,
    pub total_tt_wait_sec: Option<f64>,
    pub total_idle_30m_sec: Option<f64>,
    // same-bay subset (≈ true QC-waits-for-truck) — gap with unchanged queue/bay across it
    pub same_bay_periods: Option<f64>,
    pub same_bay_avg_sec: Option<f64>,
    pub same_bay_med_sec: Option<f64>,
    pub same_bay_total_sec: Option<f64>,
}

pub fn parse(raw: &str) -> Result<Vec<Row>> {
    parse_rows(raw).context("parsing k_qc_q rows")
}

pub async fn upsert(pool: &PgPool, date: NaiveDate, run_id: i64, rows: &[Row]) -> Result<u64> {
    let mut tx = pool.begin().await?;
    for r in rows {
        sqlx::query(
            "INSERT INTO raw_k_qc_q
               (snapshot_date, qc, idle_periods, quick_under_1m, normal_1_5m, delayed_5_10m,
                extended_10_30m, over_30m, avg_idle_sec, med_idle_sec, total_tt_wait_sec,
                total_idle_30m_sec, run_id,
                same_bay_periods, same_bay_avg_sec, same_bay_med_sec, same_bay_total_sec)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)
             ON CONFLICT (snapshot_date, qc) DO UPDATE SET
               idle_periods=EXCLUDED.idle_periods, quick_under_1m=EXCLUDED.quick_under_1m,
               normal_1_5m=EXCLUDED.normal_1_5m, delayed_5_10m=EXCLUDED.delayed_5_10m,
               extended_10_30m=EXCLUDED.extended_10_30m, over_30m=EXCLUDED.over_30m,
               avg_idle_sec=EXCLUDED.avg_idle_sec, med_idle_sec=EXCLUDED.med_idle_sec,
               total_tt_wait_sec=EXCLUDED.total_tt_wait_sec, total_idle_30m_sec=EXCLUDED.total_idle_30m_sec,
               same_bay_periods=EXCLUDED.same_bay_periods, same_bay_avg_sec=EXCLUDED.same_bay_avg_sec,
               same_bay_med_sec=EXCLUDED.same_bay_med_sec, same_bay_total_sec=EXCLUDED.same_bay_total_sec,
               run_id=EXCLUDED.run_id, extracted_at=now()",
        )
        .bind(date)
        .bind(&r.qc)
        .bind(r.idle_periods)
        .bind(r.quick_under_1m)
        .bind(r.normal_1_5m)
        .bind(r.delayed_5_10m)
        .bind(r.extended_10_30m)
        .bind(r.over_30m)
        .bind(r.avg_idle_sec)
        .bind(r.med_idle_sec)
        .bind(r.total_tt_wait_sec)
        .bind(r.total_idle_30m_sec)
        .bind(run_id)
        .bind(r.same_bay_periods)
        .bind(r.same_bay_avg_sec)
        .bind(r.same_bay_med_sec)
        .bind(r.same_bay_total_sec)
        .execute(&mut *tx)
        .await
        .context("upserting raw_k_qc_q")?;
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
        let raw = r#"{"result":"[{\"QC\":\"C37\",\"IDLE_PERIODS\":120,\"QUICK_UNDER_1M\":40,\"NORMAL_1_5M\":50,\"DELAYED_5_10M\":18,\"EXTENDED_10_30M\":9,\"OVER_30M\":3,\"AVG_IDLE_SEC\":182.4,\"MED_IDLE_SEC\":140.0,\"TOTAL_TT_WAIT_SEC\":15600,\"TOTAL_IDLE_30M_SEC\":21800}]"}"#;
        let rows = parse(raw).unwrap();
        assert_eq!(rows[0].qc, "C37");
        assert_eq!(rows[0].med_idle_sec, Some(140.0));
    }
}
