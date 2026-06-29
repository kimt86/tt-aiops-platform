-- Long-term congestion history (compact, speed-based). The live zone_density only keeps COUNT for 4
-- days; this keeps per-cell MEDIAN SPEED + traffic hourly for 180 days, so we can later compute a proper
-- road-segment congestion index (current speed ÷ free-flow) and study bottlenecks over time.
-- ~100m cells (0.0009° grid). Filled hourly by spawn_congestion_hourly from truck_pos_hifreq (3s GPS).
CREATE TABLE IF NOT EXISTS congestion_hourly (
  hour          timestamptz NOT NULL,
  cx            int         NOT NULL,   -- round(lat / 0.0009)  ~100m
  cy            int         NOT NULL,   -- round(lon / 0.0009)
  med_speed_kmh real,                   -- median moving speed of trucks crossing the cell that hour
  passes        int,                    -- number of 3s segments (traffic proxy)
  PRIMARY KEY (hour, cx, cy)
);
CREATE INDEX IF NOT EXISTS congestion_hourly_cell ON congestion_hourly (cx, cy, hour);
