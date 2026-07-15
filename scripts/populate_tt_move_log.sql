-- Populate / refresh tt_move_log by joining already-extracted Postgres tables (no Oracle load).
-- Idempotent: ON CONFLICT (ytno, contno, dispatch_ts) DO NOTHING → safe on a timer for forward refresh.
-- Window parameterized by :days (default 30 for backfill; use 2 for refresh so both twin legs land together).
\if :{?days}
\else
  \set days 30
\endif

WITH base AS (
  -- one candidate per (truck, container, dispatch); pick the QC handover of the SAME job instance
  -- (within 3h of dispatch); on a (contno,ytno) repeat, the QC event nearest the RTG event, then
  -- earliest comp_ts as a deterministic tie-break.
  SELECT DISTINCT ON (t.ytno, t.contno, t.dis_ts)
    t.ytno, t.contno, t.jobtype,
    t.dis_ts                                                    AS dispatch_ts,
    CASE WHEN t.jobtype = 'LD' THEN t.comp_ts ELSE q.comp_ts END AS pickup_ts,
    CASE WHEN t.jobtype = 'LD' THEN q.comp_ts ELSE t.comp_ts END AS free_ts,
    CASE WHEN t.jobtype = 'LD' THEN t.armgc   ELSE q.machno  END AS pickup_crane,
    CASE WHEN t.jobtype = 'LD' THEN q.machno  ELSE t.armgc   END AS free_crane,
    q.status
  FROM tos_handover_label t
  JOIN qc_move_log q
    ON  q.contno  = t.contno
    AND q.trk_id  = t.ytno
    AND q.jobtype = t.jobtype
    AND q.comp_ts >= t.dis_ts
    AND q.comp_ts <  t.dis_ts + interval '3 hours'
  WHERE t.jobtype IN ('LD','DS')
    AND t.dis_ts IS NOT NULL
    AND t.ytno   IS NOT NULL
    AND t.comp_ts > now() - make_interval(days => :days)
  ORDER BY t.ytno, t.contno, t.dis_ts,
           abs(EXTRACT(EPOCH FROM (q.comp_ts - t.comp_ts))),    -- QC event nearest the RTG event
           q.comp_ts                                            -- deterministic tie-break
),
filt AS (
  SELECT *,
    round(EXTRACT(EPOCH FROM (free_ts   - dispatch_ts)))::int AS cycle_s,
    round(EXTRACT(EPOCH FROM (pickup_ts - dispatch_ts)))::int AS empty_s,
    round(EXTRACT(EPOCH FROM (free_ts   - pickup_ts)))::int   AS laden_s
  FROM base
  WHERE free_ts    > dispatch_ts                     -- sane ordering
    AND pickup_ts >= dispatch_ts                     -- (>= matches the join boundary; ~0 = pre-staged)
    AND free_ts    > pickup_ts
    AND free_ts    - dispatch_ts < interval '3 hours' -- reject cross-job mismatches
),
tw AS (
  SELECT *,
    count(*)     OVER (PARTITION BY ytno, dispatch_ts)                     AS twin_group_size,
    row_number() OVER (PARTITION BY ytno, dispatch_ts ORDER BY free_ts, contno) AS twin_leg_seq
  FROM filt
)
INSERT INTO tt_move_log
  (ytno, contno, jobtype, dispatch_ts, pickup_ts, free_ts, pickup_crane, free_crane,
   cycle_s, empty_s, laden_s, status, is_twin, twin_group_size, twin_leg_seq, business_date, shift)
SELECT
  ytno, contno, jobtype, dispatch_ts, pickup_ts, free_ts, pickup_crane, free_crane,
  cycle_s, empty_s, laden_s, status,
  (twin_group_size > 1)                                     AS is_twin,
  twin_group_size, twin_leg_seq,
  (free_ts AT TIME ZONE 'Asia/Kuala_Lumpur')::date          AS business_date,
  CASE WHEN EXTRACT(HOUR FROM (free_ts AT TIME ZONE 'Asia/Kuala_Lumpur')) BETWEEN 6 AND 17
       THEN 'D' ELSE 'N' END                                AS shift
FROM tw
ON CONFLICT (ytno, contno, dispatch_ts) DO NOTHING;
