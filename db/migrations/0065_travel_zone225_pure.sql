-- Pure-driving OD: empty-travel time EXCLUDING stops (queue / congestion / handover-approach).
-- The existing learn_travel_zone225 bundles the ~35% of empty-travel time the truck is STOPPED
-- (same-cell OD there = 228s of pure overhead), which inflates + compresses the dispatch cost.
-- This view rebuilds OD from truck_pos_hist GPS motion: a leg = consecutive empty_travel fixes
-- (gap ≤ 90s); drive_s = Σ of 30s segments where the truck actually moved (≥8m). So drive_s is the
-- moving-only (pure driving) time. Used by the dispatch cost so cost = real driving, not driving+queue.
-- (Coverage ≈ busy pairs only — truck_pos_hist is 2-day; the matcher falls back to quay-Manhattan /
--  PURE_DRIVE_SPEED for uncovered pairs.) p50_s/p90_s column names mirror learn_travel_zone225.
CREATE MATERIALIZED VIEW IF NOT EXISTS learn_travel_zone225_pure AS
WITH f AS (
  SELECT ytno, ts, lat, lon,
         lag(ts) OVER w pts, lag(lat) OVER w plat, lag(lon) OVER w plon
  FROM truck_pos_hist WHERE state = 'empty_travel'
  WINDOW w AS (PARTITION BY ytno ORDER BY ts)
),
seg AS (
  SELECT ytno, ts, lat, lon,
    CASE WHEN pts IS NULL OR ts - pts > interval '90 seconds' THEN 1 ELSE 0 END nl,
    extract(epoch FROM ts - pts) dt,
    CASE WHEN pts IS NULL THEN NULL ELSE
      2*6371000*asin(sqrt(power(sin(radians(lat-plat)/2),2)
        + cos(radians(plat))*cos(radians(lat))*power(sin(radians(lon-plon)/2),2))) END disp_m
  FROM f
),
lg AS (SELECT *, sum(nl) OVER (PARTITION BY ytno ORDER BY ts) leg FROM seg),
legs AS (
  SELECT travel_grid225((array_agg(lat ORDER BY ts))[1],      (array_agg(lon ORDER BY ts))[1])      oz,
         travel_grid225((array_agg(lat ORDER BY ts DESC))[1], (array_agg(lon ORDER BY ts DESC))[1]) dz,
         sum(dt) FILTER (WHERE disp_m >= 8 AND dt BETWEEN 20 AND 60) drive_s   -- moving-only time
  FROM lg GROUP BY ytno, leg HAVING count(*) >= 2
)
SELECT oz, dz, count(*)::int n,
       percentile_cont(0.5) WITHIN GROUP (ORDER BY drive_s)::int p50_s,
       percentile_cont(0.9) WITHIN GROUP (ORDER BY drive_s)::int p90_s
FROM legs WHERE oz IS NOT NULL AND dz IS NOT NULL AND drive_s BETWEEN 5 AND 1800
GROUP BY oz, dz;
CREATE UNIQUE INDEX IF NOT EXISTS learn_travel_zone225_pure_pk ON learn_travel_zone225_pure (oz, dz);
