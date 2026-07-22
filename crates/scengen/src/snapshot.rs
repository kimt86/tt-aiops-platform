//! ON-DEMAND yard block-map refresh — NOT scheduled any more (the 4h timer was retired). One
//! aggregate over the current yard inventory (CYY_CONTAINER) producing (a) `scenario.yard_block`,
//! the block_id -> block-name map used to LABEL reconstructed yard cells, and (b) a block-level
//! occupancy row set `scenario.yard_snapshot`.
//!
//! Its original job — supplying the scenario's t=0 yard background — is GONE: yard_t0 is now
//! reconstructed per-container as-of-T by replaying scenario.yard_move (see yard::state_as_of).
//! Nothing reads yard_snapshot functionally any more, and the block map is physical infrastructure
//! that essentially never changes (302 blocks mapped, 0 unresolved). Re-scanning a ~115k-row hot
//! table every 4h to refresh a static 302-row map was pure waste, so it is now manual: run it only
//! when the admin page reports unresolved block_ids ("야드 블록맵 ⚠미해석").
//!
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
    block_id: Option<i64>,
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
                      MAX(TO_NUMBER(CRNT_PSN_IDX_NO1 DEFAULT NULL ON CONVERSION ERROR)) AS block_id, \
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
        // block_id -> name map (for labelling reconstructed yard cells)
        if let Some(bid) = r.block_id {
            sqlx::query(
                "INSERT INTO scenario.yard_block (block_id, block, updated_at) VALUES ($1,$2,now())
                 ON CONFLICT (block_id) DO UPDATE SET block=EXCLUDED.block, updated_at=now()",
            )
            .bind(bid as i32)
            .bind(block)
            .execute(&mut *tx)
            .await?;
        }
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
