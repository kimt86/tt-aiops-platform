-- Live vessel schedule (from TOS VSB_VOYAGE) — the deadline source for dispatch.
-- ESTWKC = estimated work-complete (when all crane work must be done), ESTDEP = estimated
-- departure. Times parsed from YYYYMMDDHHMMSS (terminal/MYT) to UTC. Refreshed each workpool tick
-- (DELETE+INSERT). disvan/loadvan = planned discharge/load container counts.
CREATE TABLE IF NOT EXISTS live_vessel_schedule (
  vessel      text NOT NULL,
  voyage      text NOT NULL,
  status      text,
  berthno     text,
  estber_ts   timestamptz,   -- estimated berth
  estwkc_ts   timestamptz,   -- estimated work complete (primary deadline)
  estdep_ts   timestamptz,   -- estimated departure
  cutoff_ts   timestamptz,   -- cargo cut-off
  actber_ts   timestamptz,   -- actual berth (null until berthed)
  actdep_ts   timestamptz,   -- actual departure (null until departed)
  disvan      int,           -- planned discharge containers
  loadvan     int,           -- planned load containers
  as_of_ts    timestamptz NOT NULL,
  PRIMARY KEY (vessel, voyage)
);
