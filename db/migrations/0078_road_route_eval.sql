-- Validate the road-graph router against the pure-drive label BEFORE wiring it into the cost:
-- store each empty leg's origin/dest GPS on learn_leg_decomp, then road_route_eval logs the graph's
-- route time/distance vs drive_s (and Manhattan, computed from the coords) so we can see whether the
-- topology-aware route actually predicts drive_s better than the cheap Manhattan baseline.
ALTER TABLE learn_leg_decomp ADD COLUMN IF NOT EXISTS origin_lat double precision;
ALTER TABLE learn_leg_decomp ADD COLUMN IF NOT EXISTS origin_lon double precision;
ALTER TABLE learn_leg_decomp ADD COLUMN IF NOT EXISTS dest_lat   double precision;
ALTER TABLE learn_leg_decomp ADD COLUMN IF NOT EXISTS dest_lon   double precision;

CREATE TABLE IF NOT EXISTS road_route_eval (
  ts           timestamptz NOT NULL DEFAULT now(),
  ytno         text,
  leg_start    timestamptz,
  drive_s      int,          -- label: pure-drive time
  route_time_s int,          -- road-graph route time (edge len ÷ learned speed)
  route_dist_m int,          -- road-graph route distance (topology-aware; the model's route feature)
  snapped      boolean       -- both endpoints snapped to the network AND a directed path exists
);
CREATE INDEX IF NOT EXISTS road_route_eval_ts ON road_route_eval (ts);
