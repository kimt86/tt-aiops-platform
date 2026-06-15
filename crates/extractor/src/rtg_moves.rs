//! Yard-crane (RTG/ES) move stream from MCH_OPERATION → rtg_move_log. The dashboard's KPI
//! extractor filters MCH_OPERATION to QC (^C), so the RTG side was never landed — yet RTG moves
//! ARE logged in detail (ST_DT start + COMPDATE||COMPTIME complete) for the full work mix
//! (DS/LD/RH/AH/GI/GO/MI/MO). DS handovers are only ~20% of an RTG's moves; the rest (reshuffles,
//! gate, repositioning) is what our DS truck waits behind. This stream gives the RTG's real
//! backlog as a wait-prediction feature. Incremental via etl_watermark (stream='rtg_move').
//! See research/rtg-work-cycle.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::PgPool;
use wp_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::runner::Toolbox;
use crate::workpool::parse_etw; // shared MYT "YYYYMMDDHH24MISS[mmm]" → UTC parser

const STREAM: &str = "rtg_move";
const FETCH_CAP: u32 = 5000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
struct MoveRow {
    machno: Option<String>,
    contno: Option<String>,
    seqno: Option<String>,
    jobtype: Option<String>,
    trk_id: Option<String>,
    st_dt: Option<String>,
    comp_dt: Option<String>, // MCH_OPER_COMPDATE||MCH_OPER_COMPTIME (14-char)
}

/// One incremental poll: upsert yard-crane moves completed since the watermark, advance it.
pub async fn tick_rtg_moves(pool: &PgPool, target: &str) -> Result<()> {
    let today = wp_core::shift::terminal_now();
    let day = today.format("%Y%m%d").to_string();
    let run_date = today.date_naive();
    run_logged(pool, "RTG_MOVE", run_date, |_| async move {
        // watermark = last move comp seen (text "YYYYMMDDHHMMSS"). First run: start of today so
        // we self-backfill today (FETCH_CAP per poll, ORDER BY comp ASC → catches up over polls).
        let wm: Option<String> = sqlx::query_scalar(
            "SELECT max(last_completed_at) FROM etl_watermark WHERE stream = $1",
        )
        .bind(STREAM)
        .fetch_one(pool)
        .await?;
        let wm = wm.unwrap_or_else(|| format!("{day}000000"));

        // COMPDATE='today' uses IDX_MCH_OPERATION_COMPDATE; comp>wm + REGEXP(RTG|ES) post-filter.
        // Same scan profile as the 5-min MPH extractor. Excludes QC (already in MPH) and trucks.
        let sql = format!(
            "SELECT MCH_OPER_MACHNO AS machno, SUBSTR(MCH_OPER_CONTNO,1,11) AS contno,
                    MCH_OPER_SEQNO AS seqno, MCH_OPER_JOBTYPE AS jobtype, TRK_ID AS trk_id,
                    ST_DT AS st_dt, MCH_OPER_COMPDATE||MCH_OPER_COMPTIME AS comp_dt
               FROM TOSADM.MCH_OPERATION
              WHERE MCH_OPER_COMPDATE = '{day}'
                AND MCH_OPER_COMPDATE||MCH_OPER_COMPTIME > '{wm}'
                AND REGEXP_LIKE(MCH_OPER_MACHNO, '^(RTG|ES)')
                AND LENGTH(MCH_OPER_COMPTIME) >= 6
              ORDER BY MCH_OPER_COMPDATE||MCH_OPER_COMPTIME
              FETCH FIRST {FETCH_CAP} ROWS ONLY"
        );
        let raw = Toolbox::from_env(target)?.run_sql(&sql).await?;
        let rows: Vec<MoveRow> = parse_rows(&raw).context("parsing rtg move rows")?;

        let mut tx = pool.begin().await?;
        let mut max_comp: Option<String> = None;
        let mut inserted = 0u64;
        for r in &rows {
            let (Some(machno), Some(contno), Some(seqno), Some(comp_dt)) =
                (r.machno.as_deref(), r.contno.as_deref(), r.seqno.as_deref(), r.comp_dt.as_deref())
            else {
                continue;
            };
            let Some(comp_ts) = parse_etw(comp_dt) else { continue };
            let st_ts = r.st_dt.as_deref().and_then(parse_etw);
            let dur_s = st_ts.map(|st| (comp_ts - st).num_seconds()).filter(|&d| (0..=3600).contains(&d));
            let bdate = NaiveDate::parse_from_str(comp_dt.get(..8).unwrap_or(""), "%Y%m%d").unwrap_or(run_date);
            let res = sqlx::query(
                "INSERT INTO rtg_move_log
                   (machno, contno, seqno, jobtype, trk_id, st_ts, comp_ts, dur_s, business_date)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
                 ON CONFLICT (machno, contno, seqno) DO NOTHING",
            )
            .bind(machno.trim())
            .bind(contno.trim())
            .bind(seqno.trim())
            .bind(r.jobtype.as_deref().map(str::trim))
            .bind(r.trk_id.as_deref().map(str::trim).filter(|s| !s.is_empty()))
            .bind(st_ts)
            .bind(comp_ts)
            .bind(dur_s.map(|d| d as i32))
            .bind(bdate)
            .execute(&mut *tx)
            .await
            .context("insert rtg_move_log")?;
            inserted += res.rows_affected();
            if max_comp.as_deref().is_none_or(|m| comp_dt > m) {
                max_comp = Some(comp_dt.to_string());
            }
        }
        if let Some(mx) = max_comp {
            sqlx::query(
                "INSERT INTO etl_watermark (stream, snapshot_date, last_completed_at, updated_at)
                 VALUES ($1, $2, $3, now())
                 ON CONFLICT (stream, snapshot_date) DO UPDATE
                   SET last_completed_at = GREATEST(etl_watermark.last_completed_at, EXCLUDED.last_completed_at),
                       updated_at = now()",
            )
            .bind(STREAM)
            .bind(run_date)
            .bind(&mx)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        tracing::info!(fetched = rows.len(), inserted, "rtg moves");
        Ok(rows.len() as u64)
    })
    .await
    .map(|_| ())
}
