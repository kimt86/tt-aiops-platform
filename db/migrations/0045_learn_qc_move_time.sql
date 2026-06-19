-- Per-crane per-jobtype median move interval (rolling ~3 days), refreshed nightly. Feeds the
-- dispatch deadline/work-ETA calc so the per-move time is crane-specific (cranes vary ~79-115s on
-- discharge) instead of a flat jobtype constant. med_sec = median seconds between consecutive moves
-- (active cadence, capped 1-300s); n = sample size. Rolling, so not keyed by date (DELETE+INSERT).
CREATE TABLE IF NOT EXISTS learn_qc_move_time (
  qc        text NOT NULL,
  jobtype   text NOT NULL,   -- 'DS' (discharge) | 'LD' (load)
  med_sec   int,
  n         int,
  as_of_ts  timestamptz NOT NULL,
  PRIMARY KEY (qc, jobtype)
);
