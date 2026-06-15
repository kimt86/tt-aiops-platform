-- Yard-crane (RTG/ES) move stream from MCH_OPERATION — the FULL work mix, not just our DS
-- handovers. Discovery (2026-06-15): MCH_OPERATION logs RTG moves in detail (ST_DT start +
-- COMPDATE||COMPTIME complete), but the dashboard's KPI extractor filtered it to QC (^C). RTG
-- does ~5x more work than our DS handovers (LD/RH/AH/GI/GO/MI/MO; DS ≈ 20%), and the physical
-- move is ~60s median — so the ~12min DS wait is the RTG interleaving OTHER (invisible-to-DS)
-- moves. This stream lets us compute the RTG's real backlog as a wait-prediction feature.
-- Collected incrementally by the `rtg-moves` extractor. See research/rtg-work-cycle.
CREATE TABLE IF NOT EXISTS rtg_move_log (
  machno        TEXT NOT NULL,        -- yard crane (RTG### / ES##)
  contno        TEXT NOT NULL,
  seqno         TEXT NOT NULL,        -- (machno,contno,seqno) = MCH_PK_OPERATION natural key
  jobtype       TEXT,                 -- DS/LD/RH/AH/GI/GO/MI/MO/... (full yard work taxonomy)
  trk_id        TEXT,                 -- truck (NULL for housekeeping moves e.g. AH/RH)
  st_ts         TIMESTAMPTZ,          -- move start (ST_DT)
  comp_ts       TIMESTAMPTZ NOT NULL, -- move complete (COMPDATE||COMPTIME)
  dur_s         INTEGER,              -- comp − st (actual handling time, ~60s median)
  business_date DATE NOT NULL,
  captured_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (machno, contno, seqno)
);
CREATE INDEX IF NOT EXISTS rtg_move_log_machno_idx ON rtg_move_log (machno, comp_ts);
CREATE INDEX IF NOT EXISTS rtg_move_log_comp_idx   ON rtg_move_log (comp_ts);
