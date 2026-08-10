//! K_TT_CYCLE extract -> raw_k_tt_cycle (one row per day). ★2026-08-10 재정의(사용자 승인):
//! local 경로(l_tt_cycle_day.sql)는 **배정→트럭 자유**(tt_move_log dispatch_ts→free_ts)를
//! 계산한다 — 근거·보존 규칙은 sql/local/l_tt_cycle.sql 머리. Oracle 킬스위치 경로는
//! 종전 c10(MCH_OPERATION per-truck QC-move interval·회전 리듬) 그대로 = 즉시 복귀 수단.
//! ⚠ 08-10 이전 raw 행은 c10 값 — 기간 조회가 그 날짜를 걸치면 두 정의가 섞인다(KC 고지).

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::{params, runner::Toolbox};

pub const KPI_KEY: &str = "K_TT_CYCLE";
const SQL: &str = include_str!("../../sql/c10_k_tt_cycle.sql");

#[derive(Debug, Clone, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "UPPERCASE")]
pub struct Row {
    pub trucks: Option<f64>,
    pub samples: Option<f64>,
    pub avg_sec: Option<f64>,
    pub med_sec: Option<f64>,
    pub p25_sec: Option<f64>,
    pub p75_sec: Option<f64>,
    pub ds_samples: Option<f64>,
    pub ds_med_sec: Option<f64>,
    pub ld_samples: Option<f64>,
    pub ld_med_sec: Option<f64>,
}

pub fn parse(raw: &str) -> Result<Vec<Row>> {
    parse_rows(raw).context("parsing k_tt_cycle rows")
}

pub async fn upsert(pool: &PgPool, date: NaiveDate, run_id: i64, rows: &[Row]) -> Result<u64> {
    // the SQL returns exactly one aggregate row; skip if it had no samples
    let r = match rows.first() {
        Some(r) if r.samples.unwrap_or(0.0) > 0.0 => r,
        _ => return Ok(0),
    };
    sqlx::query(
        "INSERT INTO raw_k_tt_cycle
           (snapshot_date, trucks, samples, avg_sec, med_sec, p25_sec, p75_sec,
            ds_samples, ds_med_sec, ld_samples, ld_med_sec, run_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
         ON CONFLICT (snapshot_date) DO UPDATE SET
           trucks=EXCLUDED.trucks, samples=EXCLUDED.samples, avg_sec=EXCLUDED.avg_sec,
           med_sec=EXCLUDED.med_sec, p25_sec=EXCLUDED.p25_sec, p75_sec=EXCLUDED.p75_sec,
           ds_samples=EXCLUDED.ds_samples, ds_med_sec=EXCLUDED.ds_med_sec,
           ld_samples=EXCLUDED.ld_samples, ld_med_sec=EXCLUDED.ld_med_sec,
           run_id=EXCLUDED.run_id, extracted_at=now()",
    )
    .bind(date)
    .bind(r.trucks.map(|v| v as i32))
    .bind(r.samples.map(|v| v as i32))
    .bind(r.avg_sec)
    .bind(r.med_sec)
    .bind(r.p25_sec)
    .bind(r.p75_sec)
    .bind(r.ds_samples.map(|v| v as i32))
    .bind(r.ds_med_sec)
    .bind(r.ld_samples.map(|v| v as i32))
    .bind(r.ld_med_sec)
    .bind(run_id)
    .execute(pool)
    .await
    .context("upserting raw_k_tt_cycle")?;
    Ok(1)
}

// KPI_NIGHTLY_SRC kill switch (PLAN-extractor.md CHUNK6 6-3): local Postgres mirror
// instead of the Oracle MCH_OPERATION day scan.
const SQL_LOCAL_DAY: &str = include_str!("../../sql/local/l_tt_cycle_day.sql");

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
    fn parses_row() {
        let raw = r#"{"result":"[{\"TRUCKS\":105,\"SAMPLES\":160,\"AVG_SEC\":804.9,\"MED_SEC\":851.5,\"P25_SEC\":666.8,\"P75_SEC\":1025.3}]"}"#;
        let rows = parse(raw).unwrap();
        assert_eq!(rows[0].samples, Some(160.0));
        assert_eq!(rows[0].med_sec, Some(851.5));
    }

    #[test]
    fn template_renders() {
        let date = NaiveDate::from_ymd_opt(2026, 6, 9).unwrap();
        let sql = params::render_day(SQL, date).unwrap();
        assert!(sql.contains("'20260609'"));
        assert!(!sql.contains("{{"));
    }
}
