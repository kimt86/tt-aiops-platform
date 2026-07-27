//! Landside gate collector: TOSADM.CYC_HISTORY gate transactions -> scenario.gate_event.
//!
//! This is the one collector whose watermark is over OUR OWN table, not over an Oracle key, and the
//! reason is in the source: CYC_HISTORY has no time-leading index (its three indexes lead with
//! CONTNO or SITUATION), so the usual "seek forward by timestamp" is impossible. It does not need to
//! be — we already know from the local yard stream exactly which containers crossed the gate, and
//! CONTNO is the PK's leading column. So we walk our own rtg_move_log GI/GO moves forward and look
//! those containers up by number: an index seek per container, batched.
//!
//! Measured cost: 1,000 containers per query ~1.7 s of Oracle time (100 -> 0.38 s). At ~8,600 gate
//! moves a day and a 15-minute cadence that is ~90 containers, i.e. under half a second per tick —
//! lighter than the collectors that do seek by time.
//!
//! We store ONLY the three events the yard stream cannot give us:
//!   import  GIY -> (YGY)          GIY = gate transaction; YGY is a GI move's comp_ts, already ours
//!   export  QYG -> (OYG) -> GOY   QYG = gate transaction; OYG is a GO move's comp_ts, already ours
//! Note QYG, not OYG, is the export intake — OYG is the yard crane lifting onto the truck.
//!
//! LAG_MIN exists because of GOY: a truck leaves a median 15.6 min (p90 25.3) AFTER the yard pick,
//! so querying a container the moment we see its move would miss its exit permanently. We stay that
//! far behind instead of re-querying, which keeps the pass single and the cost flat.

use std::time::Instant;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::state::{self, Config};
use crate::toolbox::Toolbox;
use crate::util::{jstr, parse_myt};

const STREAM: &str = "gate_event";
/// Containers per tick. Oracle caps an IN list at 1000 expressions, and 1000 is also where the
/// measured query time flattens out (~1.7 s), so this is both the hard limit and the good one.
const BATCH: i64 = 1000;
/// Stay this far behind live so a truck's gate exit has been written before we look. p90 of
/// yard-pick -> exit is 25 min; 60 gives room without making the feed feel stale.
const LAG_MIN: i64 = 60;
/// Gate transactions only. YGY/OYG are deliberately excluded — they duplicate rtg_move_log.comp_ts.
const SITUATIONS: &str = "'GIY','QYG','GOY'";

pub async fn run(pool: &PgPool, target: &str) -> Result<()> {
    let cfg = state::load_config(pool).await?;
    if !cfg.enabled {
        tracing::info!("scenario collection disabled (kill switch) — skipping gate");
        return Ok(());
    }
    let run_id = state::start_run(pool, "gate").await?;
    match tick(pool, run_id, target, &cfg).await {
        Ok(()) => {
            state::finish_run(pool, run_id, "done", None).await?;
        }
        Err(e) => {
            tracing::error!(error = %e, "scenario gate failed (isolated — others unaffected)");
            let _ = state::emit(pool, run_id, "error", "gate_failed", json!({ "error": e.to_string() })).await;
            let _ = state::finish_run(pool, run_id, "error", Some(&e.to_string())).await;
        }
    }
    Ok(()) // always Ok: non-critical subsystem must not cascade
}

async fn tick(pool: &PgPool, run_id: i64, target: &str, cfg: &Config) -> Result<()> {
    state::set_phase(pool, run_id, "extract").await?;

    // Watermark is a local timestamp: how far along rtg_move_log we have driven. With no watermark
    // we start at the beginning of the local stream and let the timer pace the catch-up — the gate
    // detail is worth having for every window a scenario can be built for.
    let wm: Option<DateTime<Utc>> = state::get_watermark(pool, STREAM)
        .await?
        .and_then(|w| DateTime::parse_from_rfc3339(&w).ok())
        .map(|t| t.with_timezone(&Utc));

    // Take the EARLIEST moves — NOT the alphabetically-first containers. The watermark advances to
    // the last comp_ts we looked at, so this walk has to be in time order; ordering by container
    // instead would march the watermark to "now" while leaving everything behind it unqueried.
    // `>=`, not `>`: comp_ts is not unique and a batch boundary can land inside a second. Redoing
    // the boundary second is free (ON CONFLICT dedups) and skipping it would be silent loss.
    let moves: Vec<(String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT contno, comp_ts
           FROM rtg_move_log
          WHERE jobtype IN ('GI','GO')
            AND ($1::timestamptz IS NULL OR comp_ts >= $1::timestamptz)
            AND comp_ts < now() - ($2 || ' minutes')::interval
          ORDER BY comp_ts
          LIMIT $3",
    )
    .bind(wm)
    .bind(LAG_MIN.to_string())
    .bind(BATCH)
    .fetch_all(pool)
    .await?;

    // A container can appear twice in one batch (in and back out); one lookup covers both.
    let mut seen = std::collections::HashSet::new();
    let due: Vec<(String, DateTime<Utc>)> = moves
        .iter()
        .filter(|(c, _)| seen.insert(c.clone()))
        .cloned()
        .collect();

    if due.is_empty() {
        state::merge_json(pool, run_id, "collection", json!({ "fetched": 0, "inserted": 0 })).await?;
        tracing::info!("scenario gate: nothing due");
        return Ok(());
    }

    // Bound the CYC_HISTORY scan per container: a box that has been through here for years carries
    // every past visit. The oldest move in this batch is the earliest gate event we could want.
    let since = due.iter().map(|(_, t)| *t).min().unwrap_or_else(Utc::now);
    let from_date = (since - chrono::Duration::days(2))
        .with_timezone(&tt_core::shift::terminal_offset()) // CYC_HIST_DATE is terminal-local
        .format("%Y%m%d")
        .to_string();
    // Container numbers come from our own table and are matched against a strict charset before
    // they are ever spliced into SQL.
    let list: Vec<String> = due
        .iter()
        .map(|(c, _)| c.as_str())
        .filter(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_alphanumeric()))
        .map(|c| format!("'{c}'"))
        .collect();
    if list.is_empty() {
        anyhow::bail!("gate: no well-formed container numbers in a batch of {}", due.len());
    }
    let in_list = list.join(",");

    let sql = format!(
        "SELECT CYC_HIST_CONTNO AS contno, CYC_HIST_POINT AS visit,
                CYC_HIST_SITUATION AS sit, CYC_HIST_DATE||CYC_HIST_TIME AS evt,
                CYC_HIST_MACHINEID AS machine, CYC_HIST_REGONO AS truck,
                CYC_HIST_USERID AS clerk,
                HIS_PSN_IDX_NO1 AS b, HIS_PSN_IDX_NO2 AS y,
                HIS_PSN_IDX_NO3 AS rr, HIS_PSN_IDX_NO4 AS tt
           FROM TOSADM.CYC_HISTORY
          WHERE CYC_HIST_CONTNO IN ({in_list})
            AND CYC_HIST_SITUATION IN ({SITUATIONS})
            AND CYC_HIST_DATE >= '{from_date}'"
    );

    let t0 = Instant::now();
    let raw = Toolbox::from_env(target, cfg.oracle_timeout_s as u64)?
        .run_sql(&sql)
        .await?;
    let query_ms = t0.elapsed().as_millis() as i64;
    let rows: Vec<serde_json::Value> = parse_rows(&raw)?;
    let fetched = rows.len();

    let num = |r: &serde_json::Value, k: &str| jstr(r, k).and_then(|s| s.parse::<i32>().ok());

    let mut tx = pool.begin().await?;
    let (mut inserted, mut skipped) = (0u64, 0u64);
    for r in &rows {
        let (Some(contno), Some(sit), Some(evt)) = (jstr(r, "CONTNO"), jstr(r, "SIT"), jstr(r, "EVT"))
        else {
            skipped += 1;
            continue;
        };
        let (Some(visit), Some(event_ts)) = (num(r, "VISIT"), parse_myt(&evt)) else {
            skipped += 1;
            continue;
        };
        let direction = if sit.trim() == "GIY" { "in" } else { "out" };
        // Position is present on the gate record for the assigned/current slot and absent at the
        // exit; store it only when the whole index decodes, same rule as the yard collector.
        let (b, y, ri, tt) = (num(r, "B"), num(r, "Y"), num(r, "RR"), num(r, "TT"));
        let res = sqlx::query(
            "INSERT INTO scenario.gate_event
               (contno, visit, situation, direction, event_ts, machine, truck_reg, clerk,
                block_id, bay_idx, row_idx, tier)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
             ON CONFLICT (contno, visit, situation) DO NOTHING",
        )
        .bind(contno.trim())
        .bind(visit)
        .bind(sit.trim())
        .bind(direction)
        .bind(event_ts)
        .bind(jstr(r, "MACHINE"))
        .bind(jstr(r, "TRUCK"))
        .bind(jstr(r, "CLERK"))
        .bind(b)
        .bind(y)
        .bind(ri)
        .bind(tt.map(|t| t + 1)) // tier = NO4 + 1
        .execute(&mut *tx)
        .await?;
        inserted += res.rows_affected();
    }
    tx.commit().await?;

    // Advance over every move we walked (not just the deduped lookups), because the walk is what
    // the watermark tracks. A container with no gate record (rare — measured 99.9% matched) must
    // not hold it back, or the stream would stall on that one box for good.
    let advanced = moves.iter().map(|(_, t)| *t).max();
    if let Some(mx) = advanced {
        state::set_watermark(pool, STREAM, &mx.to_rfc3339()).await?;
    }

    let capped = moves.len() as i64 >= BATCH;
    state::merge_json(pool, run_id, "load_stats", json!({
        "queries": 1, "rows_read": fetched, "query_ms": query_ms, "fetch_capped": capped,
    })).await?;
    state::merge_json(pool, run_id, "collection", json!({
        "moves": moves.len(), "containers": due.len(), "fetched": fetched,
        "inserted": inserted, "skipped": skipped,
    })).await?;
    state::merge_json(pool, run_id, "progress", json!({
        "wm_to": advanced.map(|t| t.to_rfc3339()), "lag_min": LAG_MIN,
    })).await?;
    if capped {
        state::emit(pool, run_id, "warn", "batch_capped", json!({
            "cap": BATCH, "note": "backlog exceeded the batch; next tick continues from watermark",
        })).await?;
    }

    tracing::info!(
        moves = moves.len(), containers = due.len(), fetched, inserted, skipped, query_ms, capped,
        "scenario gate"
    );
    Ok(())
}
