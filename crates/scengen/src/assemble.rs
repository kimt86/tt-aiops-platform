//! On-demand assembly worker — LOCAL ONLY, ZERO Oracle. Claims pending scenario.assembly_job
//! rows and slices the local warehouse for the requested window:
//!   scenario  ← scenario.move_hist (window moves) + scenario.yard_snapshot (t=0 background)
//!   emulator  ← qc_move_log / rtg_move_log sliced to the window (period-accurate)
//! and writes scenario_out / emulator_out / summary back to the job.
//!
//! Container attributes + ship cells (from BAPLIE/MOVINS) and vessel size/berth are added once the
//! collector captures them — for now the scenario is move-level (contno/type/block/time) and the
//! emulator carries qc_move_s + yc_service (the two we can already derive locally).

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::state;

pub async fn run(pool: &PgPool) -> Result<()> {
    let mut done = 0u32;
    // Claim + process pending jobs one at a time until the queue drains.
    loop {
        let claimed: Option<(i64, DateTime<Utc>, DateTime<Utc>)> = sqlx::query_as(
            "UPDATE scenario.assembly_job SET state='running'
              WHERE job_id = (
                    SELECT job_id FROM scenario.assembly_job
                     WHERE state='pending' ORDER BY requested_at
                     LIMIT 1 FOR UPDATE SKIP LOCKED)
             RETURNING job_id, window_start, window_end",
        )
        .fetch_optional(pool)
        .await?;

        let Some((job_id, ws, we)) = claimed else { break };
        let run_id = state::start_run(pool, "assemble").await?;
        match assemble_one(pool, run_id, job_id, ws, we).await {
            Ok(summary) => {
                state::merge_json(pool, run_id, "collection", summary).await.ok();
                state::finish_run(pool, run_id, "done", None).await?;
                done += 1;
            }
            Err(e) => {
                tracing::error!(job_id, error = %e, "assemble failed");
                let _ = state::finish_run(pool, run_id, "error", Some(&e.to_string())).await;
                let _ = sqlx::query(
                    "UPDATE scenario.assembly_job SET state='error', error_text=$2, finished_at=now() WHERE job_id=$1",
                )
                .bind(job_id)
                .bind(e.to_string())
                .execute(pool)
                .await;
            }
        }
    }
    if done == 0 {
        tracing::info!("assemble: no pending jobs");
    } else {
        tracing::info!(jobs = done, "assemble: done");
    }
    Ok(())
}

async fn assemble_one(
    pool: &PgPool,
    run_id: i64,
    job_id: i64,
    ws: DateTime<Utc>,
    we: DateTime<Utc>,
) -> Result<Value> {
    state::set_phase(pool, run_id, "assemble").await?;

    // ---- SCENARIO: move-level, grouped by vessel/voyage (attrs/cells come later from BAPLIE/MOVINS)
    let moves: Vec<(DateTime<Utc>, String, String, Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as(
            "SELECT comp_ts, contno, jobtype, vessel, voyage, yard_block
               FROM scenario.move_hist
              WHERE comp_ts >= $1 AND comp_ts < $2
              ORDER BY vessel NULLS FIRST, voyage NULLS FIRST, comp_ts",
        )
        .bind(ws)
        .bind(we)
        .fetch_all(pool)
        .await?;

    let mut vessels: Vec<Value> = Vec::new();
    let (mut cur_key, mut cur_conts): (Option<(String, String)>, Vec<Value>) = (None, Vec::new());
    let (mut n_ds, mut n_ld) = (0u64, 0u64);
    for (comp_ts, contno, jobtype, vessel, voyage, block) in &moves {
        match jobtype.as_str() {
            "DS" => n_ds += 1,
            "LD" => n_ld += 1,
            _ => {}
        }
        let key = (
            vessel.clone().unwrap_or_default(),
            voyage.clone().unwrap_or_default(),
        );
        if cur_key.as_ref() != Some(&key) {
            if let Some((v, vy)) = cur_key.take() {
                vessels.push(json!({"vessel_id": v, "voyage": vy, "containers": cur_conts}));
                cur_conts = Vec::new();
            }
            cur_key = Some(key);
        }
        cur_conts.push(json!({
            "container_id": contno,
            "move_type": if jobtype == "DS" { "discharge" } else if jobtype == "LD" { "load" } else { jobtype.as_str() },
            "yard_slot": { "block": block },
            "move_ts": comp_ts.to_rfc3339(),
        }));
    }
    if let Some((v, vy)) = cur_key.take() {
        vessels.push(json!({"vessel_id": v, "voyage": vy, "containers": cur_conts}));
    }

    // ---- t=0 yard background: nearest snapshot at or before window start (fall back to earliest)
    let snap_ts: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT max(snapshot_ts) FROM scenario.yard_snapshot WHERE snapshot_ts <= $1")
            .bind(ws)
            .fetch_one(pool)
            .await?;
    let snap_ts = match snap_ts {
        Some(t) => Some(t),
        None => sqlx::query_scalar("SELECT min(snapshot_ts) FROM scenario.yard_snapshot")
            .fetch_one(pool)
            .await?,
    };
    let yard_t0 = if let Some(st) = snap_ts {
        let blocks: Vec<(String, i32, i32, i32, i32, i32)> = sqlx::query_as(
            "SELECT block, n_total, n_full, n_reefer, n_20ft, n_import
               FROM scenario.yard_snapshot WHERE snapshot_ts = $1 ORDER BY block",
        )
        .bind(st)
        .fetch_all(pool)
        .await?;
        let blocks_json: Vec<Value> = blocks
            .iter()
            .map(|(b, tot, full, rf, tw, imp)| {
                json!({"block": b, "n_total": tot, "n_full": full, "n_empty": tot - full,
                       "n_reefer": rf, "n_20ft": tw, "n_import": imp})
            })
            .collect();
        json!({"snapshot_ts": st.to_rfc3339(), "exact": st <= ws, "blocks": blocks_json})
    } else {
        Value::Null
    };

    let scenario_out = json!({
        "meta": { "window": [ws.to_rfc3339(), we.to_rfc3339()], "source": "scenario.move_hist" },
        "vessels": vessels,
        "yard_t0": yard_t0,
        "_note": "move-level; container attrs + ship cells + vessel size/berth added once collector captures BAPLIE/MOVINS/CDV_VESSEL/VSB_VOYAGE",
    });

    // ---- EMULATOR: qc_move_s + yc_service sliced to the window (period-accurate, zero Oracle)
    // Same sanity caps as the extractor (QC move [1,300]s, RTG service [0,1800]s) to drop
    // staging/wait outliers that would inflate a small-window median.
    let qc: Vec<(String, Option<f64>, i64)> = sqlx::query_as(
        "SELECT jobtype, percentile_cont(0.5) WITHIN GROUP (ORDER BY dur_s), count(*)
           FROM qc_move_log
          WHERE comp_ts >= $1 AND comp_ts < $2 AND dur_s BETWEEN 1 AND 300 AND jobtype IN ('DS','LD')
          GROUP BY jobtype",
    )
    .bind(ws).bind(we).fetch_all(pool).await?;

    let yc: Vec<(String, Option<f64>, Option<f64>, Option<f64>, i64)> = sqlx::query_as(
        "SELECT jobtype,
                percentile_cont(0.1) WITHIN GROUP (ORDER BY dur_s),
                percentile_cont(0.5) WITHIN GROUP (ORDER BY dur_s),
                percentile_cont(0.9) WITHIN GROUP (ORDER BY dur_s), count(*)
           FROM rtg_move_log
          WHERE comp_ts >= $1 AND comp_ts < $2 AND dur_s BETWEEN 0 AND 1800 AND jobtype IN ('DS','LD')
          GROUP BY jobtype",
    )
    .bind(ws).bind(we).fetch_all(pool).await?;

    let qc_med = |jt: &str| qc.iter().find(|r| r.0 == jt).and_then(|r| r.1).map(|v| v.round() as i64);
    let qc_n = |jt: &str| qc.iter().find(|r| r.0 == jt).map(|r| r.2).unwrap_or(0);
    let yc_p = |jt: &str| -> Value {
        match yc.iter().find(|r| r.0 == jt) {
            Some((_, p10, p50, p90, _)) => json!([
                p10.map(|v| v.round() as i64), p50.map(|v| v.round() as i64), p90.map(|v| v.round() as i64)
            ]),
            None => Value::Null,
        }
    };
    let yc_n = |jt: &str| yc.iter().find(|r| r.0 == jt).map(|r| r.4).unwrap_or(0);

    let emulator_out = json!({
        "qc_move_s":  { "ds": qc_med("DS"), "ld": qc_med("LD") },
        "yc_service": { "ds": yc_p("DS"), "ld": yc_p("LD") },
        "hatch_s": Value::Null, "bay_change_s": Value::Null,
        "twin_ratio": Value::Null, "drive_speed_ms": Value::Null,
        "_provenance": {
            "window": [ws.to_rfc3339(), we.to_rfc3339()],
            "qc_sample": { "ds": qc_n("DS"), "ld": qc_n("LD") },
            "yc_sample": { "ds": yc_n("DS"), "ld": yc_n("LD") },
            "note": "qc_move_s/yc_service from local qc_move_log/rtg_move_log sliced to window (capped QC[1,300]s/RTG[0,1800]s); thin on small windows. hatch/bay_change/twin/drive TODO",
        },
    });

    let summary = json!({
        "vessels": vessels.len(), "containers": moves.len(), "ds": n_ds, "ld": n_ld,
        "qc_sample": qc_n("DS") + qc_n("LD"), "yc_sample": yc_n("DS") + yc_n("LD"),
    });

    sqlx::query(
        "UPDATE scenario.assembly_job
            SET state='done', scenario_out=$2::jsonb, emulator_out=$3::jsonb,
                summary=$4::jsonb, finished_at=now()
          WHERE job_id=$1",
    )
    .bind(job_id)
    .bind(scenario_out.to_string())
    .bind(emulator_out.to_string())
    .bind(summary.to_string())
    .execute(pool)
    .await?;

    tracing::info!(job_id, vessels = vessels.len(), containers = moves.len(), "assembled");
    Ok(summary)
}
