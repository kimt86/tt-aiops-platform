-- Per-QC starvation time series for the dispatch Stage-1 CAUSAL validation. The existing
-- qc_wait_sample stores only terminal-wide COUNTS (PK ts), so we cannot tell WHICH crane starved
-- and line it up with our per-QC prediction. This logs one row per working+positioned crane per 30s
-- tick (NOT only starving ones — logging every working crane lets us reconstruct each starvation
-- EPISODE's start/end: starving_real true→false, and the gps no-truck flag flipping = the truck
-- arrived). Joined to dispatch_pred_sample by (qc, time window) for the WIN/LOSS analysis.
--   no_truck_gps   : no fresh TT within 40m of the crane (raw geometric, ignores idle threshold)
--   no_truck_topos : TOS topos1 says no TT assigned to this crane
--   starving_real  : idle past threshold AND no_truck_gps AND pending = genuine truck-starvation
--   near_idle_tt   : fresh unengaged TTs within ~600m (location/travel-time control — distinguishes
--                    "no truck dispatched in time" (Stage-1) from "no truck was nearby" (Stage-2))
--   next_vessel/queuename : best-effort block/bay the crane is waiting on (lowest-seq incomplete
--                    queue; per-container is impossible — MSNSEQ is 100% NULL in the snapshot)
CREATE TABLE IF NOT EXISTS qc_wait_qc_sample (
  ts             timestamptz NOT NULL,
  qc             text        NOT NULL,
  idle_s         int,
  no_truck_gps   boolean,
  no_truck_topos boolean,
  pending        boolean,
  starving_real  boolean,
  near_idle_tt   int,
  next_vessel    text,
  next_queuename text,
  PRIMARY KEY (ts, qc)
);
CREATE INDEX IF NOT EXISTS qc_wait_qc_ts ON qc_wait_qc_sample (ts);
CREATE INDEX IF NOT EXISTS qc_wait_qc_qc ON qc_wait_qc_sample (qc, ts);
