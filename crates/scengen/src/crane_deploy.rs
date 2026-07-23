//! QC (quay-crane) deployment collector: TOSADM.JOB_CRANE_HISTORY -> scenario.crane_deploy.
//! Which crane was assigned to which vessel, and when. Its PK leads with JOB_CRHIST_DATE, so we
//! seek from the watermark's DATE (the whole table is ~8k rows, so re-reading a day is trivial and
//! inherently covers late-visible rows; ON CONFLICT dedups). Isolated + kill-switch, like collect.
//!
//! JOB_CRANE_HISTORY has QUAY cranes only (C##/M##/Z##) — RTG block deployment is NOT here; it is
//! derived at assembly time from scenario.yard_move (an RTG's block over time).

use std::time::Instant;

use anyhow::Result;
use serde_json::json;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::state::{self, Config};
use crate::toolbox::Toolbox;
use crate::util::{jstr, parse_myt};

const STREAM: &str = "crane_deploy";
const FETCH_CAP: u32 = 5000;

pub async fn run(pool: &PgPool, target: &str) -> Result<()> {
    let cfg = state::load_config(pool).await?;
    if !cfg.enabled {
        tracing::info!("scenario collection disabled (kill switch) — skipping crane-deploy");
        return Ok(());
    }
    let run_id = state::start_run(pool, "crane_deploy").await?;
    match tick(pool, run_id, target, &cfg).await {
        Ok(()) => {
            state::finish_run(pool, run_id, "done", None).await?;
        }
        Err(e) => {
            tracing::error!(error = %e, "scenario crane-deploy failed (isolated — others unaffected)");
            let _ = state::emit(pool, run_id, "error", "crane_deploy_failed", json!({ "error": e.to_string() })).await;
            let _ = state::finish_run(pool, run_id, "error", Some(&e.to_string())).await;
        }
    }
    Ok(()) // always Ok: non-critical subsystem must not cascade
}

async fn tick(pool: &PgPool, run_id: i64, target: &str, cfg: &Config) -> Result<()> {
    state::set_phase(pool, run_id, "extract").await?;

    // Watermark is 'YYYYMMDD...' MYT; seek from its DATE (PK leading column). First run: last 1 day.
    let wm = state::get_watermark(pool, STREAM).await?;
    let from_date = wm
        .as_deref()
        .and_then(|w| w.get(..8).map(str::to_string))
        .unwrap_or_else(|| {
            (tt_core::shift::terminal_now() - chrono::Duration::days(1))
                .format("%Y%m%d")
                .to_string()
        });

    // PK JOBCRANE_PK_HISTORY leads with JOB_CRHIST_DATE -> `>= from_date` is an index range seek.
    let sql = format!(
        "SELECT JOB_CRHIST_CRANENO AS craneno, JOB_CRHIST_VESSEL AS vessel,
                JOB_CRHIST_VOYAGE AS voyage, JOB_CRHIST_TYPE AS ty,
                JOB_CRHIST_DATE||JOB_CRHIST_TIME AS evk
           FROM TOSADM.JOB_CRANE_HISTORY
          WHERE JOB_CRHIST_DATE >= '{from_date}'
          ORDER BY JOB_CRHIST_DATE, JOB_CRHIST_TIME
          FETCH FIRST {FETCH_CAP} ROWS ONLY"
    );

    let t0 = Instant::now();
    let raw = Toolbox::from_env(target, cfg.oracle_timeout_s as u64)?
        .run_sql(&sql)
        .await?;
    let query_ms = t0.elapsed().as_millis() as i64;
    let rows: Vec<serde_json::Value> = parse_rows(&raw)?;
    let fetched = rows.len();

    let mut tx = pool.begin().await?;
    let mut max_key: Option<String> = None;
    let mut inserted = 0u64;
    for r in &rows {
        let (Some(craneno), Some(evk)) = (jstr(r, "CRANENO"), jstr(r, "EVK")) else {
            continue;
        };
        let Some(ev_ts) = parse_myt(&evk) else { continue };
        let res = sqlx::query(
            "INSERT INTO scenario.crane_deploy (crane_no, vessel, voyage, ev_type, ev_ts, ev_key)
             VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (crane_no, ev_key) DO NOTHING",
        )
        .bind(craneno.trim())
        .bind(jstr(r, "VESSEL"))
        .bind(jstr(r, "VOYAGE"))
        .bind(jstr(r, "TY"))
        .bind(ev_ts)
        .bind(evk.trim())
        .execute(&mut *tx)
        .await?;
        inserted += res.rows_affected();
        // Advance only on well-formed keys (all-digit, >= 14). Guards a malformed jump/stall.
        if crate::util::is_wm_key(&evk) && max_key.as_deref().is_none_or(|m| evk.as_str() > m) {
            max_key = Some(evk);
        }
    }
    tx.commit().await?;

    if let Some(mx) = &max_key {
        state::set_watermark(pool, STREAM, mx).await?;
    }

    let capped = fetched as u32 >= FETCH_CAP;
    state::merge_json(pool, run_id, "load_stats", json!({
        "queries": 1, "rows_read": fetched, "query_ms": query_ms, "fetch_capped": capped,
    })).await?;
    state::merge_json(pool, run_id, "collection", json!({ "fetched": fetched, "inserted": inserted }))
        .await?;

    tracing::info!(fetched, inserted, query_ms, capped, "scenario crane deploy");
    Ok(())
}
