-- Shadow logger for the candidate-vehicle completion-time predictor. Run ~every 2min. Pure Postgres;
-- never touches dispatch. Steps:
--   (1) log COMMITTED (status='F') pickup events with a provisional prediction
--   (2) backfill from the authoritative physical trip (tt_move_log): actual free, TOS twin count,
--       and the prediction recomputed with the correct container count
--   (3) dedup twin legs (a twin logs one row per leg; keep the trip's first)
--   (4) prune >30d
-- Window :mins (default 90). status='F' avoids the provisional M->F comp_ts double-log; the move-log
-- carries no twin flag, so n_containers is a provisional 1 at insert and corrected at backfill.
\if :{?mins}
\else
  \set mins 90
\endif

-- (1) new committed pickups -> provisional (nc=1) prediction
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
-- max(free_ts) over the trip's legs; nc = twin_group_size; prediction recomputed with the correct nc.
-- Deterministic (one trip per key); shadow pickup matched within [-60s, +5min] of the trip's first
-- pickup (covers LD twins whose 2nd pickup is >5min after the first).
WITH trips AS (
  SELECT ytno, min(pickup_ts) AS pk, max(free_ts) AS fr,
         least(coalesce(max(twin_group_size), 1), 2) AS nc
  FROM tt_move_log
  WHERE free_ts >= now() - interval '6 hours'
  GROUP BY ytno, dispatch_ts
),
best AS (   -- exactly ONE trip per pending shadow row: nearest first-pickup within [-60s, +5min]
  SELECT DISTINCT ON (s.ytno, s.pickup_ts) s.ytno, s.pickup_ts, t.fr, t.nc
  FROM cycle_pred_shadow s
  JOIN trips t ON t.ytno = s.ytno
    AND s.pickup_ts >= t.pk - interval '60 s'
    AND s.pickup_ts <= t.pk + interval '5 min'
  WHERE s.actual_free_at IS NULL
  ORDER BY s.ytno, s.pickup_ts, abs(extract(epoch FROM s.pickup_ts - t.pk))
)
UPDATE cycle_pred_shadow s
SET actual_free_at     = b.fr,
    actual_remaining_s = extract(epoch FROM b.fr - s.pickup_ts)::int,
    n_containers       = b.nc,
    pred_remaining_s   = lr.remaining_p50,
    pred_free_at       = s.pickup_ts + make_interval(secs => lr.remaining_p50),
    err_s              = lr.remaining_p50 - extract(epoch FROM b.fr - s.pickup_ts)::int
FROM best b
JOIN learn_cycle_remaining lr
  ON lr.dest_inflight_bucket = -1 AND lr.n_containers = b.nc
WHERE s.ytno = b.ytno AND s.pickup_ts = b.pickup_ts AND lr.jobtype = s.jobtype;

-- (3) dedup twin legs: keep the trip's first pickup, drop later legs that backfilled to the same free
DELETE FROM cycle_pred_shadow s USING (
  SELECT ytno, actual_free_at, min(pickup_ts) AS keep
  FROM cycle_pred_shadow WHERE actual_free_at IS NOT NULL
  GROUP BY ytno, actual_free_at
) d
WHERE s.ytno = d.ytno AND s.actual_free_at = d.actual_free_at AND s.pickup_ts <> d.keep;

-- (4) prune
DELETE FROM cycle_pred_shadow WHERE captured_at < now() - interval '30 days';
