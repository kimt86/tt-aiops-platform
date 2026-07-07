-- Quay-crane (QC: C/M/Z) move stream from MCH_OPERATION → qc_move_log. Parallel to rtg_move_log
-- (mig 0032, which covers the RTG/ES yard side). QC handovers are the OTHER two of the four cycle
-- handovers: DS pickup (QC discharges ship → truck) and LD drop (truck → QC loads onto ship).
-- Every QC move involves a truck (TRK_ID 100% populated — no housekeeping moves), so comp_ts is the
-- PHYSICAL handover completion: DS pickup done (≈ pickup_left) / LD drop done (≈ dropped_at).
-- Together with rtg_move_log this gives a TOS ground truth for ALL FOUR handovers — the Phase-2
-- backfill correction of the websocket-estimated cycle timestamps (see cycle-decomposition doc §5).
-- Incremental via etl_watermark (stream='qc_move'). Collected by the `qc-moves` extractor (~5min).
CREATE TABLE IF NOT EXISTS qc_move_log (
  machno        TEXT NOT NULL,        -- quay crane (C## / M## / Z#)
  contno        TEXT NOT NULL,
  seqno         TEXT NOT NULL,        -- (machno,contno,seqno) = MCH_PK_OPERATION natural key
  jobtype       TEXT,                 -- DS/LD/... (QC↔truck handover types)
  trk_id        TEXT,                 -- truck (100% populated for QC moves)
  st_ts         TIMESTAMPTZ,          -- move start (ST_DT)
  comp_ts       TIMESTAMPTZ NOT NULL, -- move complete (COMPDATE||COMPTIME) = physical handover done
  dur_s         INTEGER,              -- comp − st
  business_date DATE NOT NULL,
  captured_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (machno, contno, seqno)
);
CREATE INDEX IF NOT EXISTS qc_move_log_machno_idx ON qc_move_log (machno, comp_ts);
CREATE INDEX IF NOT EXISTS qc_move_log_comp_idx   ON qc_move_log (comp_ts);
CREATE INDEX IF NOT EXISTS qc_move_log_trk_idx    ON qc_move_log (trk_id, comp_ts);
