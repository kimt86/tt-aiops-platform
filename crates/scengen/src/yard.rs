//! Yard-crane (RTG + ES) move stream WITH decoded stack position -> scenario.yard_move. The event
//! source for the incremental yard-state model. Watermark-incremental over MCH_OPERATION, SEEKing
//! the PK on MCH_OPER_SEQNO (see the query comment). CRNT_PSN_IDX decode (verified vs CYY.CLOCATION):
//!   block_id=NO1 · bay_idx=NO2 · row_idx=NO3(A=0) · tier=NO4+1
//! Isolated + kill-switch, like the other collectors.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::PgPool;
use wp_core::parse::parse_rows;

use crate::state::{self, Config};
use crate::toolbox::Toolbox;
use crate::util::{jstr, parse_myt};

const STREAM: &str = "yard_move";
const FETCH_CAP: u32 = 8000;
/// Watermark safety lag (s) — covers TOS out-of-order row visibility. See util::wm_minus_secs.
const LAG_S: i64 = 120;

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

    // Seek from (watermark − LAG_S), not from the watermark itself — see util::wm_minus_secs.
    // Re-reading ~2 min of tail (~90 RTG+ES rows) is free and ON CONFLICT dedups it. This cannot
    // stall: 2 min of moves is orders of magnitude under FETCH_CAP.
    // NOTE: deleting the scenario.watermark row makes this fall back to a day-start seek (bounded
    // by the cap and self-catching-up, but a needless rescan) — don't delete it.
    let day = wp_core::shift::terminal_now().format("%Y%m%d").to_string();
    let wm = state::get_watermark(pool, STREAM)
        .await?
        .as_deref()
        .and_then(|w| crate::util::wm_minus_secs(w, LAG_S))
        .unwrap_or_else(|| format!("{day}000000"));

    // SEEK, not rescan (mirrors extractor::rtg_moves). MCH_OPER_SEQNO ("YYYYMMDDHHMMSS", globally
    // monotonic completion order) is the LEADING column of PK MCH_PK_OPERATION, so `SEQNO >= wm`
    // seeks via the PK — the INDEX hint pins it, ORDER BY SEQNO is then free, and FETCH stops early
    // (so backlog catch-up is safe too). The previous form (COMPDATE = today AND the concatenated
    // COMPDATE||COMPTIME > wm) could only range-scan the whole elapsed day and filter, and it lost
    // rows two ways: `>` skipped same-second rows whenever the cap truncated (same second holds up
    // to 6 moves here), and `COMPDATE = today` dropped the pre-midnight tail every night. `>=` plus
    // ON CONFLICT dedups the tiny re-read losslessly. REGEXP '^(RTG|ES)' covers BOTH yard-crane
    // families — the old LIKE 'RTG%' silently missed all 18 ES machines.
    let sql = format!(
        "SELECT /*+ INDEX(MCH_OPERATION MCH_PK_OPERATION) */
                MCH_OPER_MACHNO AS machno, SUBSTR(MCH_OPER_CONTNO,1,11) AS contno,
                MCH_OPER_SEQNO AS seqno, MCH_OPER_JOBTYPE AS jt,
                MCH_OPER_COMPDATE||MCH_OPER_COMPTIME AS comp,
                CRNT_PSN_IDX_NO1 AS b, CRNT_PSN_IDX_NO2 AS y,
                CRNT_PSN_IDX_NO3 AS rr, CRNT_PSN_IDX_NO4 AS tt
           FROM TOSADM.MCH_OPERATION
          WHERE MCH_OPER_SEQNO >= '{wm}'
            AND REGEXP_LIKE(MCH_OPER_MACHNO, '^(RTG|ES)')
            AND CRNT_PSN_IDX_NO1 IS NOT NULL
            AND LENGTH(MCH_OPER_COMPTIME) >= 6
          ORDER BY MCH_OPER_SEQNO
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
    let mut max_seq: Option<String> = None;
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
        // Watermark advances on SEQNO (the seek key), not comp — they are not the same ordering —
        // and ONLY on well-formed keys, so a malformed SEQNO can never jump or stall it.
        if crate::util::is_wm_key(&seqno) && max_seq.as_deref().is_none_or(|m| seqno.as_str() > m) {
            max_seq = Some(seqno);
        }
    }
    tx.commit().await?;

    if let Some(mx) = &max_seq {
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

// ---- Phase 2: reconstruct scenario.yard_cell by replaying yard_move incrementally (LOCAL, no Oracle).

const BUILD_BATCH: i64 = 20000;

/// Apply new yard_move rows (since the reconstruction watermark) to scenario.yard_cell.
/// Not gated on the kill switch — it's local processing and should finish even if collection paused.
pub async fn build(pool: &PgPool) -> Result<()> {
    let run_id = state::start_run(pool, "yard_build").await?;
    match do_build(pool, run_id).await {
        Ok(()) => {
            state::finish_run(pool, run_id, "done", None).await?;
        }
        Err(e) => {
            tracing::error!(error = %e, "scenario yard-build failed (isolated — others unaffected)");
            let _ = state::finish_run(pool, run_id, "error", Some(&e.to_string())).await;
        }
    }
    Ok(())
}

async fn do_build(pool: &PgPool, run_id: i64) -> Result<()> {
    state::set_phase(pool, run_id, "assemble").await?;
    let wm = state::get_watermark(pool, "yard_cell").await?; // ISO text of last processed comp_ts

    // `>=`, NOT `>`: comp_ts is non-unique (up to ~6 moves share one second), so if LIMIT truncates
    // mid-second a strict `>` would permanently skip that second's remaining rows on the next run —
    // the same defect we fixed on the Oracle side. It only bites during catch-up (after downtime or
    // a backfill), and a lost move corrupts the stack state forever. Re-applying the boundary row is
    // harmless because replay is idempotent: PLACE = delete-by-contno then upsert the cell, and
    // REMOVE = delete. Cannot livelock (one second's rows are far under LIMIT).
    let moves: Vec<(DateTime<Utc>, String, String, i32, i32, i32, i32)> = sqlx::query_as(
        "SELECT comp_ts, contno, jobtype, block_id, bay_idx, row_idx, tier
           FROM scenario.yard_move
          WHERE ($1::timestamptz IS NULL OR comp_ts >= $1::timestamptz)
          ORDER BY comp_ts, seqno
          LIMIT $2",
    )
    .bind(wm.as_deref())
    .bind(BUILD_BATCH)
    .fetch_all(pool)
    .await?;

    let mut tx = pool.begin().await?;
    let (mut placed, mut removed, mut seeded, mut skipped) = (0u64, 0u64, 0u64, 0u64);
    let mut last: Option<DateTime<Utc>> = None;
    for (ts, contno, jt, b, y, ri, tier) in &moves {
        match jt.as_str() {
            // PLACE: container ends occupying this cell (DS/GI discharge/gate-in, RH/AH rehandle, MI 구내이적 입고)
            "DS" | "GI" | "RH" | "AH" | "MI" => {
                // relocation: drop this container's previous cell if we tracked it
                sqlx::query("DELETE FROM scenario.yard_cell WHERE contno=$1")
                    .bind(contno)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query(
                    "INSERT INTO scenario.yard_cell (block_id,bay_idx,row_idx,tier,contno,known,updated_ts)
                     VALUES ($1,$2,$3,$4,$5,true,$6)
                     ON CONFLICT (block_id,bay_idx,row_idx,tier)
                     DO UPDATE SET contno=EXCLUDED.contno, known=true, updated_ts=EXCLUDED.updated_ts",
                )
                .bind(b).bind(y).bind(ri).bind(tier).bind(contno).bind(ts)
                .execute(&mut *tx)
                .await?;
                // seed "unknown" placeholders on tiers 1..tier-1 (only where currently empty)
                if *tier > 1 {
                    let r = sqlx::query(
                        "INSERT INTO scenario.yard_cell (block_id,bay_idx,row_idx,tier,contno,known,updated_ts)
                         SELECT $1,$2,$3,g,NULL,false,$4 FROM generate_series(1,$5) g
                         ON CONFLICT (block_id,bay_idx,row_idx,tier) DO NOTHING",
                    )
                    .bind(b).bind(y).bind(ri).bind(ts).bind(tier - 1)
                    .execute(&mut *tx)
                    .await?;
                    seeded += r.rows_affected();
                }
                placed += 1;
            }
            // REMOVE: container vacates the cell (LD/GO load/gate-out, MO 구내이적 출고). If the cell was an
            // "unknown", this move's contno reveals what it was — but it's leaving, so just clear it.
            "LD" | "GO" | "MO" => {
                sqlx::query(
                    "DELETE FROM scenario.yard_cell
                      WHERE contno=$1 OR (block_id=$2 AND bay_idx=$3 AND row_idx=$4 AND tier=$5)",
                )
                .bind(contno).bind(b).bind(y).bind(ri).bind(tier)
                .execute(&mut *tx)
                .await?;
                removed += 1;
            }
            _ => skipped += 1, // GC/LC etc. — rare, unclassified
        }
        last = Some(*ts);
    }
    tx.commit().await?;

    if let Some(l) = last {
        state::set_watermark(pool, "yard_cell", &l.to_rfc3339()).await?;
    }

    state::merge_json(pool, run_id, "collection", json!({
        "processed": moves.len(), "placed": placed, "removed": removed,
        "seeded_unknown": seeded, "skipped": skipped,
    })).await?;
    tracing::info!(processed = moves.len(), placed, removed, seeded, skipped, "scenario yard build");
    Ok(())
}

/// One reconstructed cell: (block_id, bay_idx, row_idx, tier, contno-or-none, known).
pub type Cell = (i32, i32, i32, i32, Option<String>, bool);

/// Reconstruct the yard state AS OF `at` by replaying scenario.yard_move up to that instant
/// (in-memory, LOCAL, zero Oracle). Used for a scenario's t=0 background — the state at the
/// window START, not "now". Same place/remove/seed rules as the live yard-build. Coverage grows
/// with collection (containers not yet observed aren't included until they move).
///
/// Returns (cells, moves_replayed).
///
/// ⚠ COST GROWS WITHOUT BOUND. This replays the ENTIRE history up to `at` on every call, and every
/// download calls it synchronously. At ~35k moves/day a month of accumulation is ~1M rows replayed
/// per request. The fix is a periodic cell CHECKPOINT so only the delta since the checkpoint is
/// replayed — not done here because it needs its own design. `moves_replayed` is surfaced in the
/// scenario JSON and warned on so this degrades visibly rather than silently.
///
/// ⚠ Also note this derives state ONLY from yard_move, while the live `yard_cell` accumulates
/// persistently. If yard_move is ever pruned (scenario.config.retention_days), the two silently
/// diverge: past-window yard_t0 goes wrong while the live cell state stays right.
pub async fn state_as_of(pool: &PgPool, at: DateTime<Utc>) -> Result<(Vec<Cell>, usize)> {
    use std::collections::HashMap;
    let moves: Vec<(String, String, i32, i32, i32, i32)> = sqlx::query_as(
        "SELECT contno, jobtype, block_id, bay_idx, row_idx, tier
           FROM scenario.yard_move WHERE comp_ts <= $1 ORDER BY comp_ts, seqno",
    )
    .bind(at)
    .fetch_all(pool)
    .await?;

    let replayed = moves.len();
    if replayed > 250_000 {
        tracing::warn!(replayed, "state_as_of replay is large — needs a cell checkpoint");
    }
    let mut cells: HashMap<(i32, i32, i32, i32), (Option<String>, bool)> = HashMap::new();
    let mut where_is: HashMap<String, (i32, i32, i32, i32)> = HashMap::new();
    for (contno, jt, b, y, ri, tier) in moves {
        match jt.as_str() {
            "DS" | "GI" | "RH" | "AH" | "MI" => {
                if let Some(old) = where_is.remove(&contno) {
                    cells.remove(&old);
                }
                let key = (b, y, ri, tier);
                cells.insert(key, (Some(contno.clone()), true));
                where_is.insert(contno, key);
                for t in 1..tier {
                    cells.entry((b, y, ri, t)).or_insert((None, false));
                }
            }
            "LD" | "GO" | "MO" => {
                if let Some(old) = where_is.remove(&contno) {
                    cells.remove(&old);
                }
                cells.remove(&(b, y, ri, tier));
            }
            _ => {}
        }
    }
    Ok((
        cells
            .into_iter()
            .map(|((b, y, ri, t), (c, k))| (b, y, ri, t, c, k))
            .collect(),
        replayed,
    ))
}
