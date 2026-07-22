-- Periodic full snapshots of the reconstructed yard cell state, so `yard::state_as_of(T)` no longer
-- has to replay the ENTIRE move history on every scenario download.
--
-- Without this, state_as_of replays all of scenario.yard_move up to T on each call, synchronously in
-- the download handler: at ~50k moves/day a month of accumulation is ~1.5M rows replayed PER REQUEST.
-- With it, state_as_of seeds from the newest checkpoint <= T and replays only the delta after it, so
-- cost is bounded by the checkpoint interval instead of by total history.
--
-- A checkpoint is written by `scengen yard-build` and is only valid if the build batch was NOT
-- truncated — otherwise `yard_cell` would not actually reflect every move up to checkpoint_ts.
--
-- Secondary benefit: this decouples past-window reconstruction from scenario.yard_move retention.
-- If moves are ever pruned, any T at or after a surviving checkpoint still reconstructs correctly.
-- Pruning must therefore never remove moves newer than the newest checkpoint.
CREATE TABLE IF NOT EXISTS scenario.yard_checkpoint (
  checkpoint_ts TIMESTAMPTZ NOT NULL, -- state includes every move with comp_ts <= this instant
  block_id      INTEGER     NOT NULL,
  bay_idx       INTEGER     NOT NULL,
  row_idx       INTEGER     NOT NULL,
  tier          INTEGER     NOT NULL,
  contno        TEXT,                 -- NULL when the cell is an inferred "unknown" occupant
  known         BOOLEAN     NOT NULL,
  PRIMARY KEY (checkpoint_ts, block_id, bay_idx, row_idx, tier)
);

-- Drives "newest checkpoint <= T" and the retention sweep.
CREATE INDEX IF NOT EXISTS scenario_yard_checkpoint_ts_idx
  ON scenario.yard_checkpoint (checkpoint_ts);
