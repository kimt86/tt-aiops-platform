-- Map-matching SHADOW: per-leg record of how far a truck got along its expected road route (progress
-- fraction) vs whether the current geofence/ARRIVED method would catch the arrival. Written every leg
-- transition by spawn_mapmatch_shadow. Used to measure the arrival-capture gain before wiring
-- map-matching into cycle decomposition. No live behaviour depends on it.
CREATE TABLE IF NOT EXISTS mm_arrival_shadow (
  id         bigserial PRIMARY KEY,
  logged_at  timestamptz NOT NULL DEFAULT now(),
  ytno       text        NOT NULL,
  dest_topos text        NOT NULL,
  is_crane   boolean     NOT NULL DEFAULT false,
  leg_dur_s  integer,
  route_m    real,        -- length of the expected route (origin → dest work-point)
  progress_frac real,     -- map-match: furthest along the route the truck got (0..1)
  min_dest_m real,        -- closest the raw GPS came to the dest work-point (geofence proxy)
  saw_arrived boolean,    -- did the websocket ARRIVED flag fire during the leg (current-method signal)
  max_gap_s  real,        -- worst GPS staleness (gap) seen during the leg
  max_jump_m real         -- worst GPS jump between consecutive samples during the leg
);
CREATE INDEX IF NOT EXISTS mm_arrival_shadow_ts ON mm_arrival_shadow (logged_at);
