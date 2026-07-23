-- Redesign additions for the scenario subsystem: equipment deployment + landside (gate) detail.
-- Everything else in scenario.* was wiped to a clean slate on 2026-07-23; these two tables are the
-- genuinely new TOS touch-points. RTG block deployment and TT fleet counts are NOT stored here —
-- they are DERIVED at assembly time from scenario.yard_move / the move logs (no new source needed).

-- QC (quay-crane) deployment events: which crane was assigned to which vessel and when.
-- Source: TOSADM.JOB_CRANE_HISTORY (CRANENO/VESSEL/VOYAGE, DATE+TIME; the _DT/START/END columns
-- are NULL in practice, so we keep the raw assign events and derive [start,end) intervals at
-- assembly by pairing consecutive events per (crane, vessel). Table is tiny (~8k rows total) and
-- its PK leads with DATE, so collection is a cheap date-watermark seek.
CREATE TABLE IF NOT EXISTS scenario.crane_deploy (
  crane_no  TEXT        NOT NULL,   -- C##/M##/Z## (quay cranes only; JOB_CRANE_HISTORY has no RTG)
  vessel    TEXT,
  voyage    TEXT,
  ev_type   TEXT,                   -- JOB_CRHIST_TYPE (assign/update marker)
  ev_ts     TIMESTAMPTZ NOT NULL,   -- JOB_CRHIST_DATE+TIME (MYT -> UTC)
  ev_key    TEXT        NOT NULL,   -- 'YYYYMMDDHHMMSS' MYT (watermark / dedup key)
  PRIMARY KEY (crane_no, ev_key)
);
CREATE INDEX IF NOT EXISTS scenario_crane_deploy_ts_idx ON scenario.crane_deploy (ev_ts);

-- Landside gate events: the truck-side detail that yard_move (RTG stacking/picking) does not carry.
-- Source: TOSADM.CYC_HISTORY gate situations (GIY = gate-in->yard, OYG = yard->gate pickup,
-- GOY = gate-out at gate). Adds the external road-truck reg (REGONO) and the true gate-transaction
-- time, keyed to the container. Collected by per-CONTNO lookup (CYC_HISTORY has no time-leading
-- index, but CONTNO is the PK leading column) driven by the GI/GO containers already in yard_move.
-- NOTE: physical gate/lane is NOT recorded by TOS — LANEID is a constant 'GATE00' — so no gate id.
CREATE TABLE IF NOT EXISTS scenario.gate_event (
  contno      TEXT        NOT NULL,
  direction   TEXT        NOT NULL,   -- 'in' | 'out'
  situation   TEXT,                   -- raw CYC_HIST_SITUATION (GIY/OYG/GOY/...)
  event_ts    TIMESTAMPTZ NOT NULL,   -- CYC_HIST_DATE+TIME (MYT -> UTC)
  machine     TEXT,                   -- RTG## for yard handling, 'ATGATE' for the gate transaction
  truck_reg   TEXT,                   -- external road-truck registration (CYC_HIST_REGONO)
  block_id    INTEGER,                -- decoded HIS_PSN_IDX (same encoding as CRNT_PSN_IDX); NULL at gate
  bay_idx     INTEGER,
  row_idx     INTEGER,
  tier        INTEGER,
  PRIMARY KEY (contno, situation, event_ts)
);
CREATE INDEX IF NOT EXISTS scenario_gate_event_ts_idx ON scenario.gate_event (event_ts);
