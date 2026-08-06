//! K_MPH realtime per-QC extract: phase_c/07 -> raw_k_mph_realtime.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::{params, runner::Toolbox};

pub const KPI_KEY: &str = "K_MPH_REALTIME";
const SQL: &str = include_str!("../../sql/c07_k_mph_realtime.sql");

#[derive(Debug, Clone, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "UPPERCASE")]
pub struct Row {
    pub vessel: String,
    pub voyage: String,
    pub qc_machno: String,
    pub moves: Option<f64>,
    pub load_moves: Option<f64>,
    pub discharge_moves: Option<f64>,
    pub active_hours: Option<f64>,
    pub k_mph_per_active_hour: Option<f64>,
    pub distinct_trucks: Option<f64>,
    pub distinct_containers: Option<f64>,
    pub first_move: Option<String>,
    pub last_move: Option<String>,
}

pub fn parse(raw: &str) -> Result<Vec<Row>> {
    parse_rows(raw).context("parsing k_mph_realtime rows")
}

pub async fn upsert(pool: &PgPool, date: NaiveDate, run_id: i64, rows: &[Row]) -> Result<u64> {
    let mut tx = pool.begin().await?;
    for r in rows {
        sqlx::query(
            "INSERT INTO raw_k_mph_realtime
               (snapshot_date, vessel, voyage, qc_machno, moves, load_moves, discharge_moves,
                active_hours, k_mph_per_active_hour, distinct_trucks, distinct_containers,
                first_move, last_move, run_id)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
             ON CONFLICT (snapshot_date, vessel, voyage, qc_machno) DO UPDATE SET
               moves=EXCLUDED.moves, load_moves=EXCLUDED.load_moves,
               discharge_moves=EXCLUDED.discharge_moves, active_hours=EXCLUDED.active_hours,
               k_mph_per_active_hour=EXCLUDED.k_mph_per_active_hour,
               distinct_trucks=EXCLUDED.distinct_trucks, distinct_containers=EXCLUDED.distinct_containers,
               first_move=EXCLUDED.first_move, last_move=EXCLUDED.last_move,
               run_id=EXCLUDED.run_id, extracted_at=now()",
        )
        .bind(date)
        .bind(&r.vessel)
        .bind(&r.voyage)
        .bind(&r.qc_machno)
        .bind(r.moves)
        .bind(r.load_moves)
        .bind(r.discharge_moves)
        .bind(r.active_hours)
        .bind(r.k_mph_per_active_hour)
        .bind(r.distinct_trucks)
        .bind(r.distinct_containers)
        .bind(&r.first_move)
        .bind(&r.last_move)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .context("upserting raw_k_mph_realtime")?;
    }
    tx.commit().await?;
    Ok(rows.len() as u64)
}

// KPI_NIGHTLY_SRC kill switch (PLAN-extractor.md CHUNK6 6-3): local Postgres mirror
// (qc_move_log.business_date) instead of the Oracle MCH_OPERATION day scan.
const SQL_LOCAL_DAY: &str = include_str!("../../sql/local/l_mph_day.sql");

pub async fn extract(pool: &PgPool, date: NaiveDate, target: &str) -> Result<u64> {
    if crate::kpis::common::nightly_src_local() {
        return run_logged(pool, KPI_KEY, date, |run_id| async move {
            let rows: Vec<Row> = sqlx::query_as(SQL_LOCAL_DAY).bind(date).fetch_all(pool).await?;
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
        let raw = r#"{"result":"[{\"VESSEL\":\"MAERSK\",\"VOYAGE\":\"123E\",\"QC_MACHNO\":\"C17\",\"MOVES\":318,\"LOAD_MOVES\":120,\"DISCHARGE_MOVES\":198,\"ACTIVE_HOURS\":12,\"K_MPH_PER_ACTIVE_HOUR\":26.5,\"DISTINCT_TRUCKS\":14,\"DISTINCT_CONTAINERS\":300,\"FIRST_MOVE\":\"20260604060102\",\"LAST_MOVE\":\"20260604175533\"}]"}"#;
        let rows = parse(raw).unwrap();
        assert_eq!(rows[0].qc_machno, "C17");
        assert!((rows[0].k_mph_per_active_hour.unwrap() - 26.5).abs() < 1e-9);
        assert_eq!(rows[0].first_move.as_deref(), Some("20260604060102"));
    }
}
