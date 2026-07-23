//! K_UTIL (QC + YC) extract: e1c -> raw_k_util_crane.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::{params, runner::Toolbox};

pub const KPI_KEY: &str = "K_UTIL_CRANE";
const SQL: &str = include_str!("../../sql/e1c_k_util_crane_merged_intervals.sql");

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct Row {
    pub machno: String,
    pub machine_type: String,
    pub interval_groups: Option<f64>,
    pub total_moves: Option<f64>,
    pub active_sec_merged: Option<f64>,
    pub k_util_merged_24h: Option<f64>,
    pub avg_grp_sec: Option<f64>,
    pub longest_grp_sec: Option<f64>,
}

pub fn parse(raw: &str) -> Result<Vec<Row>> {
    parse_rows(raw).context("parsing k_util_crane rows")
}

pub async fn upsert(pool: &PgPool, date: NaiveDate, run_id: i64, rows: &[Row]) -> Result<u64> {
    let mut tx = pool.begin().await?;
    for r in rows {
        sqlx::query(
            "INSERT INTO raw_k_util_crane
               (snapshot_date, machno, machine_type, interval_groups, total_moves,
                active_sec_merged, k_util_merged_24h, avg_grp_sec, longest_grp_sec, run_id)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
             ON CONFLICT (snapshot_date, machno) DO UPDATE SET
               machine_type=EXCLUDED.machine_type, interval_groups=EXCLUDED.interval_groups,
               total_moves=EXCLUDED.total_moves, active_sec_merged=EXCLUDED.active_sec_merged,
               k_util_merged_24h=EXCLUDED.k_util_merged_24h, avg_grp_sec=EXCLUDED.avg_grp_sec,
               longest_grp_sec=EXCLUDED.longest_grp_sec, run_id=EXCLUDED.run_id, extracted_at=now()",
        )
        .bind(date)
        .bind(&r.machno)
        .bind(&r.machine_type)
        .bind(r.interval_groups)
        .bind(r.total_moves)
        .bind(r.active_sec_merged)
        .bind(r.k_util_merged_24h)
        .bind(r.avg_grp_sec)
        .bind(r.longest_grp_sec)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .context("upserting raw_k_util_crane")?;
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
        let raw = r#"{"result":"[{\"MACHNO\":\"C17\",\"MACHINE_TYPE\":\"QC\",\"INTERVAL_GROUPS\":42,\"TOTAL_MOVES\":318,\"ACTIVE_SEC_MERGED\":46400,\"K_UTIL_MERGED_24H\":0.537,\"AVG_GRP_SEC\":1104.8,\"LONGEST_GRP_SEC\":3450.0},{\"MACHNO\":\"RTG50\",\"MACHINE_TYPE\":\"YC\",\"INTERVAL_GROUPS\":20,\"TOTAL_MOVES\":140,\"ACTIVE_SEC_MERGED\":21600,\"K_UTIL_MERGED_24H\":0.25,\"AVG_GRP_SEC\":1080.0,\"LONGEST_GRP_SEC\":2400.0}]"}"#;
        let rows = parse(raw).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].machine_type, "QC");
        assert_eq!(rows[1].machine_type, "YC");
        assert!((rows[0].k_util_merged_24h.unwrap() - 0.537).abs() < 1e-9);
    }
}
