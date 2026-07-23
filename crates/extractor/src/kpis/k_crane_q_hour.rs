//! K_CRANE_Q hourly extract: e5 -> raw_k_crane_q_hour (per hour 0-23).

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::{params, runner::Toolbox};

pub const KPI_KEY: &str = "K_CRANE_Q_HOUR";
const SQL: &str = include_str!("../../sql/e5_k_crane_q_by_hour.sql");

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct Row {
    pub hour: String, // SUBSTR(JOB_HIST_TIME,1,2) -> "00".."23"
    pub events: Option<f64>,
    pub avg_sec: Option<f64>,
    pub med_sec: Option<f64>,
    pub std_sec: Option<f64>,
    pub p25: Option<f64>,
    pub p75: Option<f64>,
    pub p95: Option<f64>,
    pub alert_threshold_sec: Option<f64>,
    pub distinct_cranes: Option<f64>,
}

pub fn parse(raw: &str) -> Result<Vec<Row>> {
    parse_rows(raw).context("parsing k_crane_q_hour rows")
}

pub async fn upsert(pool: &PgPool, date: NaiveDate, run_id: i64, rows: &[Row]) -> Result<u64> {
    let mut tx = pool.begin().await?;
    for r in rows {
        let hour: i16 = r
            .hour
            .trim()
            .parse()
            .with_context(|| format!("non-numeric hour '{}'", r.hour))?;
        sqlx::query(
            "INSERT INTO raw_k_crane_q_hour
               (snapshot_date, hour, events, avg_sec, med_sec, std_sec, p25, p75, p95,
                alert_threshold_sec, distinct_cranes, run_id)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
             ON CONFLICT (snapshot_date, hour) DO UPDATE SET
               events=EXCLUDED.events, avg_sec=EXCLUDED.avg_sec, med_sec=EXCLUDED.med_sec,
               std_sec=EXCLUDED.std_sec, p25=EXCLUDED.p25, p75=EXCLUDED.p75, p95=EXCLUDED.p95,
               alert_threshold_sec=EXCLUDED.alert_threshold_sec, distinct_cranes=EXCLUDED.distinct_cranes,
               run_id=EXCLUDED.run_id, extracted_at=now()",
        )
        .bind(date)
        .bind(hour)
        .bind(r.events)
        .bind(r.avg_sec)
        .bind(r.med_sec)
        .bind(r.std_sec)
        .bind(r.p25)
        .bind(r.p75)
        .bind(r.p95)
        .bind(r.alert_threshold_sec)
        .bind(r.distinct_cranes)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .context("upserting raw_k_crane_q_hour")?;
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
        let raw = r#"{"result":"[{\"HOUR\":\"12\",\"EVENTS\":420,\"AVG_SEC\":1080.0,\"MED_SEC\":990.0,\"STD_SEC\":260.0,\"P25\":820.0,\"P75\":1300.0,\"P95\":1700.0,\"ALERT_THRESHOLD_SEC\":1600.0,\"DISTINCT_CRANES\":18}]"}"#;
        let rows = parse(raw).unwrap();
        assert_eq!(rows[0].hour, "12");
        assert_eq!(rows[0].distinct_cranes, Some(18.0));
    }
}
