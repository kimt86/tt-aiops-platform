//! Per-crane per-jobtype median move interval (rolling 3 days) -> learn_qc_move_time.
//! Feeds the dispatch deadline/work-ETA calc so move time is crane-specific, not a flat constant.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::runner::Toolbox;

pub const KPI_KEY: &str = "QC_MOVE_TIME";
const SQL: &str = include_str!("../../sql/qc_move_time.sql");
// KPI_NIGHTLY_SRC kill switch (PLAN-extractor.md CHUNK6 6-3): local Postgres mirror
// (qc_move_log, rolling 3-day window) instead of the Oracle MCH_OPERATION scan.
const SQL_LOCAL: &str = include_str!("../../sql/local/l_qc_move_time.sql");

#[derive(Debug, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "UPPERCASE")]
pub struct Row {
    pub qc: String,
    pub jobtype: String,
    pub shift: String, // 'D' | 'N' | 'ALL'
    pub med_sec: Option<f64>,
    pub n: Option<f64>,
}

async fn land(pool: &PgPool, rows: &[Row]) -> Result<u64> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM learn_qc_move_time").execute(&mut *tx).await?;
    for r in rows {
        sqlx::query(
            "INSERT INTO learn_qc_move_time (qc, jobtype, shift, med_sec, n, as_of_ts)
             VALUES ($1,$2,$3,$4,$5,now())",
        )
        .bind(&r.qc)
        .bind(&r.jobtype)
        .bind(&r.shift)
        .bind(r.med_sec.map(|v| v as i32))
        .bind(r.n.map(|v| v as i32))
        .execute(&mut *tx)
        .await
        .context("insert learn_qc_move_time")?;
    }
    tx.commit().await?;
    Ok(rows.len() as u64)
}

pub async fn extract(pool: &PgPool, date: NaiveDate, target: &str) -> Result<u64> {
    if crate::kpis::common::nightly_src_local() {
        return run_logged(pool, KPI_KEY, date, |_run_id| async move {
            let rows: Vec<Row> = sqlx::query_as(SQL_LOCAL).fetch_all(pool).await?;
            land(pool, &rows).await
        })
        .await;
    }
    run_logged(pool, KPI_KEY, date, |_run_id| async move {
        let raw = Toolbox::from_env(target)?.run_sql(SQL).await?;
        let rows: Vec<Row> = parse_rows(&raw).context("parsing qc_move_time rows")?;
        land(pool, &rows).await
    })
    .await
}
