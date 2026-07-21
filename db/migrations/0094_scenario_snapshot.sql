-- As-of yard-occupancy snapshot: block-level fill captured periodically (shift cadence) so ANY
-- past period start has a t=0 yard background. CYY_CONTAINER is current-only (overwritten each
-- ETL), so this MUST be captured going forward — past yard states can't be recovered. Block-level
-- (not per-container) is enough for the quayside dispatch sim (YC congestion / destination realism)
-- and tiny to store (~285 blocks/snapshot). The assembler later picks the snapshot nearest (<=)
-- the requested window start. (Berth/truck as-of are derivable from VSB_VOYAGE / truck_pos_hist,
-- so no snapshot needed for those.)

CREATE TABLE IF NOT EXISTS scenario.yard_snapshot (
  snapshot_ts TIMESTAMPTZ NOT NULL,
  block       TEXT NOT NULL,
  n_total     INTEGER NOT NULL,
  n_full      INTEGER NOT NULL DEFAULT 0,  -- empty = n_total - n_full
  n_reefer    INTEGER NOT NULL DEFAULT 0,
  n_20ft      INTEGER NOT NULL DEFAULT 0,
  n_import    INTEGER NOT NULL DEFAULT 0,  -- CYY_CONT_DISCHPORT = home_port (MYPKG)
  captured_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (snapshot_ts, block)
);
CREATE INDEX IF NOT EXISTS scenario_yard_snapshot_block_idx ON scenario.yard_snapshot (block, snapshot_ts);
CREATE INDEX IF NOT EXISTS scenario_yard_snapshot_ts_idx    ON scenario.yard_snapshot (snapshot_ts);
