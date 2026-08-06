-- Local parity for K_CYCLE (Oracle original: sql/e3b_k_cycle_refined_v2.sql,
-- JOB_ORDER_HISTORY per-container transition span, 2-pass mean+2sd outlier).
-- ⚠ MEANING DIFFERS ON PURPOSE (documented in PLAN-extractor.md 2-2 table): the Oracle
-- original spans JOB_HIST_POINT/SEQNO transitions inside JOB_ORDER_HISTORY (multi-hop,
-- whole calendar day via k_cycle.rs's nightly render_day). The local mirror uses
-- tt_move_log's already-reconciled dispatch_ts->free_ts span (cycle_s, TOS-authoritative
-- per project_tt_move_log), windowed to the caller's shift-to-now range. Parity's whole
-- point here is to measure that gap, not to reproduce it.
-- twin_leg_seq = 1 excludes non-final twin legs (same convention as api/cycles.rs) so a
-- twin trip counts once. avg_transitions has no local analogue (tt_move_log doesn't carry
-- per-hop transition counts) and is omitted.
-- $1,$2 = window start/end (timestamptz UTC), filtered on free_ts (the completion event).
WITH cycles AS (
  SELECT jobtype, cycle_s::float8 AS cycle_sec
    FROM tt_move_log
   WHERE free_ts >= $1 AND free_ts < $2
     AND cycle_s IS NOT NULL
     AND twin_leg_seq = 1
),
stats AS (
  SELECT jobtype,
         count(*)                                                    AS jobs,
         avg(cycle_sec)                                               AS avg_cyc,
         stddev(cycle_sec)                                            AS std_cyc,
         percentile_cont(0.5)  WITHIN GROUP (ORDER BY cycle_sec)      AS med_cyc,
         avg(cycle_sec) + 2 * stddev(cycle_sec)                       AS thr,
         percentile_cont(0.25) WITHIN GROUP (ORDER BY cycle_sec)      AS p25,
         percentile_cont(0.75) WITHIN GROUP (ORDER BY cycle_sec)      AS p75,
         percentile_cont(0.95) WITHIN GROUP (ORDER BY cycle_sec)      AS p95
    FROM cycles
   GROUP BY jobtype
)
SELECT s.jobtype                                                      AS jobtype,
       s.jobs::float8                                                 AS jobs,
       round(s.avg_cyc::numeric, 1)::float8                           AS avg_sec,
       round(s.med_cyc::numeric, 1)::float8                           AS med_sec,
       round(s.std_cyc::numeric, 1)::float8                           AS std_sec,
       round(s.p25::numeric, 1)::float8                                AS p25_sec,
       round(s.p75::numeric, 1)::float8                                AS p75_sec,
       round(s.p95::numeric, 1)::float8                                AS p95_sec,
       round(s.thr::numeric, 1)::float8                                AS outlier_threshold_sec,
       (SELECT count(*) FROM cycles c
         WHERE c.jobtype = s.jobtype AND c.cycle_sec > s.thr)::float8   AS outlier_n
  FROM stats s
 ORDER BY s.jobs DESC
