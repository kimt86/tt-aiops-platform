//! Continuous incremental collector tick. Runs as its OWN systemd oneshot process, so any
//! failure here is fully isolated from the critical extractor services. Honors the kill
//! switch (scenario.config.enabled). A bad tick is recorded and returns Ok — never cascades.
//!
//! ★2026-08-06 (CHUNK 4-3) localized: JOB_ORDER_HISTORY is now read from the local mirror
//! `public.tos_handover_label` (landed by extractor::handover every 60s) instead of a direct
//! Oracle call — that table already carries the same JOBSTATUS='C' DS/LD completions this
//! stream needs (see mig0136/0137 for the vessel/voyage columns). Zero Oracle queries here now.
//! Watermark format (14-digit MYT text), scenario.watermark handling, and the move_hist landing
//! shape are all unchanged so downstream (yard-build/assemble/status) sees no difference.
//! Rows landed before the extractor's vessel/voyage columns existed have vessel/voyage = NULL —
//! expected, not a gap (see NULL-handling note in tick()).

use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::json;
use sqlx::PgPool;

use crate::state::{self, Config};
use crate::util::{is_wm_key, parse_block, parse_myt, wm_minus_secs};

const FETCH_CAP: i64 = 5000; // hard cap per tick; a 10-min DS/LD window is ~2k, so rarely binds
const INITIAL_LOOKBACK_MIN: i64 = 10; // first-ever tick: narrow window (low-load first touch)
/// Watermark safety lag (s) — kept from the Oracle-direct version even though the local mirror
/// is written inside a single committed transaction per handover.rs tick (no partial visibility):
/// the underlying JOB_ORDER_HISTORY rows can still land at Oracle out of (JOB_HIST_DATE||TIME)
/// order (~1 in 866k, measured), so re-reading a small tail costs nothing (ON CONFLICT dedups it)
/// and keeps this stream's own watermark treatment identical to before the localization.
const LAG_S: i64 = 120;

#[derive(Debug, sqlx::FromRow)]
struct HistRow {
    contno: String,
    jobtype: String,
    vessel: Option<String>,
    voyage: Option<String>,
    topos: Option<String>,
    machno: Option<String>,
    evt: String, // to_char(comp_ts, MYT) → "YYYYMMDDHHMMSS"
}

pub async fn run(pool: &PgPool, target: &str) -> Result<()> {
    let cfg = state::load_config(pool).await?;
    if !cfg.enabled {
        tracing::info!("scenario collection disabled (kill switch) — skipping tick");
        return Ok(());
    }

    let run_id = state::start_run(pool, "collect").await?;
    match tick(pool, run_id, target, &cfg).await {
        Ok(()) => {
            state::finish_run(pool, run_id, "done", None).await?;
        }
        Err(e) => {
            tracing::error!(error = %e, "scenario collect tick failed (isolated — others unaffected)");
            let _ = state::emit(pool, run_id, "error", "tick_failed", json!({ "error": e.to_string() })).await;
            let _ = state::finish_run(pool, run_id, "error", Some(&e.to_string())).await;
        }
    }
    Ok(()) // always Ok: non-critical subsystem must not cascade a failure
}

async fn tick(pool: &PgPool, run_id: i64, _target: &str, _cfg: &Config) -> Result<()> {
    state::set_phase(pool, run_id, "extract").await?;

    // Seek from (watermark − LAG_S) so rows that become visible out of key order can't be skipped;
    // ON CONFLICT dedups the re-read tail. Don't delete the watermark row — that forces a wider
    // re-read.
    //
    // The two fallbacks differ ON PURPOSE, and the distinction matters: no watermark at all means
    // this is the first-ever tick, where a narrow lookback is right (low-load first touch). But a
    // watermark that EXISTS and is unparseable must fall BACKWARD to day start — falling back to
    // "now − 10 min" there would silently skip everything in between, and this stream is what
    // attributes containers to vessels.
    let wm = match state::get_watermark(pool, "move_hist").await?.as_deref() {
        None => (tt_core::shift::terminal_now() - chrono::Duration::minutes(INITIAL_LOOKBACK_MIN))
            .format("%Y%m%d%H%M%S")
            .to_string(),
        Some(w) => wm_minus_secs(w, LAG_S).unwrap_or_else(|| {
            tracing::warn!(watermark = w, "malformed watermark — falling back to day start");
            format!("{}000000", tt_core::shift::terminal_now().format("%Y%m%d"))
        }),
    };
    let now_evt = tt_core::shift::terminal_now().format("%Y%m%d%H%M%S").to_string();
    let wm_ts = parse_myt(&wm).with_context(|| format!("bad watermark string: {wm}"))?;
    let now_ts = parse_myt(&now_evt).with_context(|| format!("bad now_evt string: {now_evt}"))?;

    // CHUNK 4-3: local mirror instead of Oracle. tos_handover_label is landed every 60s by
    // extractor::handover, already filtered to JOBSTATUS='C' DS/LD completions — same shape this
    // stream needs, zero Oracle round-trips. vessel/voyage are NULL on rows landed before the
    // extractor carried those columns (mig0136/0137 + handover.rs CHUNK 4-2) — expected, not a gap.
    let t0 = Instant::now();
    let rows: Vec<HistRow> = sqlx::query_as(
        "SELECT contno, jobtype, vessel, voyage, topos, armgc AS machno,
                to_char(comp_ts AT TIME ZONE 'Asia/Kuala_Lumpur', 'YYYYMMDDHH24MISS') AS evt
           FROM tos_handover_label
          WHERE comp_ts >= $1 AND comp_ts <= $2
          ORDER BY comp_ts
          LIMIT $3",
    )
    .bind(wm_ts)
    .bind(now_ts)
    .bind(FETCH_CAP)
    .fetch_all(pool)
    .await?;
    let query_ms = t0.elapsed().as_millis() as i64;
    let fetched = rows.len();

    let mut tx = pool.begin().await?;
    let mut max_evt: Option<String> = None;
    let (mut inserted, mut ds, mut ld) = (0u64, 0u64, 0u64);
    for r in &rows {
        let (contno, jobtype, evt) = (r.contno.as_str(), r.jobtype.as_str(), r.evt.as_str());
        let Some(comp_ts) = parse_myt(evt) else { continue };
        let res = sqlx::query(
            "INSERT INTO scenario.move_hist
               (comp_ts, contno, jobtype, vessel, voyage, yard_block, machno)
             VALUES ($1,$2,$3,$4,$5,$6,$7)
             ON CONFLICT (contno, comp_ts, jobtype) DO NOTHING",
        )
        .bind(comp_ts)
        .bind(contno.trim())
        .bind(jobtype.trim())
        .bind(r.vessel.as_deref().map(str::trim).filter(|s| !s.is_empty()))
        .bind(r.voyage.as_deref().map(str::trim).filter(|s| !s.is_empty()))
        .bind(r.topos.as_deref().and_then(parse_block))
        .bind(r.machno.as_deref().map(str::trim).filter(|s| !s.is_empty()))
        .execute(&mut *tx)
        .await?;
        inserted += res.rows_affected();
        match jobtype.trim() {
            "DS" => ds += 1,
            "LD" => ld += 1,
            _ => {}
        }
        // Advance ONLY on well-formed keys: a malformed one sorts out of order and could jump the
        // watermark ahead (skipping rows) or stall it.
        if is_wm_key(evt) && max_evt.as_deref().is_none_or(|m| evt > m) {
            max_evt = Some(evt.to_string());
        }
    }
    tx.commit().await?;

    if let Some(mx) = &max_evt {
        state::set_watermark(pool, "move_hist", mx).await?;
    }

    let capped = fetched as i64 >= FETCH_CAP;
    state::merge_json(pool, run_id, "load_stats", json!({
        "queries": 0,
        "oracle": false,
        "rows_read": fetched,
        "query_ms": query_ms,
        "rows_per_s": if query_ms > 0 { fetched as i64 * 1000 / query_ms } else { 0 },
        "fetch_capped": capped,
    })).await?;
    state::merge_json(pool, run_id, "collection", json!({
        "fetched": fetched, "inserted": inserted, "ds": ds, "ld": ld,
    })).await?;
    state::merge_json(pool, run_id, "progress", json!({
        "wm_from": wm, "wm_to": max_evt, "window_to": now_evt,
    })).await?;
    if capped {
        state::emit(pool, run_id, "warn", "fetch_capped", json!({
            "cap": FETCH_CAP, "note": "window exceeded cap; next tick continues from watermark",
        })).await?;
    }

    tracing::info!(fetched, inserted, ds, ld, query_ms, capped, "scenario move_hist tick");
    Ok(())
}
