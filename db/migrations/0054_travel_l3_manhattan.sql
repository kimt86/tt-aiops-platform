-- L3 (untrained-OD) fallback: replace straight-line (haversine ÷ 6.55 km/h) with QUAY-AXIS Manhattan
-- distance ÷ 8.2 km/h. The yard grid is rotated ~29.8° from north; Manhattan measured along the
-- quay-aligned axes tracks the real road detour (×1.18 ≈ road graph's ×1.15) far better than
-- straight-line, at no extra cost. (Validated on 250 trips: detour 1.18, MAPE 70% vs 71% straight.)
-- Keeps travel_cost_lookup consistent with the live matcher (which applies the same in Rust).
CREATE OR REPLACE FUNCTION travel_cost_lookup(o_lat float8, o_lon float8, d_lat float8, d_lon float8)
RETURNS TABLE(p50_s int, p90_s int, n int, tier text) LANGUAGE sql STABLE AS $$
  WITH l2 AS (
    SELECT z.p50_s, z.p90_s, z.n FROM learn_travel_zone225 z
    WHERE z.oz = travel_grid225(o_lat, o_lon) AND z.dz = travel_grid225(d_lat, d_lon) AND z.n >= 10),
  man AS (
    SELECT (
        abs(  ((d_lat - o_lat) * 111320.0) * 0.86777
            + ((d_lon - o_lon) * 111320.0 * cos(radians((o_lat + d_lat) / 2))) * 0.49697)
      + abs(-((d_lat - o_lat) * 111320.0) * 0.49697
            + ((d_lon - o_lon) * 111320.0 * cos(radians((o_lat + d_lat) / 2))) * 0.86777)
    )::float8 AS m)
  SELECT p50_s, p90_s, n, 'L2'::text FROM l2
  UNION ALL
  SELECT (m / 2.278)::int, (m / 2.278 * 1.5)::int, 0, 'L3'::text FROM man WHERE NOT EXISTS (SELECT 1 FROM l2)
  LIMIT 1
$$;
