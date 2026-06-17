-- Per-zone vehicle density snapshots for the travel-time congestion feature. Every ~60s the API
-- buckets live TT positions into uniform grids at FOUR cell sizes (50/100/150/200m) and stores
-- the occupied-cell counts, so we can later compare which resolution best predicts trip time.
-- cx = round(lat / (grid_m/111320)), cy = round(lon / (grid_m/111320)) — same cell def as the
-- live-map grid overlay. Rolling buffer: pruned to a few days (the aggregator denormalises each
-- trip's path density onto learn_travel_sample for long-term use). See research/travel-time.
CREATE TABLE IF NOT EXISTS zone_density (
  ts       TIMESTAMPTZ NOT NULL,   -- snapshot time (UTC)
  grid_m   INT NOT NULL,           -- cell size in metres (50/100/150/200)
  cx       INT NOT NULL,           -- round(lat/deg)
  cy       INT NOT NULL,           -- round(lon/deg)
  n        INT NOT NULL,           -- TT count in the cell
  PRIMARY KEY (ts, grid_m, cx, cy)
);
CREATE INDEX IF NOT EXISTS zone_density_ts_idx ON zone_density (ts);
