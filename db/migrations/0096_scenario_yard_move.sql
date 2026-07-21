-- Raw yard-crane (RTG) moves WITH decoded stack position — the event source for the incremental
-- yard-state model. From MCH_OPERATION RTG moves; CRNT_PSN_IDX_NO1~4 decoded (VERIFIED against
-- CYY_CONTAINER.CLOCATION for the same containers, 2026-07-21):
--   NO1 = block_id · NO2 = bay ordinal(0-based) · NO3 = row index(0=A) · NO4 = tier-1  → tier = NO4+1
-- Watermark-incremental (scenario.watermark source='yard_move'). A later migration adds
-- scenario.yard_cell which replays these to reconstruct which container sits at each
-- (block,bay,row,tier), seeding "unknown" placeholders below observed containers — so we need
-- NO initial full snapshot; after ~a month of yard turnover every cell is known.
CREATE TABLE IF NOT EXISTS scenario.yard_move (
  comp_ts     TIMESTAMPTZ NOT NULL,
  contno      TEXT NOT NULL,
  jobtype     TEXT NOT NULL,     -- DS|GI (place) · LD|GO (remove) · RH (rehandle) · MI|MO|AH…
  block_id    INTEGER NOT NULL,  -- NO1
  bay_idx     INTEGER NOT NULL,  -- NO2 (0-based bay ordinal)
  row_idx     INTEGER NOT NULL,  -- NO3 (0-based; A=0)
  tier        INTEGER NOT NULL,  -- NO4+1 (1-based)
  machno      TEXT,
  seqno       TEXT NOT NULL,
  captured_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (machno, contno, seqno)
);
CREATE INDEX IF NOT EXISTS scenario_yard_move_ts_idx    ON scenario.yard_move (comp_ts);
CREATE INDEX IF NOT EXISTS scenario_yard_move_stack_idx ON scenario.yard_move (block_id, bay_idx, row_idx, tier);
CREATE INDEX IF NOT EXISTS scenario_yard_move_cont_idx  ON scenario.yard_move (contno);
