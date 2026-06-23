-- Stage-2 SHADOW: per-tick recommended vehicle→work matches (display/validation only, never drives
-- live dispatch). Lets us measure, before any real cutover: recommended arrival-time vs TOS, the
-- "free truck nearby but QC idle" inefficiency, deadline feasibility, and (next) thrash via tick-
-- to-tick switches. One row per recommended (vehicle) assignment per tick.
CREATE TABLE IF NOT EXISTS stage2_match_shadow (
  ts               timestamptz NOT NULL DEFAULT now(),
  tick             bigint,
  ytno             text NOT NULL,          -- recommended vehicle
  qc               text,                   -- matched work: QC
  vessel           text,
  queuename        text,
  jobtype          text,                   -- DS | LD
  src_block        text,                   -- LD pickup block (DS: NULL)
  veh_state        text,                   -- idle | soon_idle | approaching | wait_rtg
  arrival_s        int,                    -- recommended arrival cost = time-to-free + OD p50
  od_p90_s         int,                    -- conservative travel (deadline check)
  deadline_slack_s int,                    -- work-ETA − recommended arrival(p90); negative = at risk
  feasible         boolean,                -- arrives within the work-ETA (p90)
  cost_tier        text,                   -- L2 (225m grid) | L3 (haversine fallback)
  PRIMARY KEY (ts, ytno)
);
CREATE INDEX IF NOT EXISTS stage2_match_ts ON stage2_match_shadow (ts);
CREATE INDEX IF NOT EXISTS stage2_match_qc ON stage2_match_shadow (qc, ts);
