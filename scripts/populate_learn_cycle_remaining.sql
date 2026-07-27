-- Refresh the baseline candidate-vehicle completion-time predictor (learn_cycle_remaining,
-- dest_inflight_bucket = -1) from tt_move_log over a rolling window. Pure Postgres, no Oracle.
--
-- Label = free_ts - pickup_ts per PHYSICAL TRIP (group by ytno, dispatch_ts; a dual/twin order shares
-- one dispatch_ts -> pickup = min(pickup_ts), free = max(free_ts) spans both drops). Full population
-- (GPS-independent) so the estimate is unbiased for GPS-silent trucks.
--
-- n_containers = TOS-authoritative twin_group_size (capped at 2). NOT inferred by pickup-gap
-- clustering: verified against tt_move_log.twin_group_size, dispatch_ts grouping matches it (LD nc=1
-- with tgs=2 is 1 row in 14d), whereas <=5min pickup clustering wrongly splits ~5k LD twins whose two
-- pickups are >5min apart. Twins are +54% (DS) / ~+85% (LD) longer, so conditioning on it is required.
--
-- Label clamped to 30..10800s (the tt_move_log 3h cap already bounds it). Window :days (default 14).
\if :{?days}
\else
  \set days 14
\endif

WITH trip AS (
  SELECT jobtype,
         least(coalesce(max(twin_group_size), 1), 2)::int        AS n_containers,
         extract(epoch FROM max(free_ts) - min(pickup_ts))::int  AS remaining_s
  FROM tt_move_log
  WHERE jobtype IN ('DS','LD')
    AND free_ts >= now() - make_interval(days => :days)
    AND pickup_ts IS NOT NULL AND free_ts IS NOT NULL
  GROUP BY ytno, dispatch_ts, jobtype
)
INSERT INTO learn_cycle_remaining
  (jobtype, n_containers, dest_inflight_bucket, n_samples, remaining_p50, remaining_p90, computed_at)
SELECT jobtype, n_containers, -1,
       count(*)::int,
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY remaining_s))::int,
       round(percentile_cont(0.9) WITHIN GROUP (ORDER BY remaining_s))::int,
       now()
FROM trip
WHERE remaining_s BETWEEN 30 AND 10800
GROUP BY jobtype, n_containers
ON CONFLICT (jobtype, n_containers, dest_inflight_bucket) DO UPDATE SET
  n_samples     = EXCLUDED.n_samples,
  remaining_p50 = EXCLUDED.remaining_p50,
  remaining_p90 = EXCLUDED.remaining_p90,
  computed_at   = now();
