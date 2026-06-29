-- Coverage fix for the pure-driving OD. 0065 built learn_travel_zone225_pure straight from
-- truck_pos_hist (2-day retention) → only ~374 cell pairs. Here we PERSIST each completed empty-travel
-- leg's moving-only time (drive_s) into a 30-day sample table, and rebuild the view to aggregate that.
-- Backfilled from the current 2-day GPS now; topped up every aggregator tick (livemap.rs). Over ~30
-- days coverage grows toward the bundled view's ~2,500 pairs. A leg = consecutive empty_travel fixes
-- (gap ≤ 90s); drive_s = Σ 30s segments where the truck moved (≥8m). Settled legs only (ended ≥3min ago).
CREATE TABLE IF NOT EXISTS learn_travel_drive_sample (
  ytno        text        NOT NULL,
  leg_start   timestamptz NOT NULL,
  oz          text        NOT NULL,
  dz          text        NOT NULL,
  drive_s     int         NOT NULL,
  captured_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (ytno, leg_start)
);
CREATE INDEX IF NOT EXISTS learn_travel_drive_sample_cap ON learn_travel_drive_sample (captured_at);

-- backfill from the 2-day truck_pos_hist (motion segmentation), settled legs only
INSERT INTO learn_travel_drive_sample (ytno, leg_start, oz, dz, drive_s)
WITH f AS (
  SELECT ytno, ts, lat, lon, lag(ts) OVER w pts, lag(lat) OVER w plat, lag(lon) OVER w plon
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
  SELECT ytno, min(ts) leg_start,
    travel_grid225((array_agg(lat ORDER BY ts))[1],      (array_agg(lon ORDER BY ts))[1])      oz,
    travel_grid225((array_agg(lat ORDER BY ts DESC))[1], (array_agg(lon ORDER BY ts DESC))[1]) dz,
    sum(dt) FILTER (WHERE disp_m >= 8 AND dt BETWEEN 20 AND 60) drive_s
  FROM lg GROUP BY ytno, leg HAVING count(*) >= 2 AND max(ts) < now() - interval '3 minutes'
)
SELECT ytno, leg_start, oz, dz, drive_s FROM legs
WHERE oz IS NOT NULL AND dz IS NOT NULL AND drive_s BETWEEN 5 AND 1800
ON CONFLICT (ytno, leg_start) DO NOTHING;

-- rebuild the pure OD view to aggregate the persistent samples (30-day), not raw truck_pos_hist
DROP MATERIALIZED VIEW IF EXISTS learn_travel_zone225_pure;
CREATE MATERIALIZED VIEW learn_travel_zone225_pure AS
SELECT oz, dz, count(*)::int n,
       percentile_cont(0.5) WITHIN GROUP (ORDER BY drive_s)::int p50_s,
       percentile_cont(0.9) WITHIN GROUP (ORDER BY drive_s)::int p90_s
FROM learn_travel_drive_sample
GROUP BY oz, dz;
CREATE UNIQUE INDEX IF NOT EXISTS learn_travel_zone225_pure_pk ON learn_travel_zone225_pure (oz, dz);
