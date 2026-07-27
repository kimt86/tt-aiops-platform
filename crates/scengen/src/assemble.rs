//! Scenario/emulator assembly — LOCAL ONLY, ZERO Oracle. `build()` produces the scenario +
//! emulator JSON for a window by slicing the local warehouse:
//!   scenario  ← qc_move_log (window; the QC WORK QUEUE — 1 row/move, crane, twin via shared
//!               (machno,seqno)) ⨝ move_hist (vessel/voyage attribution, 99.5%) ⨝ container
//!               (attrs+ship cell) ⨝ vessel_call (size+berth) + yard_t0 (as-of-T stack state)
//!             + landside ← rtg_move_log GI/GO (the ROAD side: external trucks in/out, with the
//!               external plate in trk_id) ⨝ yard_move for the decoded stack slot
//!   emulator  ← qc_move_log / rtg_move_log sliced to the window (period-accurate)
//! qc_move_log is the terminal's authoritative quay-crane move stream (MCH_OPERATION, PK
//! (machno,contno,seqno)); we use it as the work-list SPINE instead of JOB_ORDER_HISTORY, which
//! is a status-heartbeat log (JOBSTATUS cycles A→P→Q→C, emitting many rows per physical move).
//! Twin lifts = qc_move_log rows sharing (machno,seqno) → carried per-move as twin_group/is_twin.
//! The web service calls build() synchronously for on-demand download; the `assemble` worker
//! calls it for queued assembly_job rows. Containers not yet enriched come through move-level.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::state;

/// Build (scenario_out, emulator_out, summary) for [ws, we). Pure — no job/run side effects.
pub async fn build(pool: &PgPool, ws: DateTime<Utc>, we: DateTime<Utc>) -> Result<(Value, Value, Value)> {
    // ---- SCENARIO vessels: the QC WORK QUEUE (qc_move_log, window on comp_ts = physical quay
    // handover) attributed to a vessel via move_hist (99.5%) and enriched with container attrs.
    // twin_group/is_twin come from qc_move_log rows sharing (machno,seqno); crane_seq is the
    // crane's move order. Ordered by (crane, crane_seq) so per-crane queues are reconstructable.
    let vessels_txt: Option<String> = sqlx::query_scalar(
        r#"
        WITH q AS (
          SELECT machno, contno, seqno, jobtype, st_ts, comp_ts, dur_s,
                 count(*) OVER (PARTITION BY machno, seqno) AS lift_size,
                 row_number() OVER (PARTITION BY machno ORDER BY comp_ts, seqno) AS crane_seq
            FROM qc_move_log
           WHERE comp_ts >= $1 AND comp_ts < $2 AND jobtype IN ('DS','LD')
        ),
        vv AS (
          SELECT DISTINCT ON (contno, jobtype) contno, jobtype, vessel, voyage
            FROM scenario.move_hist
           WHERE comp_ts >= $1 - interval '1 day' AND comp_ts < $2 + interval '1 day'
             AND vessel IS NOT NULL AND voyage IS NOT NULL
           ORDER BY contno, jobtype, comp_ts
        ),
        cont AS (
          SELECT q.machno, q.crane_seq, vv.vessel, vv.voyage, jsonb_build_object(
                   'container_id', q.contno,
                   'move_type', CASE q.jobtype WHEN 'DS' THEN 'discharge' WHEN 'LD' THEN 'load' ELSE q.jobtype END,
                   'move_ts', q.comp_ts, 'start_ts', q.st_ts, 'service_s', q.dur_s,
                   'crane', q.machno, 'crane_seq', q.crane_seq,
                   'twin_group', q.machno || '/' || q.seqno, 'lift_size', q.lift_size, 'is_twin', (q.lift_size > 1),
                   'iso', c.iso, 'size', c.size, 'height', c.height, 'family', c.family, 'fill', c.fill,
                   'gross_kg', c.gross_kg, 'reefer_temp', c.reefer_temp, 'imdg', c.imdg, 'un_no', c.un_no, 'oog', c.oog,
                   'pod', c.pod, 'pol', c.pol, 'operator', c.operator,
                   'ship_cell', CASE WHEN c.ship_bay IS NOT NULL THEN jsonb_build_object(
                        'bay', c.ship_bay, 'row', c.ship_row, 'tier', c.ship_tier,
                        'deck_hold', CASE WHEN c.ship_tier >= 80 THEN 'deck' ELSE 'hold' END) END,
                   'out_vessel', c.out_vessel, 'out_voyage', c.out_voyage
                 ) AS cobj
            FROM q
            LEFT JOIN vv ON vv.contno = q.contno AND vv.jobtype = q.jobtype
            LEFT JOIN scenario.container c
              ON c.vessel = vv.vessel AND c.voyage = vv.voyage AND c.contno = q.contno
             AND c.disload = CASE q.jobtype WHEN 'DS' THEN 'D' WHEN 'LD' THEN 'L' ELSE q.jobtype END
        )
        SELECT coalesce(jsonb_agg(vobj ORDER BY sp NULLS LAST), '[]'::jsonb)::text FROM (
          SELECT vc.startpos_m AS sp, jsonb_build_object(
            'vessel_id', cont.vessel, 'voyage', cont.voyage, 'vsl_name', vc.vsl_name,
            'loa_m', vc.loa_m, 'beam_m', vc.beam_m, 'total_bays', vc.total_bays,
            'berth', jsonb_build_object('berthno', vc.berthno, 'side', vc.berthside, 'startpos_m', vc.startpos_m),
            'schedule', jsonb_build_object('berth_ts', vc.actber, 'depart_ts', vc.actdep, 'cutoff_ts', vc.cutoff),
            'cranes', to_jsonb(array_agg(DISTINCT cont.machno)),
            'containers', jsonb_agg(cont.cobj ORDER BY cont.machno, cont.crane_seq)
          ) AS vobj
          FROM cont
          LEFT JOIN scenario.vessel_call vc ON vc.vessel = cont.vessel AND vc.voyage = cont.voyage
          GROUP BY cont.vessel, cont.voyage, vc.startpos_m, vc.vsl_name, vc.loa_m, vc.beam_m,
                   vc.total_bays, vc.berthno, vc.berthside, vc.actber, vc.actdep, vc.cutoff
        ) t
        "#,
    )
    .bind(ws).bind(we)
    .fetch_one(pool)
    .await?;
    let vessels: Value = serde_json::from_str(&vessels_txt.unwrap_or_else(|| "[]".into()))?;

    // ---- t=0 yard background: per-container stack state reconstructed AS OF the window start `ws`
    // (replay scenario.yard_move up to ws — the yard as it was at the scenario START, not "now").
    // RH/AH/MI/MO are used here (yard-state only) but are NOT in the work list. Converging: covers
    // containers observed since collection began; the rest fill in as they move (~a month).
    let (cells, moves_replayed) = crate::yard::state_as_of(pool, ws).await?;
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
        "moves_replayed": moves_replayed, // grows with history — see yard::state_as_of cost note
        "blocks": blocks_summary,
        "cells": cells_json,
    });

    // ---- LANDSIDE (gate) work list: the ROAD side of the terminal — external trucks delivering
    // (GI) or collecting (GO) boxes. Spine is the LOCAL rtg_move_log (yard-crane move stream, one
    // row per physical move, PK (machno,contno,seqno)); for GI/GO its trk_id is the EXTERNAL road
    // truck's plate (yard tractors "TT####" only show up on DS/LD). The decoded stack slot comes
    // from scenario.yard_move — the SAME MCH_OPERATION row, joined on (machno,contno,seqno) — so a
    // gate move also says where the box landed / came from. Zero Oracle, like the rest of build().
    //
    // Two honest limits, both reported rather than hidden: `yard_slot` is null for windows older
    // than the yard collector's history (slot_known counts how many resolved), and WHICH physical
    // gate/lane a truck used is NOT recoverable — TOS records LANEID as the constant 'GATE00'.
    // RH/AH/MI/MO (rehandle, internal transfer) stay OUT of the work list by design: they are
    // yard-state only, the same rule vessels[].containers[] follows.
    let landside_txt: Option<String> = sqlx::query_scalar(
        r#"
        WITH g AS (
          SELECT machno, contno, seqno, jobtype, trk_id, st_ts, comp_ts, dur_s, status
            FROM rtg_move_log
           WHERE comp_ts >= $1 AND comp_ts < $2 AND jobtype IN ('GI','GO')
        ),
        m AS (
          SELECT g.*, yb.block, y.bay_idx, y.row_idx, y.tier
            FROM g
            LEFT JOIN scenario.yard_move y
              ON y.machno = g.machno AND y.contno = g.contno AND y.seqno = g.seqno
            LEFT JOIN scenario.yard_block yb ON yb.block_id = y.block_id
        )
        SELECT jsonb_build_object(
          'note', 'gate work list from rtg_move_log GI/GO (trk_id = external road truck). yard_slot joined from yard_move where reconstructed; physical gate/lane is not recorded by TOS.',
          'moves_total',   count(*),
          'gate_in',       count(*) FILTER (WHERE jobtype = 'GI'),
          'gate_out',      count(*) FILTER (WHERE jobtype = 'GO'),
          'trucks_unique', count(DISTINCT trk_id),
          'slot_known',    count(tier),
          'moves', coalesce(jsonb_agg(jsonb_build_object(
              'container_id', contno,
              'move_type', CASE jobtype WHEN 'GI' THEN 'gate_in' ELSE 'gate_out' END,
              'move_ts', comp_ts, 'start_ts', st_ts, 'service_s', dur_s,
              'yard_crane', machno, 'truck', trk_id,
              'fill', CASE status WHEN 'F' THEN 'full' WHEN 'M' THEN 'empty' END,
              'yard_slot', CASE WHEN tier IS NOT NULL THEN jsonb_build_object(
                   'block', block, 'bay_idx', bay_idx,
                   'row', CASE WHEN row_idx BETWEEN 0 AND 25 THEN chr(65 + row_idx) ELSE row_idx::text END,
                   'tier', tier) END
            ) ORDER BY comp_ts), '[]'::jsonb)
        )::text
        FROM m
        "#,
    )
    .bind(ws).bind(we)
    .fetch_one(pool)
    .await?;
    let landside: Value = serde_json::from_str(&landside_txt.unwrap_or_else(|| "{}".into()))?;
    let lnd = |k: &str| landside.get(k).and_then(Value::as_i64).unwrap_or(0);
    let (gate_in, gate_out, gate_trucks, gate_slots) =
        (lnd("gate_in"), lnd("gate_out"), lnd("trucks_unique"), lnd("slot_known"));

    let scenario_out = json!({
        "meta": { "window": [ws.to_rfc3339(), we.to_rfc3339()], "home_port": "MYPKG", "source": "scengen" },
        "vessels": vessels,
        "landside": landside,
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
        "hatch_s": Value::Null, "bay_change_s": Value::Null, "drive_speed_ms": Value::Null,
        "_provenance": {
            "window": [ws.to_rfc3339(), we.to_rfc3339()],
            "qc_sample": { "ds": qc_n("DS"), "ld": qc_n("LD") },
            "yc_sample": { "ds": yc_n("DS"), "ld": yc_n("LD") },
            "note": "qc_move_s/yc_service from local qc_move_log/rtg_move_log sliced to window (capped QC[1,300]s/RTG[0,1800]s); thin on small windows. twin is now per-move in the work list (twin_group/is_twin), not an emulator ratio. hatch/bay_change/drive TODO",
        },
    });

    // Summary mirrors the vessels spine: qc_move_log (window) ⨝ move_hist (vessel) ⨝ container.
    let (nc, nds, nld, ncr, ntw, nv, nenr): (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        r#"WITH q AS (
             SELECT machno, contno, seqno, jobtype,
                    count(*) OVER (PARTITION BY machno, seqno) AS lift_size
               FROM qc_move_log
              WHERE comp_ts >= $1 AND comp_ts < $2 AND jobtype IN ('DS','LD')
           ),
           vv AS (
             SELECT DISTINCT ON (contno, jobtype) contno, jobtype, vessel, voyage
               FROM scenario.move_hist
              WHERE comp_ts >= $1 - interval '1 day' AND comp_ts < $2 + interval '1 day'
                AND vessel IS NOT NULL AND voyage IS NOT NULL
              ORDER BY contno, jobtype, comp_ts
           )
           SELECT count(*),
                  count(*) FILTER (WHERE q.jobtype='DS'),
                  count(*) FILTER (WHERE q.jobtype='LD'),
                  count(DISTINCT q.machno),
                  count(*) FILTER (WHERE q.lift_size > 1),
                  count(DISTINCT vv.vessel || '/' || vv.voyage),
                  count(c.contno)
             FROM q
             LEFT JOIN vv ON vv.contno = q.contno AND vv.jobtype = q.jobtype
             LEFT JOIN scenario.container c
               ON c.vessel = vv.vessel AND c.voyage = vv.voyage AND c.contno = q.contno
              AND c.disload = CASE q.jobtype WHEN 'DS' THEN 'D' WHEN 'LD' THEN 'L' ELSE q.jobtype END"#,
    ).bind(ws).bind(we).fetch_one(pool).await?;
    let summary = json!({
        "vessels": nv, "containers": nc, "ds": nds, "ld": nld, "cranes": ncr, "twin_moves": ntw,
        "enriched": nenr, "enriched_pct": if nc > 0 { nenr * 100 / nc } else { 0 },
        "qc_sample": qc_n("DS") + qc_n("LD"), "yc_sample": yc_n("DS") + yc_n("LD"),
        // Landside (gate) — mirrors scenario_out.landside so the UI can show quay vs road at a
        // glance. slot_pct is the share of gate moves whose yard slot could be reconstructed.
        "gate_in": gate_in, "gate_out": gate_out, "gate_trucks": gate_trucks,
        "gate_slot_pct": if gate_in + gate_out > 0 { gate_slots * 100 / (gate_in + gate_out) } else { 0 },
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
