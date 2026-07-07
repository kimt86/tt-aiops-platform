-- RTG/ES yard-crane GPS position history → rtg_pos_hist. Unlike QC (which broadcasts PLC), RTGs
-- have NO PLC, so the ONLY live signal for "this RTG is engaged with this TT" is GPS proximity —
-- and that currently fires for just ~16% of DS drop handovers (RTG GPS is often >30m off, we
-- suspect mostly in the across-block/ROW axis while stable along the BAY axis). We never persisted
-- RTG GPS before (only trucks: truck_pos_hist/hifreq). Collect it now, INCLUDING stationary jitter,
-- to (1) verify that anisotropy against the terminal grid axis and (2) research whether block-aware
-- GPS correction lets us catch the RTG↔TT handover-START moment. Ground truth for validation =
-- rtg_move_log (st_ts/comp_ts per machno+trk_id, from TOS MCH_OPERATION). Research window; 3-day prune.
CREATE TABLE IF NOT EXISTS rtg_pos_hist (
  ts     TIMESTAMPTZ      NOT NULL,
  machno TEXT             NOT NULL,   -- yard crane device id (RTG### / ES##)
  lat    DOUBLE PRECISION NOT NULL,
  lon    DOUBLE PRECISION NOT NULL,
  PRIMARY KEY (machno, ts)
);
CREATE INDEX IF NOT EXISTS rtg_pos_hist_ts ON rtg_pos_hist (ts);
