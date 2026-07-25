-- Shadow logger for the candidate-vehicle completion-time predictor. Run ~every 2min. Pure Postgres;
-- never touches dispatch. Steps:
--   (1) log COMMITTED (status='F') pickup events with a provisional prediction
--   (2) backfill from the authoritative physical trip (tt_move_log): actual free, TOS twin count,
--       LD destination-QC in-flight (as-of pickup), and the prediction recomputed with the correct
--       container count AND in-flight bucket (baseline kept alongside for the live A/B)
--   (3) dedup twin legs (a twin logs one row per leg; keep the trip's first)
--   (4) prune >30d
-- Window :mins (default 90). status='F' avoids the provisional M->F comp_ts double-log; the move-log
-- carries no twin flag, so n_containers is a provisional 1 at insert and corrected at backfill.
\if :{?mins}
\else
  \set mins 90
\endif

-- (1) new committed pickups -> provisional (nc=1, baseline bucket -1) prediction
WITH pickups AS (
  SELECT trk_id AS ytno, contno, 'DS'::text AS jobtype, 'qc'::text AS src, comp_ts AS pickup_ts
    FROM qc_move_log  WHERE jobtype='DS' AND status='F' AND comp_ts >= now() - make_interval(mins => :mins)
  UNION ALL
  SELECT trk_id, contno, 'LD', 'rtg', comp_ts
    FROM rtg_move_log WHERE jobtype='LD' AND status='F' AND comp_ts >= now() - make_interval(mins => :mins)
)
INSERT INTO cycle_pred_shadow
  (ytno, pickup_ts, jobtype, n_containers, src, contno, pred_remaining_s, pred_free_at, captured_at)
SELECT p.ytno, p.pickup_ts, p.jobtype, 1, p.src, p.contno,
       lr.remaining_p50, p.pickup_ts + make_interval(secs => lr.remaining_p50), now()
FROM pickups p
JOIN learn_cycle_remaining lr
  ON lr.jobtype = p.jobtype AND lr.n_containers = 1 AND lr.dest_inflight_bucket = -1
ON CONFLICT (ytno, pickup_ts) DO NOTHING;

-- (2) backfill from the authoritative physical trip (group by ytno,dispatch_ts): actual free =
-- max(free_ts) over the trip's legs; nc = twin_group_size; dest QC = free_crane (LD); prediction
-- recomputed with the correct nc AND (LD) the destination-QC in-flight bucket. Deterministic (one trip
-- per key); shadow pickup matched within [-60s, +5min] of the trip's first pickup.

-- (2a) match exactly ONE physical trip per pending shadow row
DROP TABLE IF EXISTS bf_match;
CREATE TEMP TABLE bf_match AS
WITH trips AS (
  SELECT ytno, dispatch_ts, min(pickup_ts) AS pk, max(free_ts) AS fr,
         least(coalesce(max(twin_group_size), 1), 2) AS nc,
         max(free_crane) AS dest_qc, max(jobtype) AS jobtype
  FROM tt_move_log
  WHERE free_ts >= now() - interval '6 hours'
  GROUP BY ytno, dispatch_ts
)
SELECT DISTINCT ON (s.ytno, s.pickup_ts)
       s.ytno, s.pickup_ts, s.jobtype AS s_jobtype,
       t.dispatch_ts AS t_disp, t.fr, t.nc, t.dest_qc
FROM cycle_pred_shadow s
JOIN trips t ON t.ytno = s.ytno
  AND s.pickup_ts >= t.pk - interval '60 s'
  AND s.pickup_ts <= t.pk + interval '5 min'
WHERE s.actual_free_at IS NULL
ORDER BY s.ytno, s.pickup_ts, abs(extract(epoch FROM s.pickup_ts - t.pk));

-- (2b) LD destination-QC in-flight at each matched pickup (as-of; # OTHER LD trips dispatched<=pickup
-- and not-yet-free to the same dest QC in the 6h window). Bucket CASE kept IN SYNC with
-- scripts/populate_learn_cycle_remaining.sql. DS rows get no bf_if row → bucket -1 (baseline).
DROP TABLE IF EXISTS bf_if;
CREATE TEMP TABLE bf_if AS
WITH ld AS (
  SELECT ytno, dispatch_ts, min(pickup_ts) AS pk, max(free_ts) AS fr, max(free_crane) AS dest_qc
  FROM tt_move_log
  WHERE jobtype='LD' AND free_ts >= now() - interval '6 hours' AND free_crane IS NOT NULL
  GROUP BY ytno, dispatch_ts
),
cnt AS (
  SELECT m.ytno, m.pickup_ts,
         (SELECT count(*) FROM ld o
           WHERE o.dest_qc = m.dest_qc
             AND o.dispatch_ts <= m.pickup_ts
             AND o.fr        >  m.pickup_ts
             AND NOT (o.ytno = m.ytno AND o.dispatch_ts = m.t_disp)) AS in_flight
  FROM bf_match m
  WHERE m.s_jobtype = 'LD' AND m.dest_qc IS NOT NULL
)
SELECT ytno, pickup_ts, in_flight,
       CASE WHEN in_flight <= 2 THEN 0 WHEN in_flight <= 4 THEN 1 WHEN in_flight <= 6 THEN 2
            WHEN in_flight <= 8 THEN 3 WHEN in_flight <= 10 THEN 4 WHEN in_flight <= 13 THEN 5
            ELSE 6 END AS bucket
FROM cnt;

-- (2c) apply: actual free + nc + dest_qc/in-flight/bucket; pred = bucketed p50 (LD, if that bucket is
-- loaded) else baseline; keep baseline pred + error alongside for the standing live A/B.
UPDATE cycle_pred_shadow s SET
  actual_free_at       = m.fr,
  actual_remaining_s   = extract(epoch FROM m.fr - s.pickup_ts)::int,
  n_containers         = m.nc,
  dest_qc              = m.dest_qc,
  dest_inflight        = f.in_flight,
  dest_inflight_bucket = coalesce(f.bucket, -1),
  pred_baseline_s      = lr_base.remaining_p50,
  err_baseline_s       = lr_base.remaining_p50 - extract(epoch FROM m.fr - s.pickup_ts)::int,
  pred_remaining_s     = coalesce(lr_bkt.remaining_p50, lr_base.remaining_p50),
  pred_free_at         = s.pickup_ts + make_interval(secs => coalesce(lr_bkt.remaining_p50, lr_base.remaining_p50)),
  err_s                = coalesce(lr_bkt.remaining_p50, lr_base.remaining_p50) - extract(epoch FROM m.fr - s.pickup_ts)::int
FROM bf_match m
LEFT JOIN bf_if f ON f.ytno = m.ytno AND f.pickup_ts = m.pickup_ts
LEFT JOIN learn_cycle_remaining lr_base
       ON lr_base.jobtype = m.s_jobtype AND lr_base.n_containers = m.nc AND lr_base.dest_inflight_bucket = -1
LEFT JOIN learn_cycle_remaining lr_bkt
       ON m.s_jobtype = 'LD' AND lr_bkt.jobtype = 'LD' AND lr_bkt.n_containers = m.nc
      AND lr_bkt.dest_inflight_bucket = f.bucket
WHERE s.ytno = m.ytno AND s.pickup_ts = m.pickup_ts;

DROP TABLE IF EXISTS bf_if;
DROP TABLE IF EXISTS bf_match;

-- (3) dedup twin legs: keep the trip's first pickup, drop later legs that backfilled to the same free
DELETE FROM cycle_pred_shadow s USING (
  SELECT ytno, actual_free_at, min(pickup_ts) AS keep
  FROM cycle_pred_shadow WHERE actual_free_at IS NOT NULL
  GROUP BY ytno, actual_free_at
) d
WHERE s.ytno = d.ytno AND s.actual_free_at = d.actual_free_at AND s.pickup_ts <> d.keep;

-- (4) prune
DELETE FROM cycle_pred_shadow WHERE captured_at < now() - interval '30 days';
