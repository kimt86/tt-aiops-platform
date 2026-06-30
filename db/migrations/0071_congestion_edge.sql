-- Road-network (EDGE) based congestion, replacing the deprecated cell-based congestion_hourly.
-- For each inferred-graph edge, the median truck speed on that edge in the hour (3s GPS map-matched to
-- the nearest edge). Congestion index = hour speed ÷ that edge's free-flow (high percentile over history),
-- computed on read. Filled by scripts/reinfer_roadgraph.sh (rebuilds graph + map-matches). Keyed by the
-- edge MIDPOINT so it stays comparable as the graph is re-inferred denser over time.
DROP TABLE IF EXISTS congestion_hourly;
CREATE TABLE IF NOT EXISTS congestion_edge (
  hour          timestamptz      NOT NULL,
  mlat          double precision NOT NULL,   -- edge midpoint
  mlon          double precision NOT NULL,
  med_speed_kmh real,                         -- median truck speed on the edge that hour
  n             int,                           -- map-matched 3s segments
  len_m         real,                          -- edge length
  PRIMARY KEY (hour, mlat, mlon)
);
CREATE INDEX IF NOT EXISTS congestion_edge_loc ON congestion_edge (mlat, mlon, hour);
