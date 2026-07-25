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
--
-- Stage 2 (LD only) adds the destination-QC IN-FLIGHT lever: dest_inflight_bucket >= 0. See the
-- block at the bottom. DS has no such lever (its "destination" is a yard block), so DS keeps only the
-- baseline (-1) row. Buckets below :gate samples are not stored → the shadow falls back to -1.
\if :{?days}
\else
  \set days 14
\endif
\if :{?gate}
\else
  \set gate 500
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

-- ── Stage 2: LD destination-QC in-flight lever (dest_inflight_bucket >= 0) ──────────────────────
-- in-flight(trip) = # OTHER LD trips dispatched-and-not-yet-free to the same dest QC (free_crane) at
-- the trip's pickup instant. AS-OF-pickup ⇒ leak-free (state observable at pickup; never the target's
-- own free_ts). Computed by EVENT DIFFERENCING (O(n log n)): per dest QC, a running sum of +1 at each
-- trip's dispatch_ts and -1 at each free_ts, read at each pickup point, minus 1 for self. Validated:
-- reconstruction corr(in_flight, ln remaining) = 0.32 (≈ the 0.30 offline analysis, mig 0102).
--
-- Buckets (data-chosen; p50 monotone across them; a continuous per-value model beats these by <0.5pp,
-- so 7 tiers capture the feature's ceiling): 0:if<=2  1:3-4  2:5-6  3:7-8  4:9-10  5:11-13  6:>=14.
-- Keep this CASE IN SYNC with scripts/populate_cycle_pred_shadow.sql (the shadow buckets identically).
-- Only buckets with >= :gate samples are stored; sparse ones are removed so the shadow falls back to -1.
--
-- VALIDATION (out-of-sample temporal split + live-shadow re-score, 2026-07-25): this FULL 7-tier load
-- improves LD MAE -4% and p90 -6% consistently (both samples); medAE -5.6% (population) but ~0% on the
-- shadow subset. Per-bucket: LOW congestion (0-2) wins big (medAE -14..-46%, physically robust: idle
-- QC -> fast free); MID-HIGH (3-5) REGRESS out-of-sample (bucket p50 adds estimation noise near the
-- baseline / drifts). The Step-2 offline projection (-16% medAE) is NOT reproduced. A SELECTIVE load of
-- only buckets 0-2 gives medAE -10% / <5min +4pp but no tail gain -- flip `HAVING ...` to also require
-- `bucket IN (0,1,2)` to switch. Full load is kept here (net-positive on average+tail); the shadow logs
-- pred_baseline alongside pred_remaining, so the live A/B keeps measuring which wins as data accrues.
DELETE FROM learn_cycle_remaining WHERE jobtype = 'LD' AND dest_inflight_bucket >= 0;

WITH trips AS (
  SELECT ytno, dispatch_ts,
         min(pickup_ts)                                   AS pickup_ts,
         max(free_ts)                                     AS free_ts,
         max(free_crane)                                  AS dest_qc,
         least(coalesce(max(twin_group_size), 1), 2)::int AS nc
  FROM tt_move_log
  WHERE jobtype = 'LD'
    AND free_ts >= now() - make_interval(days => :days)
    AND pickup_ts IS NOT NULL AND free_ts IS NOT NULL AND free_crane IS NOT NULL
  GROUP BY ytno, dispatch_ts
),
stream AS (
  SELECT dest_qc, dispatch_ts AS ts, 0 AS ord,  1 AS delta, NULL::int AS nc, NULL::int AS remaining_s FROM trips
  UNION ALL
  SELECT dest_qc, free_ts,      0,     -1,       NULL,       NULL                                       FROM trips
  UNION ALL
  SELECT dest_qc, pickup_ts,    1,      0,       nc, extract(epoch FROM free_ts - pickup_ts)::int       FROM trips
),
run AS (
  SELECT *, sum(delta) OVER (PARTITION BY dest_qc ORDER BY ts, ord
                             ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS cum
  FROM stream
),
labelled AS (
  SELECT nc, remaining_s,
         CASE WHEN cum-1 <= 2 THEN 0 WHEN cum-1 <= 4 THEN 1 WHEN cum-1 <= 6 THEN 2
              WHEN cum-1 <= 8 THEN 3 WHEN cum-1 <= 10 THEN 4 WHEN cum-1 <= 13 THEN 5
              ELSE 6 END AS bucket
  FROM run
  WHERE ord = 1 AND remaining_s BETWEEN 30 AND 10800
)
INSERT INTO learn_cycle_remaining
  (jobtype, n_containers, dest_inflight_bucket, n_samples, remaining_p50, remaining_p90, computed_at)
SELECT 'LD', nc, bucket,
       count(*)::int,
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY remaining_s))::int,
       round(percentile_cont(0.9) WITHIN GROUP (ORDER BY remaining_s))::int,
       now()
FROM labelled
GROUP BY nc, bucket
HAVING count(*) >= :gate
ON CONFLICT (jobtype, n_containers, dest_inflight_bucket) DO UPDATE SET
  n_samples     = EXCLUDED.n_samples,
  remaining_p50 = EXCLUDED.remaining_p50,
  remaining_p90 = EXCLUDED.remaining_p90,
  computed_at   = now();
