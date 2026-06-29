-- High-frequency (~3s) GPS capture for road-network MAP INFERENCE (build a trusted graph from our own
-- traces instead of the unreliable imported one). Kept SEPARATE from the 30s truck_pos_hist — that one
-- feeds the pure-OD motion segmentation (which assumes 30s gaps), so its cadence must not change.
-- Only logs a truck when it MOVED >5m since its last log (skips parked dupes) → dense road trails only.
-- 5-day retention (inference needs density, not long history).
CREATE TABLE IF NOT EXISTS truck_pos_hifreq (
  ts   timestamptz      NOT NULL,
  ytno text             NOT NULL,
  lat  double precision NOT NULL,
  lon  double precision NOT NULL,
  PRIMARY KEY (ytno, ts)
);
CREATE INDEX IF NOT EXISTS truck_pos_hifreq_ts ON truck_pos_hifreq (ts);
