//! Scenario/emulator assembly — LOCAL ONLY, ZERO Oracle. `build()` produces the scenario +
//! emulator JSON for a window by slicing the local warehouse:
//!   scenario  ← qc_move_log (window; the QC WORK QUEUE — 1 row/move, crane, twin via shared
//!               (machno,seqno)) ⨝ move_hist (vessel/voyage attribution, 99.5%) ⨝ container
//!               (attrs+ship cell) ⨝ vessel_call (size+berth) + yard_t0 (as-of-T stack state)
//!             + landside ← rtg_move_log GI/GO (the ROAD side: external trucks in/out, with the
//!               external plate in trk_id) ⨝ yard_move for the decoded stack slot ⨝ gate_event for
//!               the truck's own clock (gate transaction, wait, exit)
//!             + equipment ← deployment spans derived from the same real move streams
//!               (qc_move_log / yard_move / tt_move_log), never from the TOS assignment plan
//!   emulator  ← learn_qc_move_time (crane move time; a snapshot, NOT window-sliced) + rtg_move_log
//!               sliced to the window (yard-crane service) + documented constants for what our
//!               move logs cannot separate (hatch cover, bay change, drive speed)
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

/// Idle gap that ends an equipment deployment span. Measured: 95% of consecutive quay-crane moves
/// are <8 min apart and only 0.6% of gaps reach 30 min, so 60 min cleanly separates "still on this
/// job" (including a shift handover) from "no longer deployed" without fragmenting real spans.
const SPAN_GAP_MIN: i64 = 60;
/// Bucket width for the truck-fleet curve.
const FLEET_BUCKET_MIN: i64 = 30;

/// Yard-crane service cap used when estimating the emulator distribution. Matches the published
/// methodology (scripts/estimate_equipment_specs.sh): below 5s is not a service, above 600s is a
/// stall bleeding into the measurement. NOT the extractor's storage cap — a different job.
const YC_CAP_S: (i64, i64) = (5, 600);
/// Deck<->hold hatch-cover swap, once per bay. Measured in the research log; our move logs cannot
/// separate it, so it travels as a documented constant instead of as null.
const HATCH_S_DS: i64 = 428;
const HATCH_S_LD: i64 = 496;
/// Gantry move to a different bay.
const BAY_CHANGE_S: i64 = 180;
/// Pure driving speed from the GPS motion split (22.8 km/h with stopped segments excluded).
/// Stop overhead is modelled by the emulator itself (handover, queueing, stall), not by this.
const DRIVE_SPEED_MS: f64 = 6.33;
/// Used only when the learner has no row for a job type yet.
const QC_MOVE_S_FALLBACK: (i64, i64) = (90, 110);
/// Cap on the gap between one crane's consecutive completions, in seconds. Copied from the learner
/// (extractor/sql/qc_move_time.sql) so a window-measured number is comparable with the snapshot it
/// replaces. The cap is the whole measurement: it drops meal breaks, idle stretches and bay/hatch
/// transitions, leaving the per-container handling cadence rather than a shift log.
const QC_GAP_CAP_S: (f64, f64) = (1.0, 300.0);
/// Gaps a crane needs before its median counts, also the learner's threshold. A window too short or
/// too quiet to clear this simply falls back — reported per job type, not silently averaged in.
const QC_MIN_GAPS_PER_CRANE: i64 = 30;
/// Bay-change gap bounds (s) and the sample floor. 1800 drops shift breaks; 0 keeps the low end
/// visible so a contaminated population shows itself rather than being trimmed into looking clean.
const BAY_GAP_CAP_S: (f64, f64) = (0.0, 1800.0);
/// Transitions needed before the window's own number is used. The estimator below is a densest-half
/// mean, which needs a real population to find a peak in.
const BAY_MIN_SAMPLES: i64 = 100;
/// Cranes that must qualify before the window's own number is used. This is a FLEET parameter, and
/// one crane is not a fleet. Measured on a 1-hour window: only a single crane cleared the gap
/// threshold for load, so its personal rhythm would have become the published fleet figure while the
/// sample count alone looked healthy. Below this the learner's multi-day, ~55-crane snapshot is the
/// better estimate, and the scope field says which one was used.
const QC_MIN_CRANES: i64 = 3;

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
                        -- 50, not 80. Cross-checked against the queuename D/H letter that TOS itself
                        -- writes, over 2,094 discharge moves: 'H' rows are ship_tier 2..22 and 'D'
                        -- rows are 66..94, with the 25..65 band empty. The old threshold put tiers
                        -- 66..78 — 33.6% of all deck containers — into 'hold'.
                        'deck_hold', CASE WHEN c.ship_tier >= 50 THEN 'deck' ELSE 'hold' END) END,
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
            -- est_depart_ts is THE SCORING BASELINE. A better dispatch does not make a ship carry
            -- more boxes — its stowage plan is fixed — it makes the ship FINISH EARLIER, and
            -- "earlier" needs something to be earlier than. It was collected all along and simply
            -- never emitted, which left the output with no way to say whether a run was good.
            -- depart_ts (actual) is what happened; est_depart_ts is what was promised. Note actual
            -- is only present once the ship has left AND while it is still inside the live
            -- schedule's ~2-day window, so a scenario for a call still alongside carries the
            -- estimate alone — which is the right input for a forward-looking run anyway.
            -- The spec's deadline is min(est_depart − buffer, estwkc). estwkc does not exist in this
            -- schema, so the raw times are emitted and the buffer stays where it belongs: a policy
            -- parameter of the run, not a constant baked into the data.
            'schedule', jsonb_build_object('berth_ts', vc.actber, 'depart_ts', vc.actdep,
                                           'est_depart_ts', vc.estdep, 'cutoff_ts', vc.cutoff),
            'cranes', to_jsonb(array_agg(DISTINCT cont.machno)),
            'containers', jsonb_agg(cont.cobj ORDER BY cont.machno, cont.crane_seq)
          ) AS vobj
          FROM cont
          LEFT JOIN scenario.vessel_call vc ON vc.vessel = cont.vessel AND vc.voyage = cont.voyage
          GROUP BY cont.vessel, cont.voyage, vc.startpos_m, vc.vsl_name, vc.loa_m, vc.beam_m,
                   vc.total_bays, vc.berthno, vc.berthside, vc.actber, vc.actdep, vc.estdep, vc.cutoff
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
          SELECT g.*, yb.block, y.bay_idx, y.row_idx, y.tier,
                 gi.event_ts AS gate_ts, gi.clerk AS gate_clerk, gx.event_ts AS exit_ts,
                 cs.size, scenario.size_teu(cs.size) AS teu
            FROM g
            -- Box size, so road volume can be stated in TEU and not only in moves. Keyed by
            -- container number alone: an ISO type is a permanent property of the box, so one lookup
            -- ever (mig 0114). NULL where we have not met the box yet, which is why the header
            -- publishes how many moves the TEU total actually covers.
            LEFT JOIN scenario.container_spec cs ON cs.contno = g.contno
            LEFT JOIN scenario.yard_move y
              ON y.machno = g.machno AND y.contno = g.contno AND y.seqno = g.seqno
            LEFT JOIN scenario.yard_block yb ON yb.block_id = y.block_id
            -- Gate transaction for THIS visit: the intake nearest before the yard handling. A box
            -- passes through many times, so match on time rather than on the container alone.
            LEFT JOIN LATERAL (
              SELECT ge.event_ts, ge.clerk FROM scenario.gate_event ge
               WHERE ge.contno = g.contno
                 AND ge.situation = CASE g.jobtype WHEN 'GI' THEN 'GIY' ELSE 'QYG' END
                 AND ge.event_ts <= g.comp_ts
               ORDER BY ge.event_ts DESC LIMIT 1
            ) gi ON true
            -- Export only: when the truck actually left the terminal.
            LEFT JOIN LATERAL (
              SELECT ge.event_ts FROM scenario.gate_event ge
               WHERE g.jobtype = 'GO' AND ge.contno = g.contno AND ge.situation = 'GOY'
                 AND ge.event_ts >= g.comp_ts
               ORDER BY ge.event_ts LIMIT 1
            ) gx ON true
        )
        SELECT jsonb_build_object(
          'note', 'gate work list from rtg_move_log GI/GO (trk_id = external road truck). yard_slot joined from yard_move where reconstructed; gate_ts/exit_ts from the gate transaction stream. physical gate/lane is not recorded by TOS.',
          'moves_total',   count(*),
          'gate_in',       count(*) FILTER (WHERE jobtype = 'GI'),
          'gate_out',      count(*) FILTER (WHERE jobtype = 'GO'),
          'trucks_unique', count(DISTINCT trk_id),
          'slot_known',    count(tier),
          'gate_ts_known', count(gate_ts),
          'exit_ts_known', count(exit_ts),
          -- TEU over the moves whose box size is known. size_known is NOT decoration: read the TEU
          -- total without it and a half-covered window looks like a quiet one.
          'teu_in',        coalesce(sum(teu) FILTER (WHERE jobtype = 'GI'), 0),
          'teu_out',       coalesce(sum(teu) FILTER (WHERE jobtype = 'GO'), 0),
          'size_known',    count(size),
          'size_mix',      (SELECT coalesce(jsonb_object_agg(size, n), '{}'::jsonb)
                              FROM (SELECT size, count(*) n FROM m WHERE size IS NOT NULL
                                     GROUP BY size) z),
          'moves', coalesce(jsonb_agg(jsonb_build_object(
              'container_id', contno,
              'move_type', CASE jobtype WHEN 'GI' THEN 'gate_in' ELSE 'gate_out' END,
              'move_ts', comp_ts, 'start_ts', st_ts, 'service_s', dur_s,
              'yard_crane', machno, 'truck', trk_id,
              'fill', CASE status WHEN 'F' THEN 'full' WHEN 'M' THEN 'empty' END,
              'size', size, 'teu', teu,
              -- The road truck's own clock: cleared the gate, waited, was served, left.
              'gate_ts', gate_ts,
              'gate_wait_s', CASE WHEN gate_ts IS NOT NULL
                                  THEN round(extract(epoch FROM (st_ts - gate_ts)))::int END,
              'exit_ts', exit_ts,
              'exit_s', CASE WHEN exit_ts IS NOT NULL
                             THEN round(extract(epoch FROM (exit_ts - comp_ts)))::int END,
              'gate_clerk', gate_clerk,
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
    let gate_ts_known = lnd("gate_ts_known");

    // ---- EQUIPMENT DEPLOYMENT: which machines were on duty, when, and on what. Derived from the
    // REAL move streams, deliberately NOT from TOS's assignment plan (JOB_CRANE_HISTORY): measured
    // against a collected ground truth, plan-derived crane->vessel attribution put 27% of quay
    // moves on the WRONG ship (whole crane-shifts misassigned), because the plan churns and is not
    // what the crane actually worked. scenario.crane_deploy stays a plan-vs-actual comparison aid.
    //   qc  ← qc_move_log runs of one crane on one vessel  (vessel via move_hist, as in vessels[])
    //   rtg ← scenario.yard_move runs of one machine in one block (RTG + ES yard cranes)
    //   tt_fleet ← tt_move_log cycles [dispatch_ts, free_ts) overlapping each bucket. trk_id/ytno
    //              is a real vehicle id, not a trip id — verified against GPS (432 of 443 quay-move
    //              truck ids appear in the position feed) and against the authoritative cycle log.
    // A span ends after SPAN_GAP_MIN of silence or when the vessel/block changes. Unattributed quay
    // moves are skipped rather than allowed to split a span into phantom one-move deployments.
    // Note rtg[] only covers what the yard collector has reconstructed for this window; qc[] and
    // tt_fleet come from streams that reach back further.
    let equipment_txt: Option<String> = sqlx::query_scalar(&format!(
        r#"
        WITH vv AS (
          SELECT DISTINCT ON (contno, jobtype) contno, jobtype, vessel, voyage
            FROM scenario.move_hist
           WHERE comp_ts >= $1 - interval '1 day' AND comp_ts < $2 + interval '1 day'
             AND vessel IS NOT NULL AND voyage IS NOT NULL
           ORDER BY contno, jobtype, comp_ts
        ),
        qm AS (
          SELECT q.machno, q.comp_ts, q.jobtype, vv.vessel, vv.voyage
            FROM qc_move_log q
            JOIN vv ON vv.contno = q.contno AND vv.jobtype = q.jobtype
           WHERE q.comp_ts >= $1 AND q.comp_ts < $2 AND q.jobtype IN ('DS','LD')
        ),
        qb AS (
          SELECT *, CASE WHEN lag(comp_ts) OVER w IS NULL
                           OR comp_ts - lag(comp_ts) OVER w > interval '{SPAN_GAP_MIN} min'
                           OR vessel IS DISTINCT FROM lag(vessel) OVER w
                           OR voyage IS DISTINCT FROM lag(voyage) OVER w
                         THEN 1 ELSE 0 END brk
            FROM qm WINDOW w AS (PARTITION BY machno ORDER BY comp_ts)
        ),
        qs AS (SELECT *, sum(brk) OVER (PARTITION BY machno ORDER BY comp_ts) grp FROM qb),
        qspan AS (
          SELECT machno, vessel, voyage, min(comp_ts) st, max(comp_ts) en, count(*) n,
                 count(*) FILTER (WHERE jobtype = 'DS') ds, count(*) FILTER (WHERE jobtype = 'LD') ld
            FROM qs GROUP BY machno, vessel, voyage, grp
        ),
        ym AS (
          SELECT machno, block_id, comp_ts FROM scenario.yard_move
           WHERE comp_ts >= $1 AND comp_ts < $2
        ),
        yb AS (
          SELECT *, CASE WHEN lag(comp_ts) OVER w IS NULL
                           OR comp_ts - lag(comp_ts) OVER w > interval '{SPAN_GAP_MIN} min'
                           OR block_id IS DISTINCT FROM lag(block_id) OVER w
                         THEN 1 ELSE 0 END brk
            FROM ym WINDOW w AS (PARTITION BY machno ORDER BY comp_ts)
        ),
        ys AS (SELECT *, sum(brk) OVER (PARTITION BY machno ORDER BY comp_ts) grp FROM yb),
        yspan AS (
          SELECT machno, block_id, min(comp_ts) st, max(comp_ts) en, count(*) n
            FROM ys GROUP BY machno, block_id, grp
        ),
        tbk AS (
          SELECT generate_series($1, $2 - interval '{FLEET_BUCKET_MIN} min',
                                 interval '{FLEET_BUCKET_MIN} min') ts
        ),
        tcy AS (
          SELECT ytno, dispatch_ts, free_ts FROM tt_move_log
           WHERE free_ts >= $1 AND dispatch_ts < $2
        ),
        tfl AS (
          SELECT tbk.ts, count(DISTINCT tcy.ytno) trucks, count(tcy.*) cycles
            FROM tbk LEFT JOIN tcy
              ON tcy.dispatch_ts < tbk.ts + interval '{FLEET_BUCKET_MIN} min'
             AND tcy.free_ts >= tbk.ts
           GROUP BY tbk.ts
        )
        SELECT jsonb_build_object(
          'note', 'deployment derived from actual move logs, not the TOS assignment plan. a span ends after span_gap_min of silence or when the vessel/block changes. rtg is limited to the yard history reconstructed for this window.',
          'span_gap_min', {SPAN_GAP_MIN},
          'qc', (SELECT coalesce(jsonb_agg(jsonb_build_object(
                    'crane', machno, 'vessel_id', vessel, 'voyage', voyage,
                    'start_ts', st, 'end_ts', en, 'moves', n, 'ds', ds, 'ld', ld)
                  ORDER BY machno, st), '[]'::jsonb) FROM qspan),
          'rtg', (SELECT coalesce(jsonb_agg(jsonb_build_object(
                    'machine', yspan.machno, 'block', ybk.block, 'block_id', yspan.block_id,
                    'start_ts', yspan.st, 'end_ts', yspan.en, 'moves', yspan.n)
                  ORDER BY yspan.machno, yspan.st), '[]'::jsonb)
                    FROM yspan LEFT JOIN scenario.yard_block ybk ON ybk.block_id = yspan.block_id),
          'tt_fleet', jsonb_build_object(
             'bucket_minutes', {FLEET_BUCKET_MIN},
             'trucks_total', (SELECT count(DISTINCT ytno) FROM tcy),
             'peak_trucks',  (SELECT coalesce(max(trucks), 0) FROM tfl),
             'buckets', (SELECT coalesce(jsonb_agg(jsonb_build_object(
                            'ts', ts, 'trucks', trucks, 'cycles', cycles) ORDER BY ts), '[]'::jsonb)
                         FROM tfl))
        )::text
        "#
    ))
    .bind(ws).bind(we)
    .fetch_one(pool)
    .await?;
    let equipment: Value = serde_json::from_str(&equipment_txt.unwrap_or_else(|| "{}".into()))?;
    let eq_len = |k: &str| equipment.get(k).and_then(Value::as_array).map_or(0, Vec::len) as i64;
    let (qc_spans, rtg_spans) = (eq_len("qc"), eq_len("rtg"));
    let peak_trucks = equipment
        .get("tt_fleet")
        .and_then(|f| f.get("peak_trucks"))
        .and_then(Value::as_i64)
        .unwrap_or(0);

    // cranes[] — the work list the simulator walks, derived from what the quay cranes ACTUALLY did.
    //
    // WHY ACTUALS AND NOT THE PLAN. A quay crane does not choose its work; it executes the stowage
    // plan it was given. It is a boundary condition of the simulation, not one of its decisions —
    // the decision under test is truck dispatch. So taking the observed sequence as given is not a
    // compromise on fidelity, it is the correct model of the machine.
    //
    // And the plan is the wrong source for a REPLAY besides: ships leave with work undone. Measured
    // over 41 departed calls, 805 planned containers were never worked (CVAS 003/2026 alone: 4,430
    // planned, 4,121 done). Handing the simulator the plan makes it perform moves that never
    // happened, and charges dispatch for a shortfall that was a roll-over or a cut-off.
    //
    // Runs, not totals: a crane returning to a bay is a real event that costs a gantry move, and the
    // data is full of them — C37 on MOUA 007/2026 went 22D-D, 22H-D, 10D-D, back to 22H-D for a
    // SINGLE container, then 10D-D again. Collapsing by bay would erase that; the run boundary keeps
    // each visit as its own queue entry, which is also how the spec asks for it.
    //
    // `vessel_id` sits on the queue ENTRY, not on the crane. The spec's one-vessel-per-crane shape
    // does not hold here — measured, 41 of 73 cranes serve two or more vessels, one of them four.
    //
    // observed_from/to are provenance, NOT a schedule. They are what happened, kept so a run can be
    // scored against reality; the simulator decides its own timing from the queue and the policy
    // under test. Window is [ws, we), so for a past window these moves ARE the outstanding work at ws.
    let cranes_txt: Option<String> = sqlx::query_scalar(
        r#"
        WITH mv AS (
          SELECT machno, comp_ts, vessel, voyage,
                 regexp_replace(queuename, '^([0-9]+[HD]-[DL])[0-9]+$', '\1') AS qkey
            FROM qc_move_log
           WHERE comp_ts >= $1 AND comp_ts < $2
             AND jobtype IN ('DS','LD')          -- MI/MO carry yard-internal ids, a different grammar
             AND queuename IS NOT NULL           -- labels start 2026-07-30 02:12Z; older windows resolve nothing
             AND vessel IS NOT NULL
        ),
        b AS (
          SELECT *, CASE WHEN lag(qkey)   OVER w IS DISTINCT FROM qkey
                           OR lag(vessel) OVER w IS DISTINCT FROM vessel
                           OR lag(voyage) OVER w IS DISTINCT FROM voyage
                         THEN 1 ELSE 0 END brk
            FROM mv WINDOW w AS (PARTITION BY machno ORDER BY comp_ts)
        ),
        r AS (SELECT *, sum(brk) OVER (PARTITION BY machno ORDER BY comp_ts) grp FROM b),
        q AS (
          SELECT machno, vessel, voyage, qkey, grp,
                 count(*)::int qty, min(comp_ts) st, max(comp_ts) en
            FROM r GROUP BY machno, vessel, voyage, qkey, grp
        ),
        s AS (SELECT *, row_number() OVER (PARTITION BY machno ORDER BY st)::int seq FROM q)
        SELECT coalesce(jsonb_agg(c ORDER BY c->>'qc_id'), '[]'::jsonb)::text FROM (
          SELECT jsonb_build_object(
                   'qc_id', machno,
                   'moves', sum(qty)::int,
                   'queue', jsonb_agg(jsonb_build_object(
                       'seq', seq, 'vessel_id', vessel, 'voyage', voyage,
                       'bay', substring(qkey from '^([0-9]+)')::int,
                       'dh',  substring(qkey from '^[0-9]+([HD])'),
                       'job', right(qkey, 1),
                       'qty', qty,
                       'observed_from', st, 'observed_to', en) ORDER BY seq)) AS c
            FROM s GROUP BY machno
        ) z
        "#,
    )
    .bind(ws).bind(we)
    .fetch_one(pool)
    .await?;
    let cranes: Value = serde_json::from_str(&cranes_txt.unwrap_or_else(|| "[]".into()))?;

    // How much of the window's quay work made it into cranes[]. The failure that matters here is
    // UNDER-reporting: a move without a bay label is simply absent, and a scenario missing a third of
    // the berth looks exactly like a quiet shift. Surfaced rather than assumed.
    let (qc_moves_total, qc_moves_queued): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE queuename IS NOT NULL AND vessel IS NOT NULL)
           FROM qc_move_log
          WHERE comp_ts >= $1 AND comp_ts < $2 AND jobtype IN ('DS','LD')",
    )
    .bind(ws).bind(we)
    .fetch_one(pool)
    .await?;

    let scenario_out = json!({
        "meta": { "window": [ws.to_rfc3339(), we.to_rfc3339()], "home_port": "MYPKG", "source": "scengen" },
        "vessels": vessels,
        "cranes": cranes,
        "landside": landside,
        "equipment": equipment,
        "yard_t0": yard_t0,
    });

    // ---- EMULATOR (the MeasuredModel the simulator is initialised with).
    //
    // qc_move_s comes from the LEARNED table, not from qc_move_log.dur_s. dur_s is COMP−ST, which
    // the equipment study explicitly rejected: it does not capture the lift cycle. Measured here,
    // the old wiring was worse than "imprecise" — capping dur_s to [1,300]s admitted 34% of
    // discharge moves but only 0.91% of load moves (whose median is 1396s), so the published
    // "load move time" was the median of a 0.9% selection-biased tail. learn_qc_move_time holds
    // per-crane medians of the effective one-container time (shift='ALL' is the combined row).
    //
    // yc_service stays window-sliced from rtg_move_log (that IS period-accurate) but now uses the
    // study's cap, and carries the rehandle/gate job types the yard crane also serves.
    //
    // hatch_s / bay_change_s / drive_speed_ms cannot be separated out of our move logs at all, so
    // they ship as documented constants rather than as null — a simulator cannot start without
    // them, and null silently forced every consumer to invent its own number.
    let qcl: Vec<(String, Option<f64>, i64, i64, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT jobtype, avg(med_sec)::float8, count(DISTINCT qc), coalesce(sum(n),0)::bigint,
                max(as_of_ts)
           FROM learn_qc_move_time WHERE shift = 'ALL' GROUP BY jobtype",
    ).fetch_all(pool).await?;
    let yc: Vec<(String, Option<f64>, Option<f64>, Option<f64>, i64)> = sqlx::query_as(
        "SELECT jobtype,
                percentile_cont(0.1) WITHIN GROUP (ORDER BY dur_s),
                percentile_cont(0.5) WITHIN GROUP (ORDER BY dur_s),
                percentile_cont(0.9) WITHIN GROUP (ORDER BY dur_s), count(*)
           FROM rtg_move_log
          WHERE comp_ts >= $1 AND comp_ts < $2 AND dur_s BETWEEN $3 AND $4
            AND jobtype IN ('DS','LD','RH','AH','GI','GO')
          GROUP BY jobtype",
    ).bind(ws).bind(we).bind(YC_CAP_S.0 as i32).bind(YC_CAP_S.1 as i32).fetch_all(pool).await?;

    // Same measure as the learner, but over THIS window — so a scenario describes the crane speed of
    // the period it depicts instead of whatever the learner last refreshed to.
    //
    // The definition is copied deliberately, not invented (extractor/sql/qc_move_time.sql): the gap
    // between one crane's consecutive completions, capped to [1,300]s. The cap is what makes it a
    // handling cadence rather than a shift log — it drops meal breaks, idle stretches, and bay/hatch
    // transitions, leaving the genuine per-container rhythm. Per-crane median first, then averaged
    // across cranes, so one busy crane cannot set the fleet's number.
    //
    // Everything it needs is in qc_move_log, so this costs no Oracle. What it does NOT use is
    // dur_s (COMP-ST): the equipment study rejected that as not capturing the lift cycle, and
    // measured here it was worse than imprecise — capping dur_s to [1,300]s admitted 34% of
    // discharge moves but 0.91% of load moves (median 1396s), so the published "load move time"
    // was the median of a 0.9% selection-biased tail.
    let qcw: Vec<(String, Option<f64>, i64, i64)> = sqlx::query_as(
        "WITH g AS (
           SELECT machno, jobtype,
                  extract(epoch FROM comp_ts
                          - lag(comp_ts) OVER (PARTITION BY machno ORDER BY comp_ts)) AS gap
             FROM qc_move_log
            WHERE comp_ts >= $1 AND comp_ts < $2
              AND jobtype IN ('DS','LD') AND machno ~ '^[CMZ][0-9]+$'
         ),
         per_crane AS (
           SELECT machno, jobtype,
                  percentile_cont(0.5) WITHIN GROUP (ORDER BY gap) AS med, count(*) AS n
             FROM g WHERE gap BETWEEN $3 AND $4
            GROUP BY machno, jobtype HAVING count(*) >= $5
         )
         SELECT jobtype, avg(med)::float8, count(*)::bigint, coalesce(sum(n),0)::bigint
           FROM per_crane GROUP BY jobtype",
    )
    .bind(ws).bind(we)
    .bind(QC_GAP_CAP_S.0).bind(QC_GAP_CAP_S.1).bind(QC_MIN_GAPS_PER_CRANE)
    .fetch_all(pool)
    .await?;

    // bay_change_s — measured, from the gap between one crane finishing a bay and completing its
    // first container in the next one.
    //
    // THE PROBLEM WITH THAT GAP is that it is not purely the gantry move: it also contains whatever
    // truck wait followed. Publishing the median (361s over 1,025 clean transitions) would fold
    // dispatch performance INTO an equipment constant, and then a better policy could never show an
    // improvement — the delay it should remove is already baked into the machine.
    //
    // THE ESCAPE is the shape of the distribution. Observed gap = physical transition + a
    // non-negative wait, so the waits only ever push right. The histogram bears that out — a single
    // peak near 210s with a long right tail — which makes the DENSEST REGION, not the median, the
    // estimate closest to the physical floor. This takes the shortest interval containing half the
    // transitions and averages inside it: robust to the tail, and it uses averaging within the peak
    // to cancel noise rather than across the whole contaminated range.
    //
    // Then subtract one container cycle, because the interval ends at a COMPLETION — it contains the
    // full lift of the first container in the new bay. Measured over 2026-07-28 onward: densest half
    // 267s (band 132..423) minus a ~100s cycle = ~167s, against the research constant of 180s. The two
    // methods landing within 7% of each other is the reason this is trusted enough to ship.
    //
    // Runs of a single move are excluded on both sides. 45% of raw "visits" are one container, and
    // 2,473 of those sit between two visits to the SAME bay with a ~116s gap — one container cycle,
    // i.e. a stray box handled mid-bay, not a gantry move at all. Counting them would halve the
    // estimate.
    //
    // hatch_s gets NO such treatment and stays a constant: the same measurement on same-bay
    // deck↔hold transitions puts the densest half at 16..389s, and 16s cannot contain a lift plus a
    // hatch-cover operation. That population is contaminated by label changes that are not physical
    // hatch work, and nothing here separates them.
    let bay_gap: Option<(Option<f64>, i64)> = sqlx::query_as(
        r#"
        WITH m AS (
          SELECT machno, vessel, comp_ts,
                 regexp_replace(queuename, '^([0-9]+[HD]-[DL])[0-9]+$', '\1') AS qk
            FROM qc_move_log
           WHERE comp_ts >= $1 AND comp_ts < $2 AND jobtype IN ('DS','LD')
             AND queuename IS NOT NULL AND vessel IS NOT NULL AND machno ~ '^[CMZ][0-9]+$'
        ),
        b AS (SELECT *, CASE WHEN lag(qk) OVER w IS DISTINCT FROM qk
                               OR lag(vessel) OVER w IS DISTINCT FROM vessel THEN 1 ELSE 0 END brk
                FROM m WINDOW w AS (PARTITION BY machno ORDER BY comp_ts)),
        r AS (SELECT *, sum(brk) OVER (PARTITION BY machno ORDER BY comp_ts) g FROM b),
        v AS (SELECT machno, vessel, qk, g, min(comp_ts) st, max(comp_ts) en, count(*) n
                FROM r GROUP BY 1,2,3,4),
        t AS (SELECT *, lag(qk) OVER w pq, lag(en) OVER w pe, lag(n) OVER w pn
                FROM v WINDOW w AS (PARTITION BY machno ORDER BY st)),
        gap AS (
          SELECT extract(epoch FROM st - pe) AS s FROM t
           WHERE pe IS NOT NULL AND n >= 2 AND pn >= 2
             AND substring(qk FROM '^[0-9]+') IS DISTINCT FROM substring(pq FROM '^[0-9]+')
             AND extract(epoch FROM st - pe) BETWEEN $3 AND $4
        ),
        o AS (SELECT s, row_number() OVER (ORDER BY s) i, count(*) OVER () n FROM gap),
        w2 AS (SELECT i, s lo, lead(s, (n/2)::int) OVER (ORDER BY s) hi FROM o),
        best AS (SELECT i i0 FROM w2 WHERE hi IS NOT NULL ORDER BY hi - lo LIMIT 1)
        SELECT avg(o.s)::float8, (SELECT max(n) FROM o)::bigint
          FROM o, best
         WHERE o.i BETWEEN best.i0 AND best.i0 + (SELECT max(n)/2 FROM o)
        "#,
    )
    .bind(ws).bind(we).bind(BAY_GAP_CAP_S.0).bind(BAY_GAP_CAP_S.1)
    .fetch_optional(pool)
    .await?;

    let qc_row = |jt: &str| qcl.iter().find(|r| r.0 == jt);
    let qcw_row = |jt: &str| {
        qcw.iter().find(|r| r.0 == jt && r.1.is_some() && r.2 >= QC_MIN_CRANES)
    };
    // Window first, learner second, constant last. A short window may not give any crane the
    // minimum number of gaps, and that is a normal outcome rather than an error — which is exactly
    // why the scope is reported per job type instead of being claimed once for the whole file.
    let qc_move_s = |jt: &str, fallback: i64| {
        qcw_row(jt)
            .and_then(|r| r.1)
            .or_else(|| qc_row(jt).and_then(|r| r.1))
            .map_or(fallback, |v| v.round() as i64)
    };
    let qc_scope = |jt: &str| {
        if qcw_row(jt).is_some() { "window" }
        else if qc_row(jt).map_or(false, |r| r.1.is_some()) { "snapshot (learner — NOT this window)" }
        else { "constant fallback" }
    };
    let qc_cranes = |jt: &str| qcw_row(jt).map_or_else(|| qc_row(jt).map_or(0, |r| r.2), |r| r.2);
    let qc_n = |jt: &str| qcw_row(jt).map_or_else(|| qc_row(jt).map_or(0, |r| r.3), |r| r.3);
    let qc_as_of = qcl.iter().filter_map(|r| r.4).max();

    // Subtract one container cycle — the interval ends at a completion, so it carries the first
    // lift in the new bay. Floored at 30s: a gantry move plus spotting cannot be quicker, and a
    // number below that means the population was contaminated, not that the crane was fast.
    let one_cycle = (qc_move_s("DS", QC_MOVE_S_FALLBACK.0) + qc_move_s("LD", QC_MOVE_S_FALLBACK.1)) as f64 / 2.0;
    let (bay_change_s, bay_scope, bay_n, bay_shorth) = match bay_gap {
        Some((Some(shorth), n)) if n >= BAY_MIN_SAMPLES && shorth - one_cycle >= 30.0 => {
            ((shorth - one_cycle).round() as i64, "window", n, Some(shorth.round() as i64))
        }
        Some((s, n)) => (BAY_CHANGE_S, "constant fallback", n, s.map(|v| v.round() as i64)),
        None => (BAY_CHANGE_S, "constant fallback", 0, None),
    };

    // Keyed by lowercased job type so a consumer can look up exactly the service it needs.
    let mut yc_service = serde_json::Map::new();
    let mut yc_sample = serde_json::Map::new();
    for (jt, p10, p50, p90, n) in &yc {
        let r = |v: &Option<f64>| v.map(|x| x.round() as i64);
        yc_service.insert(jt.to_lowercase(), json!([r(p10), r(p50), r(p90)]));
        yc_sample.insert(jt.to_lowercase(), json!(n));
    }
    let yc_n = |jt: &str| yc.iter().find(|r| r.0 == jt).map_or(0, |r| r.4);

    let emulator_out = json!({
        "qc_move_s": {
            "ds": qc_move_s("DS", QC_MOVE_S_FALLBACK.0),
            "ld": qc_move_s("LD", QC_MOVE_S_FALLBACK.1),
        },
        "yc_service": yc_service,   // per job type: [p10, p50, p90] seconds
        "hatch_s": { "ds": HATCH_S_DS, "ld": HATCH_S_LD },
        "bay_change_s": bay_change_s,
        "drive_speed_ms": DRIVE_SPEED_MS,
        // Every value says where it came from and, crucially, whether it describes THIS window.
        "_provenance": {
            "window": [ws.to_rfc3339(), we.to_rfc3339()],
            "qc_move_s": {
                "source": "gap between one crane's consecutive completions, capped, per-crane median then averaged",
                // Per job type, because a window can measure one and fall back for the other.
                "scope": { "ds": qc_scope("DS"), "ld": qc_scope("LD") },
                "cap_s": [QC_GAP_CAP_S.0, QC_GAP_CAP_S.1],
                "min_gaps_per_crane": QC_MIN_GAPS_PER_CRANE,
                "min_cranes": QC_MIN_CRANES,
                "cranes": { "ds": qc_cranes("DS"), "ld": qc_cranes("LD") },
                "samples": { "ds": qc_n("DS"), "ld": qc_n("LD") },
                // Only meaningful where scope fell back to the learner.
                "learner_as_of": qc_as_of.map(|t| t.to_rfc3339()),
            },
            "yc_service": {
                "source": "rtg_move_log", "scope": "window",
                "cap_s": [YC_CAP_S.0, YC_CAP_S.1],
                "samples": yc_sample,
            },
            "hatch_s": { "source": "research-log measurement", "scope": "constant" },
            "bay_change_s": {
                "source": "densest half of crane bay-to-bay gaps, minus one container cycle",
                "scope": bay_scope,
                "transitions": bay_n,
                "gap_shorth_s": bay_shorth,      // before the cycle subtraction
                "cycle_subtracted_s": one_cycle.round() as i64,
                // The median of the same gaps is far higher because it carries truck wait; the
                // densest region is used precisely to keep dispatch delay OUT of an equipment number.
                "excludes": "single-move visits on either side (45% of raw visits; a stray box mid-bay is not a gantry move)",
            },
            "drive_speed_ms": { "source": "GPS motion split, stopped segments excluded", "scope": "constant", "kmh": 22.8 },
            "note": "twin lifts are per-move in the work list (twin_group/is_twin), not an emulator ratio. containers[].service_s is the raw observed COMP-ST and is a record of what happened, not this model parameter.",
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
        // Emulator backing samples. qc_* counts the LEARNER's samples (a snapshot, not this
        // window); yc_* counts this window's yard-crane services.
        "qc_learn_sample": qc_n("DS") + qc_n("LD"), "yc_sample": yc_n("DS") + yc_n("LD"),
        // Landside (gate) — mirrors scenario_out.landside so the UI can show quay vs road at a
        // glance. slot_pct is the share of gate moves whose yard slot could be reconstructed.
        "gate_in": gate_in, "gate_out": gate_out, "gate_trucks": gate_trucks,
        "gate_slot_pct": if gate_in + gate_out > 0 { gate_slots * 100 / (gate_in + gate_out) } else { 0 },
        // Share of gate moves that carry the truck's own gate-transaction time (the collector
        // walks the local stream forward, so older windows fill in as it catches up).
        "gate_time_pct": if gate_in + gate_out > 0 { gate_ts_known * 100 / (gate_in + gate_out) } else { 0 },
        // Road volume in TEU, and the share of moves it is computed over. A TEU total read without
        // this coverage understates the window by exactly the boxes we have not met yet, and looks
        // like a quiet shift rather than a partial answer. Windows before the size collector started
        // sit near 50% (the manifest seed); after it, ~95%.
        "teu_in": landside.get("teu_in").cloned().unwrap_or(json!(0)),
        "teu_out": landside.get("teu_out").cloned().unwrap_or(json!(0)),
        "teu_known_pct": if gate_in + gate_out > 0 { lnd("size_known") * 100 / (gate_in + gate_out) } else { 0 },
        // Equipment deployment — how much machinery the window actually used.
        "qc_spans": qc_spans, "rtg_spans": rtg_spans, "peak_trucks": peak_trucks,
        // ★Trust signal for cranes[]. A quay move can only enter the work list if it carries a bay
        // label and a vessel, and labels only exist from 2026-07-30 02:12Z — so an older window
        // silently yields an almost empty crane list that reads as a quiet terminal rather than as
        // missing data. Read queue_pct BEFORE reading cranes[]: below ~99 the window predates the
        // labels and the scenario is not usable as a replay.
        "qc_moves": qc_moves_total,
        "qc_queued": qc_moves_queued,
        "queue_pct": if qc_moves_total > 0 { qc_moves_queued * 100 / qc_moves_total } else { 0 },
        "crane_queues": cranes.as_array().map_or(0, |a| {
            a.iter().map(|c| c.get("queue").and_then(Value::as_array).map_or(0, Vec::len) as i64).sum()
        }),
        // How many vessels carry a departure baseline. Without it a run cannot be scored at all —
        // "finished earlier" needs something to be earlier than — so this belongs next to the other
        // trust signals rather than being discovered as a null halfway through an analysis.
        "vessels_with_eta": vessels.as_array().map_or(0, |a| {
            a.iter()
                .filter(|v| {
                    v.get("schedule")
                        .and_then(|s| s.get("est_depart_ts"))
                        .is_some_and(|t| !t.is_null())
                })
                .count() as i64
        }),
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
