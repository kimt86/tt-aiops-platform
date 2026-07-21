-- Full-fidelity enrichment: per-voyage vessel size + berth, and per-container attributes + ship
-- cells (from BAPLIE discharge / MOVINS load). Pulled once per voyage (dedup) by `scengen enrich`,
-- so the assembler can emit a full scenario (ISO/weight/reefer/DG/OOG/cell/berth) not just moves.

-- One row per vessel call: size (CDV_VESSEL) + berth/schedule (VSB_VOYAGE).
CREATE TABLE IF NOT EXISTS scenario.vessel_call (
  vessel      TEXT NOT NULL,
  voyage      TEXT NOT NULL,
  vsl_name    TEXT,
  loa_m       NUMERIC,
  beam_m      NUMERIC,   -- often blank in source
  draft_m     NUMERIC,
  max_teu     INTEGER,   -- often blank in source
  total_bays  INTEGER,
  berthno     TEXT,
  berthside   TEXT,      -- P=port / S=starboard alongside
  startpos_m  NUMERIC,   -- along-quay position (meters)
  actber      TIMESTAMPTZ,
  actdep      TIMESTAMPTZ,
  estdep      TIMESTAMPTZ,
  cutoff      TIMESTAMPTZ,
  disvan      INTEGER,
  loadvan     INTEGER,
  enriched_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (vessel, voyage)
);

-- One row per (call, container, direction). disload D=discharge(BAPLIE) / L=load(MOVINS).
CREATE TABLE IF NOT EXISTS scenario.container (
  vessel        TEXT NOT NULL,
  voyage        TEXT NOT NULL,
  contno        TEXT NOT NULL,
  disload       TEXT NOT NULL,       -- 'D' | 'L'
  iso           TEXT,
  size          TEXT,                -- twenty|forty|forty_five (from iso)
  height        TEXT,                -- standard|high_cube (from iso)
  family        TEXT,                -- general|reefer|tank|open_top|flat_rack (conttype||iso)
  fill          TEXT,                -- full|empty
  gross_kg      INTEGER,             -- VGM/GROSWGHT parsed
  reefer_temp   TEXT,                -- raw TMPRCONT (format varies)
  imdg          TEXT,                -- IMDG class (DG only)
  un_no         TEXT,
  oog           BOOLEAN NOT NULL DEFAULT false,
  pod           TEXT,                -- discharge port
  pol           TEXT,                -- load/origin port
  operator      TEXT,
  ship_bay      INTEGER,             -- from CONTSTWG BBBRRTT
  ship_row      INTEGER,
  ship_tier     INTEGER,
  out_vessel    TEXT,                -- transship reload (BAPLIE NEXTVESSEL)
  out_voyage    TEXT,
  captured_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (vessel, voyage, contno, disload)
);
CREATE INDEX IF NOT EXISTS scenario_container_call_idx ON scenario.container (vessel, voyage);
CREATE INDEX IF NOT EXISTS scenario_container_cont_idx ON scenario.container (contno);
