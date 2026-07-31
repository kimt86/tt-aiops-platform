-- The subtraction, as a function and nothing else yet: what quay work was still OUTSTANDING at an
-- instant. Deliberately not wired into any output — the scenario assembler picks it up in a later
-- change, once the plan backfill has drained. Building it read-only first means the arithmetic can
-- be checked against reality before anything downstream can be quietly wrong.
--
--     remaining(T0) = archived plan  −  what the move log says was already done before T0
--
-- FOUR FOLDS, IN THIS ORDER, AND EVERY ONE OF THEM WAS MEASURED
--
-- 1. Latest revision, as of T0. A call is revised while it is alongside (measured: 682 of 1,657
--    queues revised, up to 13 times, 118 of them changing quantity). Reading rev DESC gives the plan
--    as last known; the captured_at <= T0 bound keeps a scenario from seeing edits made after the
--    instant it claims to depict.
--
-- 2. FOLD THE CRANE AWAY. This is the fold that matters most and the one that is easiest to skip.
--    The same (vessel, voyage, queuename) can carry several crane rows — crane reassignment leaves
--    the old one behind — so summing without folding multiplies the plan. Measured on 17 live calls:
--    folded, 17/17 discharge and 16/17 load land within 5% of the call's declared count; unfolded,
--    9/17 and 8/17. Which crane's row survives does not matter: of 513 multi-crane keys only 57
--    disagree on quantity at all, and MAX−MIN over all of them is 437 containers (0.5%). MAX is used
--    because it is deterministic. The crane list still ships, as an initial assignment.
--    The plan's crane is NOT a join key for the same reason plus one more: it agrees with the crane
--    that actually worked only 99.10% of the time, and the plan contains placeholder cranes
--    (DC01..DC05, CR4) on 139 rows / 4,151 containers across 8 calls.
--
-- 3. NORMALISE THE SPLIT SUFFIX. 6.5% of queue names carry a trailing digit ("26H-L1") — one bay
--    worked in pieces. Joining raw leaves DS 100% / LD 97.67% matched; stripping the suffix gives
--    100% / 100%. The residue was real: a call sealed as "30H-L" had its queue split into "30H-L1"
--    afterwards, so moves carried a name the plan never had. Left unnormalised those containers are
--    double-counted — still outstanding in the plan AND unmatched in the moves.
--
-- 4. MATCH THE MOVES ON (vessel, voyage, normalised name), containers not lifts. The plan counts
--    containers; so does the move log; twin lifts (18.2%) therefore cancel out. Verified against
--    TOS's own comp_qty: difference 0.
--
-- WHAT THIS FUNCTION DOES NOT DO, ON PURPOSE
--   * It does not fall back to act_dt for windows before the move log carried queue names
--     (2026-07-30 02:12:08Z). That path needs three corrections shipped together or it overstates
--     what is left by 20-30%: 93 archived rows are complete but have no act_dt (13-21% phantom), and
--     act_dt is per-queue binary so an in-flight queue reads as wholly untouched (a further 7-12%).
--     Until then, a caller asking about an earlier instant gets a low resolved_pct and must say so
--     rather than a confident wrong number.
--   * It does not invent rows for calls with no archived plan. It reports them (see the companion
--     view) because the honest failure here is UNDER-reporting: a scenario missing a call is a quiet
--     half-empty terminal, which looks perfectly normal.

CREATE OR REPLACE FUNCTION scenario.qc_plan_remaining(t0 timestamptz)
RETURNS TABLE (
    vessel      text,
    voyage      text,
    qkey        text,   -- queue name with any split suffix removed
    bay         int,    -- 40ft bay label. NOTE 20ft bays nest under it (odd bays fold to the even one)
    dh          text,   -- 'D' deck / 'H' hold
    job         text,   -- 'D' discharge / 'L' load
    planned     int,    -- containers, per the plan
    done_before int,    -- containers the move log says were completed strictly before t0
    remaining   int,    -- planned - done_before, NOT clamped: negatives are a signal, see below
    visits      int,    -- how many split pieces this bay was worked in (each costs a gantry move)
    qcs         text[], -- cranes the plan assigned. Initial assignment only, not authoritative
    min_seq     int,    -- earliest plan sequence among the folded rows, for ordering a crane's work
    -- ★THE GUARD. False means the move log cannot say what this call had already done, so
    -- `remaining` is the whole plan by default — which reads as "nothing has been worked yet" and is
    -- the single most dangerous output this function can produce. It happens whenever a call was
    -- worked before the move log started carrying queue names (2026-07-30 02:12:08Z, mig 0109):
    -- every one of its moves is filtered out, done_before comes back 0, and a departed ship looks
    -- like it is waiting to start. Caught by the invariant test — a departed call evaluated after
    -- its own departure must have remaining 0, and before this column it did not.
    -- A caller MUST drop or flag these rows. Do not clamp, do not guess.
    done_known  boolean
) LANGUAGE sql STABLE AS $fn$
WITH latest AS (
    -- 1: the newest revision of each (call, crane, queue) known at t0
    SELECT DISTINCT ON (p.vessel, p.voyage, p.qc, p.queuename)
           p.vessel, p.voyage, p.qc, p.queuename, p.disload, p.seq, p.total_qty
      FROM scenario.qc_plan p
     WHERE p.captured_at <= t0
     ORDER BY p.vessel, p.voyage, p.qc, p.queuename, p.rev DESC
),
folded AS (
    -- 2: collapse the crane dimension so a reassigned bay is counted once
    SELECT vessel, voyage, queuename, max(disload) AS disload,
           max(total_qty) AS total_qty, min(seq) AS min_seq
      FROM latest
     GROUP BY vessel, voyage, queuename
),
cranes AS (
    -- The crane list is gathered straight off `latest` at the normalised key, rather than being
    -- carried through the folds — array-of-array flattening is the kind of thing that works until
    -- one call has a different number of cranes than another.
    SELECT vessel, voyage,
           regexp_replace(queuename, '^([0-9]+[HD]-[DL])[0-9]+$', '\1') AS qkey,
           array_agg(DISTINCT qc ORDER BY qc) AS qcs
      FROM latest
     GROUP BY 1, 2, 3
),
plan AS (
    -- 3: collapse split pieces of one bay, keeping the visit count
    SELECT f.vessel, f.voyage,
           regexp_replace(f.queuename, '^([0-9]+[HD]-[DL])[0-9]+$', '\1') AS qkey,
           max(f.disload) AS job,
           sum(f.total_qty)::int AS planned,
           count(*)::int AS visits,
           min(f.min_seq) AS min_seq
      FROM folded f
     GROUP BY 1, 2, 3
),
done AS (
    -- 4: containers completed before t0, keyed the same way. The vessel falls back to move_hist for
    -- moves predating the columns landing on the log itself (mig 0109).
    SELECT COALESCE(q.vessel, vv.vessel) AS vessel,
           COALESCE(q.voyage, vv.voyage) AS voyage,
           regexp_replace(q.queuename, '^([0-9]+[HD]-[DL])[0-9]+$', '\1') AS qkey,
           count(*)::int AS n
      FROM public.qc_move_log q
      LEFT JOIN LATERAL (
            SELECT m.vessel, m.voyage FROM scenario.move_hist m
             WHERE m.contno = q.contno AND m.jobtype = q.jobtype
             ORDER BY abs(extract(epoch FROM m.comp_ts - q.comp_ts))
             LIMIT 1
      ) vv ON q.vessel IS NULL
     WHERE q.comp_ts < t0
       AND q.jobtype IN ('DS', 'LD')     -- MI/MO carry yard-internal ids, a different grammar
       AND q.queuename IS NOT NULL
     GROUP BY 1, 2, 3
),
measurable AS (
    -- Per call: does the move log carry ANY labelled move for it? If not, its done side is unknown
    -- rather than zero. Deliberately not bounded by t0 — a call with labelled moves only AFTER t0 is
    -- still measurable at t0 (the answer is genuinely "none done yet"), whereas a call with no
    -- labelled moves at all is outside what the log can answer.
    SELECT COALESCE(q.vessel, vv.vessel) AS vessel,
           COALESCE(q.voyage, vv.voyage) AS voyage
      FROM public.qc_move_log q
      LEFT JOIN LATERAL (
            SELECT m.vessel, m.voyage FROM scenario.move_hist m
             WHERE m.contno = q.contno AND m.jobtype = q.jobtype
             ORDER BY abs(extract(epoch FROM m.comp_ts - q.comp_ts))
             LIMIT 1
      ) vv ON q.vessel IS NULL
     WHERE q.queuename IS NOT NULL AND q.jobtype IN ('DS', 'LD')
     GROUP BY 1, 2
)
SELECT p.vessel, p.voyage, p.qkey,
       substring(p.qkey from '^([0-9]+)')::int          AS bay,
       substring(p.qkey from '^[0-9]+([HD])')           AS dh,
       p.job,
       p.planned,
       COALESCE(d.n, 0)                                 AS done_before,
       -- NOT clamped to zero. A negative means the crane worked more containers than the plan said
       -- (seen: a bay planned at 38 that ran 39), and a caller that silently clamps loses the only
       -- signal that its plan and its moves disagree. Clamp at the point of use, and count them.
       p.planned - COALESCE(d.n, 0)                     AS remaining,
       p.visits, c.qcs, p.min_seq,
       (m.vessel IS NOT NULL)                           AS done_known
  FROM plan p
  LEFT JOIN cranes c
         ON c.vessel = p.vessel AND c.voyage = p.voyage AND c.qkey = p.qkey
  LEFT JOIN done d
         ON d.vessel = p.vessel AND d.voyage = p.voyage AND d.qkey = p.qkey
  LEFT JOIN measurable m
         ON m.vessel = p.vessel AND m.voyage = p.voyage
$fn$;

-- Companion: is an answer from the function trustworthy at this instant? The dangerous failure is
-- under-reporting — a call alongside with no archived plan simply is not in the result, and a
-- scenario missing a third of the berth looks exactly like a quiet shift. This makes that visible,
-- and it is what the assembler will surface as scenario quality rather than computing its own.
CREATE OR REPLACE FUNCTION scenario.qc_plan_quality(t0 timestamptz)
RETURNS TABLE (
    voyages_alongside      int,
    voyages_with_plan      int,
    moves_before_with_plan int,
    moves_before_no_plan   int,
    unmatched_move_keys    int,  -- moves whose queue key is absent from a plan we DO hold. Expect 0
    negative_queues        int
) LANGUAGE sql STABLE AS $fn$
WITH al AS (
    SELECT vc.vessel, vc.voyage
      FROM scenario.vessel_call vc
     WHERE vc.actber < t0 AND (vc.actdep IS NULL OR vc.actdep > t0)
),
withplan AS (
    SELECT a.vessel, a.voyage,
           EXISTS (SELECT 1 FROM scenario.qc_plan_call c
                    WHERE c.vessel = a.vessel AND c.voyage = a.voyage AND c.revs > 0) AS has_plan
      FROM al a
),
mv AS (
    SELECT COALESCE(q.vessel, vv.vessel) AS vessel,
           COALESCE(q.voyage, vv.voyage) AS voyage,
           regexp_replace(q.queuename, '^([0-9]+[HD]-[DL])[0-9]+$', '\1') AS qkey
      FROM public.qc_move_log q
      LEFT JOIN LATERAL (
            SELECT m.vessel, m.voyage FROM scenario.move_hist m
             WHERE m.contno = q.contno AND m.jobtype = q.jobtype
             ORDER BY abs(extract(epoch FROM m.comp_ts - q.comp_ts))
             LIMIT 1
      ) vv ON q.vessel IS NULL
     WHERE q.comp_ts < t0 AND q.comp_ts > t0 - interval '7 days'
       AND q.jobtype IN ('DS', 'LD') AND q.queuename IS NOT NULL
)
SELECT
  (SELECT count(*)::int FROM withplan),
  (SELECT count(*)::int FROM withplan WHERE has_plan),
  (SELECT count(*)::int FROM mv JOIN withplan w USING (vessel, voyage) WHERE w.has_plan),
  (SELECT count(*)::int FROM mv JOIN withplan w USING (vessel, voyage) WHERE NOT w.has_plan),
  (SELECT count(*)::int FROM (
      SELECT DISTINCT mv.vessel, mv.voyage, mv.qkey FROM mv
        JOIN withplan w ON w.vessel = mv.vessel AND w.voyage = mv.voyage AND w.has_plan
       WHERE NOT EXISTS (SELECT 1 FROM scenario.qc_plan_remaining(t0) r
                          WHERE r.vessel = mv.vessel AND r.voyage = mv.voyage AND r.qkey = mv.qkey)
   ) z),
  (SELECT count(*)::int FROM scenario.qc_plan_remaining(t0) WHERE remaining < 0)
$fn$;
