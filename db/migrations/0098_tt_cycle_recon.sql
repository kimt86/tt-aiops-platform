-- tt_cycle_recon: per-physical-trip GPS move/stop decomposition attached to each tt_move_log cycle.
-- Cycle IS tt_move_log (TOS-authoritative boundaries: dispatch_ts → pickup_ts → free_ts, ~97.8% cover).
-- GPS (truck_pos_hifreq) contributes ONLY the motion split within those boundaries:
--   each leg (empty=dispatch→pickup, laden=pickup→free) is segmented into DRIVE vs in-leg STOP,
--   and edge_wait_s = cycle_s − observed = the silent boundary dwell (dispatch wait + pickup + drop).
-- Fully reconciling by construction: e_drive+e_stop+l_drive+l_stop+edge_wait = cycle_s.
-- One row per PHYSICAL trip (twins collapsed): PK (ytno, dispatch_ts), 1:1 with tt_move_log twin_leg_seq=1.
-- Supersedes GPS cycle decomposition (tt_cycle_log / tt_cycle_v2), which under-cover (~29-49%).

CREATE TABLE IF NOT EXISTS tt_cycle_recon (
  ytno          TEXT        NOT NULL,
  dispatch_ts   TIMESTAMPTZ NOT NULL,
  contno        TEXT,                       -- representative container (twin_leg_seq=1)
  jobtype       TEXT,                        -- DS / LD
  is_twin       BOOLEAN,                     -- 2+ containers carried in this one physical trip
  n_containers  INTEGER,                     -- containers on the trip (1 = single, 2 = twin, 3 = tandem)
  -- Physical-trip boundaries (twins collapsed): pickup_ts = MIN over legs (first loaded),
  -- free_ts = MAX over legs (last freed). For DS twins the two containers are dropped at the RTG
  -- ~2min apart, so MAX free_ts is required or the trip ends early and misses the second drop.
  pickup_ts     TIMESTAMPTZ,                 -- TOS: first pickup (empty→laden boundary)
  free_ts       TIMESTAMPTZ,                 -- TOS: last container freed (cycle end)
  cycle_s       INTEGER,                     -- TOS: free_ts(max) − dispatch_ts (authoritative)

  -- GPS-reconstructed motion split (seconds / metres); NULL-safe 0 when no GPS.
  -- Long silent gaps (dist>=100m over >60s = movement during telemetry silence) credit only the
  -- nominal transit time (dist/5.5 m/s) to drive; the residual goes to stop (the truck was parked
  -- most of the gap). So *_drive_s is rolling time, not "any motion".
  e_drive_s     INTEGER,                     -- empty leg: rolling driving time
  e_stop_s      INTEGER,                     -- empty leg: in-leg stop (mid-route/queue) time observed
  e_drive_m     INTEGER,                     -- empty leg: driven path distance
  l_drive_s     INTEGER,                     -- laden leg: rolling driving time
  l_stop_s      INTEGER,                     -- laden leg: in-leg stop (queue at drop) time observed
  l_drive_m     INTEGER,                     -- laden leg: driven path distance
  edge_wait_s   INTEGER,                     -- cycle_s − observed = silent boundary dwell (= sum of the 3 below)
  dispatch_wait_s INTEGER,                   -- dispatch_ts → first observed fix (waiting to depart / staging)
  pickup_dwell_s  INTEGER,                   -- last empty fix → first laden fix (arrive+handover at pickup)
  drop_dwell_s    INTEGER,                   -- last laden fix → free_ts (arrive+queue+handover at drop; silent)

  gps_covered   BOOLEAN,                     -- a usable drive segment was observed (false = no drive segment: GPS-silent whole cycle or aged out of hifreq)
  n_fix         INTEGER,                     -- hifreq fixes inside the cycle window (confidence)
  long_gap_s    INTEGER,                     -- total time inside >60s dist>=100 silent gap-bridge segments (low-confidence share of motion)
  business_date DATE,
  shift         TEXT,                        -- D (06–17 MYT) / N
  computed_at   TIMESTAMPTZ NOT NULL DEFAULT now(),

  PRIMARY KEY (ytno, dispatch_ts)
);

CREATE INDEX IF NOT EXISTS tt_cycle_recon_bdate_idx ON tt_cycle_recon (business_date);
CREATE INDEX IF NOT EXISTS tt_cycle_recon_free_idx  ON tt_cycle_recon (free_ts);
