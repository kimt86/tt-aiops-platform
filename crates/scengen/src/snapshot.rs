//! Periodic as-of yard-occupancy snapshot. Runs as its OWN systemd oneshot (separate cadence
//! from `collect` — a few times/day, not every 10 min). Captures block-level fill from the
//! CURRENT yard inventory (CYY_CONTAINER) so any past period start has a t=0 yard background;
//! CYY is overwritten each ETL, so this history can only be built going forward.
//!
//! One small aggregate query (~115k-row table -> ~285 block rows) — low load, off-peak-friendly.
//! Isolated + honors the kill switch, like `collect`.

use std::time::Instant;

use anyhow::Result;
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;
use wp_core::parse::parse_rows;

use crate::state::{self, Config};
use crate::toolbox::Toolbox;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
struct YardRow {
    block: Option<String>,
    n_total: Option<i64>,
    n_full: Option<i64>,
    n_reefer: Option<i64>,
    n_20ft: Option<i64>,
    n_import: Option<i64>,
}

pub async fn run(pool: &PgPool, target: &str) -> Result<()> {
    let cfg = state::load_config(pool).await?;
    if !cfg.enabled {
        tracing::info!("scenario collection disabled (kill switch) — skipping snapshot");
        return Ok(());
    }

    let run_id = state::start_run(pool, "snapshot").await?;
    match take(pool, run_id, target, &cfg).await {
        Ok(()) => {
            state::finish_run(pool, run_id, "done", None).await?;
        }
        Err(e) => {
            tracing::error!(error = %e, "scenario yard snapshot failed (isolated — others unaffected)");
            let _ = state::emit(pool, run_id, "error", "snapshot_failed", json!({ "error": e.to_string() })).await;
            let _ = state::finish_run(pool, run_id, "error", Some(&e.to_string())).await;
        }
    }
    Ok(()) // always Ok: non-critical subsystem must not cascade
}

async fn take(pool: &PgPool, run_id: i64, target: &str, cfg: &Config) -> Result<()> {
    state::set_phase(pool, run_id, "extract").await?;
    let snap_ts = Utc::now();

    // Block-level occupancy aggregate over the current yard inventory. block = first token of
    // CLOCATION (e.g. "09M-1819-E-1" -> "09M"). One aggregate; returns ~285 rows.
    let sql = "SELECT SUBSTR(CYY_CONT_CLOCATION,1,INSTR(CYY_CONT_CLOCATION,'-')-1) AS block, \
                      COUNT(*) AS n_total, \
                      SUM(CASE WHEN CYY_CONT_STATUS='F' THEN 1 ELSE 0 END) AS n_full, \
                      SUM(CASE WHEN CYY_CONT_CONTTYPE='RE' THEN 1 ELSE 0 END) AS n_reefer, \
                      SUM(CASE WHEN SUBSTR(CYY_CONT_ISO,1,1)='2' THEN 1 ELSE 0 END) AS n_20ft, \
                      SUM(CASE WHEN CYY_CONT_DISCHPORT='MYPKG' THEN 1 ELSE 0 END) AS n_import \
                 FROM TOSADM.CYY_CONTAINER \
                WHERE CYY_CONT_CLOCATION IS NOT NULL AND INSTR(CYY_CONT_CLOCATION,'-') >= 2 \
                GROUP BY SUBSTR(CYY_CONT_CLOCATION,1,INSTR(CYY_CONT_CLOCATION,'-')-1)";

    let t0 = Instant::now();
    let raw = Toolbox::from_env(target, cfg.oracle_timeout_s as u64)?
        .run_sql(sql)
        .await?;
    let query_ms = t0.elapsed().as_millis() as i64;
    let rows: Vec<YardRow> = parse_rows(&raw)?;

    let mut tx = pool.begin().await?;
    let (mut total_cont, mut nblocks) = (0i64, 0u64);
    for r in &rows {
        let Some(block) = r.block.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let n_total = r.n_total.unwrap_or(0);
        total_cont += n_total;
        nblocks += 1;
        sqlx::query(
            "INSERT INTO scenario.yard_snapshot
               (snapshot_ts, block, n_total, n_full, n_reefer, n_20ft, n_import)
             VALUES ($1,$2,$3,$4,$5,$6,$7)
             ON CONFLICT (snapshot_ts, block) DO NOTHING",
        )
        .bind(snap_ts)
        .bind(block)
        .bind(n_total as i32)
        .bind(r.n_full.unwrap_or(0) as i32)
        .bind(r.n_reefer.unwrap_or(0) as i32)
        .bind(r.n_20ft.unwrap_or(0) as i32)
        .bind(r.n_import.unwrap_or(0) as i32)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    state::merge_json(pool, run_id, "load_stats", json!({
        "queries": 1, "rows_read": rows.len(), "query_ms": query_ms,
        "oracle_timeout_s": cfg.oracle_timeout_s,
    })).await?;
    state::merge_json(pool, run_id, "collection", json!({
        "blocks": nblocks, "containers": total_cont, "snapshot_ts": snap_ts.to_rfc3339(),
    })).await?;

    tracing::info!(blocks = nblocks, containers = total_cont, query_ms, "scenario yard snapshot");
    Ok(())
}
