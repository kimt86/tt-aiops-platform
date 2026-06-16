-- Tighten the quay GPS grid 150m → 50m. GPS is precise enough (TT accuracy median 5m, p90 9m,
-- 99% < 25m = half a 50m cell), so jitter rarely crosses a cell boundary. The aggregator calls
-- this DB function, so the change takes effect on the next tick with no rebuild/restart. Because
-- raw coords are stored (mig 0036), we re-grid ALL existing samples retroactively too.
-- 50m ≈ 50/111320 = 0.000449 deg. Block zones ignore coords (logical code) → unchanged.
CREATE OR REPLACE FUNCTION travel_zone(code text, lat float8, lon float8) RETURNS text
LANGUAGE sql IMMUTABLE AS $$
  SELECT CASE
    WHEN code IS NULL OR code = '' THEN NULL
    WHEN code ~ '^[0-9]' THEN split_part(code,'-',1) || '-' || left(split_part(code,'-',2), 2)
    WHEN lat IS NOT NULL AND lon IS NOT NULL THEN
      'Q' || round(lat / 0.00045)::int || '_' || round(lon / 0.00045)::int
    ELSE NULL
  END
$$;

-- Retroactive re-grid (blocks idempotent; quay → 50m; crane-without-coord stays NULL).
UPDATE learn_travel_sample SET
  origin_zone = travel_zone(origin, origin_lat, origin_lon),
  dest_zone   = travel_zone(dest,   dest_lat,   dest_lon);
