//! Scenario/emulator assembly — LOCAL ONLY, ZERO Oracle. `build()` produces the scenario +
//! emulator JSON for a window by slicing the local warehouse:
//!   scenario  ← move_hist (window) ⨝ container (attrs+ship cell) ⨝ vessel_call (size+berth)
//!               + yard_snapshot (t=0 background)
//!   emulator  ← qc_move_log / rtg_move_log sliced to the window (period-accurate)
//! The web service calls build() synchronously for on-demand download; the `assemble` worker
//! calls it for queued assembly_job rows. Containers not yet enriched come through move-level.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::state;

/// Build (scenario_out, emulator_out, summary) for [ws, we). Pure — no job/run side effects.
pub async fn build(pool: &PgPool, ws: DateTime<Utc>, we: DateTime<Utc>) -> Result<(Value, Value, Value)> {
    // ---- SCENARIO vessels: window moves enriched with container attrs + ship cell + vessel_call.
    let vessels_txt: Option<String> = sqlx::query_scalar(
        r#"SELECT coalesce(jsonb_agg(vobj ORDER BY sp NULLS LAST), '[]'::jsonb)::text FROM (
             SELECT vc.startpos_m AS sp, jsonb_build_object(
               'vessel_id', cs.vessel, 'voyage', cs.voyage, 'vsl_name', vc.vsl_name,
               'loa_m', vc.loa_m, 'beam_m', vc.beam_m, 'total_bays', vc.total_bays,
               'berth', jsonb_build_object('berthno', vc.berthno, 'side', vc.berthside, 'startpos_m', vc.startpos_m),
               'schedule', jsonb_build_object('berth_ts', vc.actber, 'depart_ts', vc.actdep, 'cutoff_ts', vc.cutoff),
               'containers', cs.containers
             ) AS vobj
             FROM (
               SELECT m.vessel, m.voyage, jsonb_agg(jsonb_build_object(
                        'container_id', m.contno,
                        'move_type', CASE m.jobtype WHEN 'DS' THEN 'discharge' WHEN 'LD' THEN 'load' ELSE m.jobtype END,
                        'move_ts', m.comp_ts, 'yard_slot', jsonb_build_object('block', m.yard_block),
                        'iso', c.iso, 'size', c.size, 'height', c.height, 'family', c.family, 'fill', c.fill,
                        'gross_kg', c.gross_kg, 'reefer_temp', c.reefer_temp, 'imdg', c.imdg, 'un_no', c.un_no, 'oog', c.oog,
                        'pod', c.pod, 'pol', c.pol, 'operator', c.operator,
                        'ship_cell', CASE WHEN c.ship_bay IS NOT NULL THEN jsonb_build_object(
                             'bay', c.ship_bay, 'row', c.ship_row, 'tier', c.ship_tier,
                             'deck_hold', CASE WHEN c.ship_tier >= 80 THEN 'deck' ELSE 'hold' END) END,
                        'out_vessel', c.out_vessel, 'out_voyage', c.out_voyage
                      ) ORDER BY m.comp_ts) AS containers
                 FROM scenario.move_hist m
                 LEFT JOIN scenario.container c
                   ON c.vessel = m.vessel AND c.voyage = m.voyage AND c.contno = m.contno
                  AND c.disload = CASE m.jobtype WHEN 'DS' THEN 'D' WHEN 'LD' THEN 'L' ELSE m.jobtype END
                WHERE m.comp_ts >= $1 AND m.comp_ts < $2
                GROUP BY m.vessel, m.voyage
             ) cs
             LEFT JOIN scenario.vessel_call vc ON vc.vessel = cs.vessel AND vc.voyage = cs.voyage
           ) t"#,
    )
    .bind(ws).bind(we)
    .fetch_one(pool)
    .await?;
    let vessels: Value = serde_json::from_str(&vessels_txt.unwrap_or_else(|| "[]".into()))?;

    // ---- t=0 yard background: per-container stack state reconstructed AS OF the window start `ws`
    // (replay scenario.yard_move up to ws — the yard as it was at the scenario START, not "now").
    // RH/AH/MI/MO are used here (yard-state only) but are NOT in the work list. Converging: covers
    // containers observed since collection began; the rest fill in as they move (~a month).
    let cells = crate::yard::state_as_of(pool, ws).await?;
    let blkmap: std::collections::HashMap<i32, String> =
        sqlx::query_as::<_, (i32, String)>("SELECT block_id, block FROM scenario.yard_block")
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();
    let mut summ: std::collections::HashMap<i32, (i64, i64)> = std::collections::HashMap::new();
    let mut cells_json: Vec<Value> = Vec::with_capacity(cells.len());
    for (b, y, ri, t, cont, known) in &cells {
        let e = summ.entry(*b).or_insert((0, 0));
        if *known { e.0 += 1 } else { e.1 += 1 }
        cells_json.push(json!({
            "block": blkmap.get(b), "bay_idx": y, "row": crate::util::row_name(*ri), "tier": t,
            "contno": cont, "known": known,
        }));
    }
    let mut summ_vec: Vec<(i32, (i64, i64))> = summ.into_iter().collect();
    summ_vec.sort_by(|a, b| blkmap.get(&a.0).cmp(&blkmap.get(&b.0)));
    let blocks_summary: Vec<Value> = summ_vec.iter().map(|(b, (k, u))| json!({
        "block": blkmap.get(b), "block_id": b, "n_total": k + u, "n_known": k, "n_unknown": u,
    })).collect();
    let yard_t0 = json!({
        "as_of": ws.to_rfc3339(),
        "note": "per-container stack state reconstructed from yard_move up to window start (converging; covers observed containers). unknown=inferred-occupied.",
        "cells_total": cells.len(),
        "blocks": blocks_summary,
        "cells": cells_json,
    });

    let scenario_out = json!({
        "meta": { "window": [ws.to_rfc3339(), we.to_rfc3339()], "home_port": "MYPKG", "source": "scengen" },
        "vessels": vessels,
        "yard_t0": yard_t0,
    });

    // ---- EMULATOR: qc_move_s + yc_service sliced to the window (period-accurate, capped).
    let qc: Vec<(String, Option<f64>, i64)> = sqlx::query_as(
        "SELECT jobtype, percentile_cont(0.5) WITHIN GROUP (ORDER BY dur_s), count(*)
           FROM qc_move_log
          WHERE comp_ts >= $1 AND comp_ts < $2 AND dur_s BETWEEN 1 AND 300 AND jobtype IN ('DS','LD')
          GROUP BY jobtype",
    ).bind(ws).bind(we).fetch_all(pool).await?;
    let yc: Vec<(String, Option<f64>, Option<f64>, Option<f64>, i64)> = sqlx::query_as(
        "SELECT jobtype,
                percentile_cont(0.1) WITHIN GROUP (ORDER BY dur_s),
                percentile_cont(0.5) WITHIN GROUP (ORDER BY dur_s),
                percentile_cont(0.9) WITHIN GROUP (ORDER BY dur_s), count(*)
           FROM rtg_move_log
          WHERE comp_ts >= $1 AND comp_ts < $2 AND dur_s BETWEEN 0 AND 1800 AND jobtype IN ('DS','LD')
          GROUP BY jobtype",
    ).bind(ws).bind(we).fetch_all(pool).await?;

    let qc_med = |jt: &str| qc.iter().find(|r| r.0 == jt).and_then(|r| r.1).map(|v| v.round() as i64);
    let qc_n = |jt: &str| qc.iter().find(|r| r.0 == jt).map(|r| r.2).unwrap_or(0);
    let yc_p = |jt: &str| -> Value {
        match yc.iter().find(|r| r.0 == jt) {
            Some((_, p10, p50, p90, _)) => json!([p10.map(|v| v.round() as i64), p50.map(|v| v.round() as i64), p90.map(|v| v.round() as i64)]),
            None => Value::Null,
        }
    };
    let yc_n = |jt: &str| yc.iter().find(|r| r.0 == jt).map(|r| r.4).unwrap_or(0);

    let emulator_out = json!({
        "qc_move_s":  { "ds": qc_med("DS"), "ld": qc_med("LD") },
        "yc_service": { "ds": yc_p("DS"), "ld": yc_p("LD") },
        "hatch_s": Value::Null, "bay_change_s": Value::Null, "twin_ratio": Value::Null, "drive_speed_ms": Value::Null,
        "_provenance": {
            "window": [ws.to_rfc3339(), we.to_rfc3339()],
            "qc_sample": { "ds": qc_n("DS"), "ld": qc_n("LD") },
            "yc_sample": { "ds": yc_n("DS"), "ld": yc_n("LD") },
            "note": "qc_move_s/yc_service from local qc_move_log/rtg_move_log sliced to window (capped QC[1,300]s/RTG[0,1800]s); thin on small windows. hatch/bay_change/twin/drive TODO",
        },
    });

    let (nc, nds, nld, nv, nenr): (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE m.jobtype='DS'), count(*) FILTER (WHERE m.jobtype='LD'),
                count(DISTINCT m.vessel||'/'||m.voyage), count(c.contno)
           FROM scenario.move_hist m
           LEFT JOIN scenario.container c
             ON c.vessel=m.vessel AND c.voyage=m.voyage AND c.contno=m.contno
            AND c.disload = CASE m.jobtype WHEN 'DS' THEN 'D' WHEN 'LD' THEN 'L' ELSE m.jobtype END
          WHERE m.comp_ts >= $1 AND m.comp_ts < $2",
    ).bind(ws).bind(we).fetch_one(pool).await?;
    let summary = json!({
        "vessels": nv, "containers": nc, "ds": nds, "ld": nld,
        "enriched": nenr, "enriched_pct": if nc > 0 { nenr * 100 / nc } else { 0 },
        "qc_sample": qc_n("DS") + qc_n("LD"), "yc_sample": yc_n("DS") + yc_n("LD"),
    });

    Ok((scenario_out, emulator_out, summary))
}

/// Worker: claim pending assembly_job rows and materialize their output (async path).
pub async fn run(pool: &PgPool) -> Result<()> {
    let mut done = 0u32;
    loop {
        let claimed: Option<(i64, DateTime<Utc>, DateTime<Utc>)> = sqlx::query_as(
            "UPDATE scenario.assembly_job SET state='running'
              WHERE job_id = (SELECT job_id FROM scenario.assembly_job
                               WHERE state='pending' ORDER BY requested_at LIMIT 1 FOR UPDATE SKIP LOCKED)
             RETURNING job_id, window_start, window_end",
        )
        .fetch_optional(pool)
        .await?;
        let Some((job_id, ws, we)) = claimed else { break };
        let run_id = state::start_run(pool, "assemble").await?;
        state::set_phase(pool, run_id, "assemble").await.ok();
        match build(pool, ws, we).await {
            Ok((scenario_out, emulator_out, summary)) => {
                sqlx::query(
                    "UPDATE scenario.assembly_job SET state='done', scenario_out=$2::jsonb,
                        emulator_out=$3::jsonb, summary=$4::jsonb, finished_at=now() WHERE job_id=$1",
                )
                .bind(job_id).bind(scenario_out.to_string()).bind(emulator_out.to_string())
                .bind(summary.to_string()).execute(pool).await?;
                state::merge_json(pool, run_id, "collection", summary).await.ok();
                state::finish_run(pool, run_id, "done", None).await?;
                done += 1;
            }
            Err(e) => {
                tracing::error!(job_id, error = %e, "assemble failed");
                let _ = state::finish_run(pool, run_id, "error", Some(&e.to_string())).await;
                let _ = sqlx::query("UPDATE scenario.assembly_job SET state='error', error_text=$2, finished_at=now() WHERE job_id=$1")
                    .bind(job_id).bind(e.to_string()).execute(pool).await;
            }
        }
    }
    tracing::info!(jobs = done, "assemble: done");
    Ok(())
}
