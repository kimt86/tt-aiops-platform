-- Empty-leg time DECOMPOSITION (settle the "why is effective speed only ~7 km/h" question with data,
-- not interpretation). For each settled empty leg (truck driving empty toward its pickup), split the
-- wall-clock time into MOVING vs STOPPED using 30s GPS motion segmentation, and locate the stopped
-- time relative to the destination — so we can SEE how much is real driving vs sitting (and whether the
-- sitting is near the pickup = crane/queue-side vs en-route). leg ends at the TOS ARRIVED (handover);
-- gps_arrived_at marks when GPS physically first reached the pickup, so (arrived_at − gps_arrived_at)
-- is the post-physical wait. Filled by spawn_leg_decomp (5min) + a one-time backfill.
CREATE TABLE IF NOT EXISTS learn_leg_decomp (
  ytno             text        NOT NULL,
  leg_start        timestamptz NOT NULL,   -- empty_travel_start (first movement empty)
  arrived_at       timestamptz,            -- empty_arrived (TOS ARRIVED at the handover point)
  dest_topos       text,
  grid_dist_m      int,                    -- rotated-grid Manhattan, first-fix → pickup coord
  total_s          int,                    -- leg_start → arrived_at  (= the realized travel_s)
  drive_s          int,                    -- moving time (Σ 30s windows with ≥8m displacement)
  stop_s           int,                    -- stopped time within the leg (total motion-segmented − drive)
  stop_near_dest_s int,                    -- stopped time spent within 60m of the pickup coord
  gps_arrived_at   timestamptz,            -- first GPS fix within 50m of the pickup coord
  captured_at      timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (ytno, leg_start)
);
CREATE INDEX IF NOT EXISTS learn_leg_decomp_cap ON learn_leg_decomp (captured_at);
