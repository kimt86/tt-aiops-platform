//! Yard-crane (RTG) move stream WITH decoded stack position -> scenario.yard_move. The event
//! source for the incremental yard-state model. Watermark-incremental over MCH_OPERATION (RTG
//! machines). CRNT_PSN_IDX decode (verified vs CYY.CLOCATION):
//!   block_id=NO1 · bay_idx=NO2 · row_idx=NO3(A=0) · tier=NO4+1
//! Isolated + kill-switch, like the other collectors.

use anyhow::Result;
use serde_json::{json, Value};
use sqlx::PgPool;
use wp_core::parse::parse_rows;

use crate::state::{self, Config};
use crate::toolbox::Toolbox;
use crate::util::{jstr, parse_myt};

const STREAM: &str = "yard_move";
const FETCH_CAP: u32 = 8000;

pub async fn run(pool: &PgPool, target: &str) -> Result<()> {
    let cfg = state::load_config(pool).await?;
    if !cfg.enabled {
        tracing::info!("scenario collection disabled (kill switch) — skipping yard-moves");
        return Ok(());
    }
    let run_id = state::start_run(pool, "yard_moves").await?;
    match tick(pool, run_id, target, &cfg).await {
        Ok(()) => {
            state::finish_run(pool, run_id, "done", None).await?;
        }
        Err(e) => {
            tracing::error!(error = %e, "scenario yard-moves failed (isolated — others unaffected)");
            let _ = state::emit(pool, run_id, "error", "yard_moves_failed", json!({ "error": e.to_string() })).await;
            let _ = state::finish_run(pool, run_id, "error", Some(&e.to_string())).await;
        }
    }
    Ok(())
}

async fn tick(pool: &PgPool, run_id: i64, target: &str, cfg: &Config) -> Result<()> {
    state::set_phase(pool, run_id, "extract").await?;

    let day = wp_core::shift::terminal_now().format("%Y%m%d").to_string();
    let wm = state::get_watermark(pool, STREAM)
        .await?
        .unwrap_or_else(|| format!("{day}000000"));

    // Index-supported (IDX_MCH_OPERATION_COMPDATE): today's RTG moves since the watermark.
    let sql = format!(
        "SELECT MCH_OPER_MACHNO AS machno, SUBSTR(MCH_OPER_CONTNO,1,11) AS contno,
                MCH_OPER_SEQNO AS seqno, MCH_OPER_JOBTYPE AS jt,
                MCH_OPER_COMPDATE||MCH_OPER_COMPTIME AS comp,
                CRNT_PSN_IDX_NO1 AS b, CRNT_PSN_IDX_NO2 AS y,
                CRNT_PSN_IDX_NO3 AS rr, CRNT_PSN_IDX_NO4 AS tt
           FROM TOSADM.MCH_OPERATION
          WHERE MCH_OPER_COMPDATE = '{day}'
            AND MCH_OPER_COMPDATE||MCH_OPER_COMPTIME > '{wm}'
            AND MCH_OPER_MACHNO LIKE 'RTG%'
            AND CRNT_PSN_IDX_NO1 IS NOT NULL
            AND LENGTH(MCH_OPER_COMPTIME) >= 6
          ORDER BY MCH_OPER_COMPDATE||MCH_OPER_COMPTIME
          FETCH FIRST {FETCH_CAP} ROWS ONLY"
    );

    let t0 = std::time::Instant::now();
    let raw = Toolbox::from_env(target, cfg.oracle_timeout_s as u64)?
        .run_sql(&sql)
        .await?;
    let query_ms = t0.elapsed().as_millis() as i64;
    let rows: Vec<Value> = parse_rows(&raw)?;
    let fetched = rows.len();

    let num = |r: &Value, k: &str| jstr(r, k).and_then(|s| s.parse::<i32>().ok());

    let mut tx = pool.begin().await?;
    let mut max_comp: Option<String> = None;
    let mut inserted = 0u64;
    for r in &rows {
        let (Some(machno), Some(contno), Some(seqno), Some(comp)) =
            (jstr(r, "MACHNO"), jstr(r, "CONTNO"), jstr(r, "SEQNO"), jstr(r, "COMP"))
        else {
            continue;
        };
        let Some(comp_ts) = parse_myt(&comp) else { continue };
        let (Some(b), Some(y), Some(ri), Some(tt)) =
            (num(r, "B"), num(r, "Y"), num(r, "RR"), num(r, "TT"))
        else {
            continue;
        };
        let jt = jstr(r, "JT").unwrap_or_default();
        let res = sqlx::query(
            "INSERT INTO scenario.yard_move
               (comp_ts, contno, jobtype, block_id, bay_idx, row_idx, tier, machno, seqno)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
             ON CONFLICT (machno, contno, seqno) DO NOTHING",
        )
        .bind(comp_ts)
        .bind(contno.trim())
        .bind(&jt)
        .bind(b)
        .bind(y)
        .bind(ri)
        .bind(tt + 1) // tier = NO4 + 1
        .bind(machno.trim())
        .bind(seqno.trim())
        .execute(&mut *tx)
        .await?;
        inserted += res.rows_affected();
        if max_comp.as_deref().is_none_or(|m| comp.as_str() > m) {
            max_comp = Some(comp);
        }
    }
    tx.commit().await?;

    if let Some(mx) = &max_comp {
        state::set_watermark(pool, STREAM, mx).await?;
    }

    let capped = fetched as u32 >= FETCH_CAP;
    state::merge_json(pool, run_id, "load_stats", json!({
        "queries": 1, "rows_read": fetched, "query_ms": query_ms, "fetch_capped": capped,
    })).await?;
    state::merge_json(pool, run_id, "collection", json!({ "fetched": fetched, "inserted": inserted }))
        .await?;

    tracing::info!(fetched, inserted, query_ms, capped, "scenario yard moves");
    Ok(())
}
