-- Reconstructed yard state: which container sits at each (block,bay,row,tier). Built INCREMENTALLY
-- by replaying scenario.yard_move in comp_ts order (never a full re-replay, so long-dwell containers
-- whose placing move has aged out of retention are NOT lost). Reconstruction watermark =
-- scenario.watermark source='yard_cell' (last processed comp_ts, ISO text).
--
-- known=false = "unknown placeholder": we inferred a container is here (something was stacked above
-- it) but haven't observed its identity yet. It resolves the moment it's removed (that move carries
-- the contno) — so after ~a month of yard turnover every cell becomes known. No initial full snapshot.
--
-- Move -> action (jobtype, per ops): PLACE = DS·GI·RH·AH(재취급)·MI(구내이적 입고) / REMOVE = LD·GO·MO(구내이적 출고).
CREATE TABLE IF NOT EXISTS scenario.yard_cell (
  block_id   INTEGER NOT NULL,
  bay_idx    INTEGER NOT NULL,
  row_idx    INTEGER NOT NULL,
  tier       INTEGER NOT NULL,
  contno     TEXT,                       -- NULL = unknown placeholder
  known      BOOLEAN NOT NULL DEFAULT false,
  updated_ts TIMESTAMPTZ,
  PRIMARY KEY (block_id, bay_idx, row_idx, tier)
);
CREATE INDEX IF NOT EXISTS scenario_yard_cell_cont_idx ON scenario.yard_cell (contno);
