-- Stage-2 OD cost infrastructure. The fine travel zones (block+bay / 50m quay grid) leave only ~60
-- O→D pairs at n>=10 confidence — too sparse for the dispatch cost matrix, and block zones are
-- logical codes a truck's bare GPS can't match. Re-aggregate to a UNIFORM ~225m SPATIAL grid on the
-- handover coordinates (both endpoints), so (a) far more pairs reach confidence (~1,900 vs ~60) and
-- (b) a truck's live GPS maps directly to an origin cell. Materialized for a fast single-join
-- lookup; refreshed every 5min by the travel aggregator.
CREATE OR REPLACE FUNCTION travel_grid225(lat float8, lon float8) RETURNS text
LANGUAGE sql IMMUTABLE AS $$
  SELECT CASE WHEN lat IS NULL OR lon IS NULL THEN NULL
    ELSE 'G' || round(lat / 0.00202)::int || '_' || round(lon / 0.00202)::int END  -- ~225m cell
$$;

DROP MATERIALIZED VIEW IF EXISTS learn_travel_zone225;
CREATE MATERIALIZED VIEW learn_travel_zone225 AS
  SELECT travel_grid225(origin_lat, origin_lon) AS oz,
         travel_grid225(dest_lat,   dest_lon)   AS dz,
         count(*)::int                                          AS n,
         percentile_cont(0.5) WITHIN GROUP (ORDER BY travel_s)::int AS p50_s,
         percentile_cont(0.9) WITHIN GROUP (ORDER BY travel_s)::int AS p90_s,
         avg(dist_m)::float8                                    AS dist_m
    FROM learn_travel_sample
   WHERE travel_s BETWEEN 1 AND 3600 AND origin_lat IS NOT NULL AND dest_lat IS NOT NULL
   GROUP BY 1, 2;
CREATE UNIQUE INDEX IF NOT EXISTS learn_travel_zone225_pk ON learn_travel_zone225 (oz, dz);
DROP FUNCTION IF EXISTS travel_zone_coarse(text, float8, float8);  -- superseded by the spatial grid

-- Hierarchical OD travel-cost lookup for Stage-2. Coords in (truck GPS → work point), returns the
-- predicted travel time both as p50 (for the arrival-sum objective) and p90 (conservative, for the
-- deadline edge-filter), the support n, and which tier answered. Always returns a row (never NULL):
--   L2 = the ~225m spatial-grid pair, n>=10 (the workhorse).
--   L3 = haversine straight-line distance ÷ 6.55 km/h yard speed (always available fallback).
-- (L1 exact code-pair precision can be layered later for code↔code endpoints; the truck origin has
--  no code, so the spatial L2 is the primary layer for dispatch.)
CREATE OR REPLACE FUNCTION travel_cost_lookup(o_lat float8, o_lon float8, d_lat float8, d_lon float8)
RETURNS TABLE(p50_s int, p90_s int, n int, tier text) LANGUAGE sql STABLE AS $$
  WITH l2 AS (
    SELECT z.p50_s, z.p90_s, z.n
      FROM learn_travel_zone225 z
     WHERE z.oz = travel_grid225(o_lat, o_lon)
       AND z.dz = travel_grid225(d_lat, d_lon)
       AND z.n >= 10
  ),
  hav AS (
    SELECT (2 * 6371000 * asin(sqrt(
              power(sin(radians(d_lat - o_lat) / 2), 2)
            + cos(radians(o_lat)) * cos(radians(d_lat)) * power(sin(radians(d_lon - o_lon) / 2), 2)
           )))::float8 AS m
  )
  SELECT p50_s, p90_s, n, 'L2'::text FROM l2
  UNION ALL
  SELECT (m / 1.8194)::int, (m / 1.8194 * 1.5)::int, 0, 'L3'::text
    FROM hav WHERE NOT EXISTS (SELECT 1 FROM l2)
  LIMIT 1
$$;
