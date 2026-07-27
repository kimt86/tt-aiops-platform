-- Rebuild scenario.gate_event on the MEASURED gate lifecycle. The 0101 shape was written from an
-- assumed situation set (GIY/OYG/GOY) before the source was probed; probing it changed the design:
--
--   import  GIY -> YGY        GIY = gate transaction (truck plate, lane, assigned slot, clerk)
--   export  QYG -> OYG -> GOY QYG = gate transaction (machine 'ATGATE'), GOY = truck leaves
--
-- OYG is NOT the export gate transaction — it is the yard crane lifting the box onto the truck, and
-- it equals rtg_move_log.comp_ts for a GO move, i.e. we already have it locally for free. The real
-- export intake is QYG, which 0101 did not mention at all. YGY likewise equals a GI move's comp_ts.
-- So this table stores ONLY the three events the yard stream cannot give us: GIY, QYG, GOY.
--
-- What that buys (measured over 1,000 local gate moves, 99.9% matched, truck plate agreeing 100%):
--   gate intake -> yard crane start   import  median 17.1 min   p90 38.2
--                                     export  median 30.4 min   p90 73.5
--   yard pick -> gate exit            export  median 15.6 min   p90 25.3
-- The scenario previously contained only the crane service itself (seconds), i.e. almost none of
-- the time a road truck actually spends here.
--
-- Physical gate/lane is still NOT recoverable: LANEID is the constant 'GATE00' (721 of 726 sampled,
-- rest null). CYC_HIST_USERID does vary (36 distinct clerks in the sample) and is kept as the only
-- available handle on how many gate positions were staffed — a lead, not a lane id.
DROP TABLE IF EXISTS scenario.gate_event;

CREATE TABLE scenario.gate_event (
  contno     TEXT        NOT NULL,
  visit      INTEGER     NOT NULL,   -- CYC_HIST_POINT: a container passes through many times
  situation  TEXT        NOT NULL,   -- GIY | QYG | GOY  (raw code, kept verbatim)
  direction  TEXT        NOT NULL,   -- 'in' (GIY) | 'out' (QYG, GOY)
  event_ts   TIMESTAMPTZ NOT NULL,   -- CYC_HIST_DATE+TIME (MYT -> UTC); CYC_HIST_DT is NULL here
  machine    TEXT,                   -- 'ATGATE' on QYG/GOY; the assigned yard crane on GIY
  truck_reg  TEXT,                   -- external road-truck plate; empty on GOY (carried by QYG)
  clerk      TEXT,                   -- CYC_HIST_USERID — gate operator
  block_id   INTEGER,                -- decoded HIS_PSN_IDX_NO1..4 (same encoding as CRNT_PSN_IDX)
  bay_idx    INTEGER,
  row_idx    INTEGER,
  tier       INTEGER,
  -- One gate transaction of a given kind per visit. Keying on the visit rather than on the
  -- timestamp means a re-read cannot fork a second row if TOS ever restates the time.
  PRIMARY KEY (contno, visit, situation)
);
CREATE INDEX IF NOT EXISTS scenario_gate_event_ts_idx ON scenario.gate_event (event_ts);
