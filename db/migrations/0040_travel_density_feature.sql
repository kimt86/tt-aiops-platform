-- Per-trip congestion feature: density along the O→D corridor at the trip's time, at all 4 grid
-- sizes (so we can compare which resolution best explains within-OD travel variance). The
-- aggregator samples 5 points along the straight O→D line, maps each to its cell, looks up the
-- zone_density snapshot in the trip's minute, and averages → density_{50,100,150,200}. trip_ts =
-- the trip's mid-time (the moment to read congestion at). Replaces the useless global `congestion`.
ALTER TABLE learn_travel_sample
  ADD COLUMN IF NOT EXISTS trip_ts     TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS density_50  REAL,
  ADD COLUMN IF NOT EXISTS density_100 REAL,
  ADD COLUMN IF NOT EXISTS density_150 REAL,
  ADD COLUMN IF NOT EXISTS density_200 REAL;

-- cell-keyed lookup for the per-trip density join (PK is ts-leading, so add a cell-leading index)
CREATE INDEX IF NOT EXISTS zone_density_cell_idx ON zone_density (grid_m, cx, cy, ts);
