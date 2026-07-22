//! Continuous incremental collector tick. Runs as its OWN systemd oneshot process, so any
//! failure here is fully isolated from the critical extractor services. Honors the kill
//! switch (scenario.config.enabled). A bad tick is recorded and returns Ok — never cascades.
//!
//! Reuses the extractor's proven watermark-incremental pattern (see extractor::handover):
//! a bounded, index-supported range scan of JOB_ORDER_HISTORY on (JOB_HIST_DATE||JOB_HIST_TIME),
//! FETCH-capped, via the isolated toolbox (short timeout). First-ever tick pulls only the last
//! ~10 min, so the first real Oracle touch is tiny.

use std::time::Instant;

use anyhow::Result;
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;
use wp_core::parse::parse_rows;

use crate::state::{self, Config};
use crate::toolbox::Toolbox;
use crate::util::{is_wm_key, parse_block, parse_myt, wm_minus_secs};

const FETCH_CAP: u32 = 5000; // hard cap per tick; a 10-min DS/LD window is ~2k, so rarely binds
const INITIAL_LOOKBACK_MIN: i64 = 10; // first-ever tick: narrow window (low-load first touch)
/// Watermark safety lag (s) — covers TOS out-of-order row visibility. See util::wm_minus_secs.
const LAG_S: i64 = 120;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
struct HistRow {
    contno: Option<String>,
    jobtype: Option<String>,
    vessel: Option<String>,
    voyage: Option<String>,
    topos: Option<String>,
    machno: Option<String>,
    evt: Option<String>, // JOB_HIST_DATE||JOB_HIST_TIME (MYT "YYYYMMDDHHMMSS")
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

async fn tick(pool: &PgPool, run_id: i64, target: &str, cfg: &Config) -> Result<()> {
    state::set_phase(pool, run_id, "extract").await?;

    // Seek from (watermark − LAG_S) so rows that become visible out of key order can't be skipped;
    // ON CONFLICT dedups the re-read tail. A 14-digit bound correctly bounds these 17-digit
    // (millisecond) keys. Don't delete the watermark row — that forces a wider re-read.
    //
    // The two fallbacks differ ON PURPOSE, and the distinction matters: no watermark at all means
    // this is the first-ever tick, where a narrow lookback is right (low-load first touch). But a
    // watermark that EXISTS and is unparseable must fall BACKWARD to day start — falling back to
    // "now − 10 min" there would silently skip everything in between, and this stream is what
    // attributes containers to vessels.
    let wm = match state::get_watermark(pool, "move_hist").await?.as_deref() {
        None => (wp_core::shift::terminal_now() - chrono::Duration::minutes(INITIAL_LOOKBACK_MIN))
            .format("%Y%m%d%H%M%S")
            .to_string(),
        Some(w) => wm_minus_secs(w, LAG_S).unwrap_or_else(|| {
            tracing::warn!(watermark = w, "malformed watermark — falling back to day start");
            format!("{}000000", wp_core::shift::terminal_now().format("%Y%m%d"))
        }),
    };
    let now_evt = wp_core::shift::terminal_now().format("%Y%m%d%H%M%S").to_string();

    // Index-supported range scan on the PK (JOB_HIST_DATE, JOB_HIST_TIME). Completed DS/LD only.
    let sql = format!(
        "SELECT JOB_HIST_CONTNO AS contno, JOB_HIST_JOBTYPE AS jobtype,
                JOB_HIST_VESSEL AS vessel, JOB_HIST_VOYAGE AS voyage,
                SUBSTR(JOB_HIST_YT_TOPOS,1,40) AS topos, JOB_HIST_ARMGC AS machno,
                JOB_HIST_DATE||JOB_HIST_TIME AS evt
           FROM TOSADM.JOB_ORDER_HISTORY
          WHERE JOB_HIST_DATE||JOB_HIST_TIME >= '{wm}'
            AND JOB_HIST_DATE||JOB_HIST_TIME <= '{now_evt}'
            AND JOB_HIST_JOBSTATUS = 'C'
            AND JOB_HIST_JOBTYPE IN ('DS','LD')
          ORDER BY JOB_HIST_DATE||JOB_HIST_TIME
          FETCH FIRST {FETCH_CAP} ROWS ONLY"
    );

    let t0 = Instant::now();
    let raw = Toolbox::from_env(target, cfg.oracle_timeout_s as u64)?
        .run_sql(&sql)
        .await?;
    let query_ms = t0.elapsed().as_millis() as i64;
    let rows: Vec<HistRow> = parse_rows(&raw)?;
    let fetched = rows.len();

    let mut tx = pool.begin().await?;
    let mut max_evt: Option<String> = None;
    let (mut inserted, mut ds, mut ld) = (0u64, 0u64, 0u64);
    for r in &rows {
        let (Some(contno), Some(jobtype), Some(evt)) =
            (r.contno.as_deref(), r.jobtype.as_deref(), r.evt.as_deref())
        else {
            continue;
        };
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

    let capped = fetched as u32 >= FETCH_CAP;
    state::merge_json(pool, run_id, "load_stats", json!({
        "queries": 1,
        "rows_read": fetched,
        "query_ms": query_ms,
        "rows_per_s": if query_ms > 0 { fetched as i64 * 1000 / query_ms } else { 0 },
        "oracle_timeout_s": cfg.oracle_timeout_s,
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
