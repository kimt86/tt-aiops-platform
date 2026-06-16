-- OD travel-time model v1: zone keys + features (design: bay-level blocks, GPS-grid quay).
--   • Block legs → block + bay (first 2 digits of the suffix; tier dropped — it doesn't affect
--     drive time). e.g. '01F-0708' → '01F-07', '05H-25' → '05H-25'.
--   • Quay/mobile-crane legs → ~150m GPS grid of the captured handover coordinate, because a QC
--     id is NOT a fixed location (cranes roam the rail). Needs the leg coord (Phase 2 capture);
--     pre-capture rows resolve to NULL (no retroactive quay zone possible).
-- Plus travel-time features: dow, shift, congestion (concurrent in-flight cycles). Weather is
-- joined at model time from weather_hourly by hour, not denormalized here.
ALTER TABLE learn_travel_sample
  ADD COLUMN IF NOT EXISTS origin_zone TEXT,
  ADD COLUMN IF NOT EXISTS dest_zone   TEXT,
  ADD COLUMN IF NOT EXISTS dow         INT,   -- 0=Sun..6=Sat (arrival)
  ADD COLUMN IF NOT EXISTS shift       TEXT,
  ADD COLUMN IF NOT EXISTS congestion  INT;   -- cycles in-flight at arrival (forward-only)

-- code(+coord) → zone. IMMUTABLE so it can be used in expressions/backfill.
CREATE OR REPLACE FUNCTION travel_zone(code text, lat float8, lon float8) RETURNS text
LANGUAGE sql IMMUTABLE AS $$
  SELECT CASE
    WHEN code IS NULL OR code = '' THEN NULL
    -- yard block: block + bay (drop tier/finer)
    WHEN code ~ '^[0-9]' THEN split_part(code,'-',1) || '-' || left(split_part(code,'-',2), 2)
    -- mobile crane (QC '^C', also M/Z): ~150m GPS grid of the handover coordinate
    WHEN lat IS NOT NULL AND lon IS NOT NULL THEN
      'Q' || round(lat / 0.00135)::int || '_' || round(lon / 0.00135)::int
    ELSE NULL  -- crane handover with no captured coordinate (pre-Phase-2 sample)
  END
$$;

-- Backfill block zones from existing codes (crane rows store no coord → stay NULL) + dow.
UPDATE learn_travel_sample SET
  origin_zone = travel_zone(origin, NULL, NULL),
  dest_zone   = travel_zone(dest, NULL, NULL),
  dow         = extract(dow FROM dropped_at)::int
WHERE origin_zone IS NULL AND dest_zone IS NULL;

CREATE INDEX IF NOT EXISTS learn_travel_sample_zone_idx ON learn_travel_sample (origin_zone, dest_zone);
