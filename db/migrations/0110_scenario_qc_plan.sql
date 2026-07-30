-- Archive the quay-crane work plan (stowage queue) per vessel call, so a past window can be replayed
-- as "here is what was left to do at T0" instead of "here is what happened, timestamps and all".
--
-- WHY THIS EXISTS. The simulator spec asks for cranes[].queue[{bay,dh,job,qty}] and expects the QC
-- module to walk that queue and decide WHEN each lift happens — that is the only shape in which a
-- different dispatch policy can produce a different outcome. Today's output is the opposite: the
-- actual moves with their real timestamps baked in. The plan itself lives in TOSADM.JOB_QUEUE_SCHEDULE
-- and reaches us via public.live_workqueue, which the extractor DELETEs and refills every 90s — so we
-- have the present plan and no history. This table is that history.
--
-- WHAT WE MEASURED BEFORE WRITING IT (2026-07-30, and it changed the design three times):
--
-- 1. "Capture once at issue time and seal" DOES NOT WORK. Two snapshots 4m50s apart: MTXT 009/2026,
--    still 10.1h before berthing with comp_qty=0 everywhere, moved 1471 -> 1465. And at 1471 it
--    matched that call's declared disvan+loadvan EXACTLY — i.e. it passed a "plan is complete now"
--    test and then changed anyway. Row-level: 12 edits in 3m10s across 1115 rows. So a single sealed
--    snapshot freezes a superseded number. Hence rev: append the CHANGED ROWS, keep the history.
--
-- 2. comp_qty MUST NOT trigger a revision. It is progress, not plan, and it is the single largest
--    source of churn. Only (qc, seq, total_qty, disload) define a new rev. comp_qty is stored as
--    comp_qty_at_capture for provenance and nothing else.
--
-- 3. A ROW VANISHING IS NOT A DELETION. Rows leave live_workqueue for two indistinguishable reasons:
--    removed from the plan, or completed and then dropped by the extractor's 6-hour exclusion
--    (crates/extractor/sql/workqueue.sql). Measured: SISF 006/2026 at 16.5h after berthing shows 8
--    rows / 381 qty against a declared 1189 — 32% of its own plan. Recording that as a revision
--    writes the false history "the plan shrank to a third". So this table never records absence, and
--    the collector stops revising a call once it berths (sealed_ts), which is when the erosion starts.
--
-- 4. THE PLAN IS NOT LOST IF WE MISS IT. Oracle keeps JOB_QUEUE_SCHEDULE for 6+ months (CRE_DT seen
--    back to 2026-02-04, DELT_FLG never used). Asking by (vessel, voyage) with the two time
--    predicates removed returned 100% of the plan for 12 of 12 calls, including one that had departed
--    74.7h earlier — totals matching declared disvan+loadvan exactly. That query is also CHEAPER than
--    the live one, which has no index on UPD_DT and full-scans ~458MB every 90s. So past calls are
--    backfillable and this archive is not a race against the clock.
--
-- 5. total_qty IS A CONTAINER COUNT, NOT A LIFT COUNT. On 18 of 20 cleanly-completed bays it equals
--    the move count exactly, while distinct lifts are systematically fewer (40 -> 24, 35 -> 20,
--    66 -> 42; twin rate 18.2%). Treating qty as truck trips overstates yard round-trip demand by up
--    to 20%. The twin ratio is not in the plan and has to be learned from actuals.
--
-- 6. plan_qty IS NOT THE REMAINDER. Of 1,077 live rows: 619 have plan_qty=total_qty, 739 have
--    plan_qty=total-comp, 413 have plan_qty=0 — and all 59 partially-worked rows have plan_qty=0.
--    Remaining is total_qty - comp_qty. plan_qty is kept only so the archive is faithful.
--
-- 7. SENTINELS MUST BE EXCLUDED. live_workqueue carries vessel='RHXX' (voyage 001/2026) rows whose
--    qc is not a crane at all ('GRP A', 'POOLCS', 'CNSOL2', 'VC101'…) and whose queuename is a yard
--    form ('YY2-Y'), totalling 15% of all qty. Joining to live_vessel_schedule on (vessel, voyage)
--    drops exactly those rows and nothing else (verified: same 24 rows as the disload IS NULL rule).

CREATE TABLE IF NOT EXISTS scenario.qc_plan (
    vessel               text        NOT NULL,
    voyage               text        NOT NULL,
    qc                   text        NOT NULL, -- quay crane the bay-job is assigned to
    queuename            text        NOT NULL, -- "18H-L" = 40ft bay 18 / Hold / Load. 6.5% carry a
                                               -- trailing split digit ("26H-L1"): one bay worked in
                                               -- pieces, revisited at non-contiguous seq. bays[] must
                                               -- SUM these; a crane queue must keep them as separate
                                               -- visits (each visit costs a gantry move / hatch).
    rev                  int         NOT NULL, -- 1-based; a new rev only on a real plan change
    disload              text,                 -- 'D' discharge / 'L' load
    seq                  int,                  -- order WITHIN (vessel, crane); NOT global, and it does
                                               -- not order one crane's work across two vessels
    total_qty            int,                  -- CONTAINERS (see note 5). Not a hard cap either:
                                               -- one sampled bay ran 39 moves against total_qty 38.
    plan_qty             int,                  -- NOT the remainder (note 6) — fidelity only
    comp_qty_at_capture  int,                  -- progress at capture; never triggers a rev (note 2)
    captured_at          timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (vessel, voyage, qc, queuename, rev)
);
CREATE INDEX IF NOT EXISTS scenario_qc_plan_call_idx ON scenario.qc_plan (vessel, voyage);

-- Per-call header: what we knew, when we stopped, and whether the archive is trustworthy.
-- `sealed_reason` is the honesty field. A call first seen AFTER it berthed cannot be archived from
-- the local table at all (erosion has already started), so it is recorded as needing the Oracle
-- backfill rather than silently stored as a short plan.
CREATE TABLE IF NOT EXISTS scenario.qc_plan_call (
    vessel        text        NOT NULL,
    voyage        text        NOT NULL,
    first_seen_ts timestamptz NOT NULL DEFAULT now(),
    last_rev_ts   timestamptz,
    revs          int         NOT NULL DEFAULT 0, -- total rev rows appended for this call
    rows_latest   int         NOT NULL DEFAULT 0, -- distinct (qc, queuename) at last capture
    qty_latest    int,                            -- sum(total_qty) at last capture
    estber_ts     timestamptz,
    actber_ts     timestamptz,
    disvan        int,                            -- declared discharge count for the call
    loadvan       int,                            -- declared load count
    sealed_ts     timestamptz,
    sealed_reason text,        -- 'berthed' | 'missed_preberth' | 'backfilled'
    source        text        NOT NULL DEFAULT 'live', -- 'live' | 'oracle_backfill'
    PRIMARY KEY (vessel, voyage)
);
CREATE INDEX IF NOT EXISTS scenario_qc_plan_call_open_idx
    ON scenario.qc_plan_call (sealed_ts) WHERE sealed_ts IS NULL;

-- Coverage view for the monitor: does what we archived match what the call declared? A call whose
-- archived discharge/load totals fall short of disvan/loadvan lost plan rows somewhere, and until
-- now nothing anywhere compared the two — the loss was completely silent.
CREATE OR REPLACE VIEW scenario.qc_plan_coverage AS
SELECT c.vessel, c.voyage, c.actber_ts, c.sealed_ts, c.sealed_reason, c.source,
       c.disvan, c.loadvan,
       p.arch_d, p.arch_l,
       CASE WHEN coalesce(c.disvan, 0) > 0 THEN round(100.0 * coalesce(p.arch_d, 0) / c.disvan, 1) END AS pct_d,
       CASE WHEN coalesce(c.loadvan, 0) > 0 THEN round(100.0 * coalesce(p.arch_l, 0) / c.loadvan, 1) END AS pct_l
  FROM scenario.qc_plan_call c
  LEFT JOIN (
      -- latest rev per (qc, queuename), summed by direction
      SELECT vessel, voyage,
             sum(total_qty) FILTER (WHERE disload = 'D') AS arch_d,
             sum(total_qty) FILTER (WHERE disload = 'L') AS arch_l
        FROM (
            SELECT DISTINCT ON (vessel, voyage, qc, queuename) vessel, voyage, disload, total_qty
              FROM scenario.qc_plan
             ORDER BY vessel, voyage, qc, queuename, rev DESC
        ) latest
       GROUP BY vessel, voyage
  ) p ON p.vessel = c.vessel AND p.voyage = c.voyage;
