-- scenario: ISOLATED subsystem for the simulation scenario/emulator collector + assembler.
-- Everything lives in its OWN `scenario` schema (never `public`) so a fault here can NEVER
-- touch the critical dispatch/dashboard tables. The `scengen` binary is the only writer;
-- crates/api reads these for the /sim UI. Control is fully decoupled through Postgres:
--   * kill switch          = scenario.config.enabled  (UI flips it; collector reads each tick)
--   * backfill/assemble req = scenario.command / scenario.assembly_job rows the UI enqueues
-- No RPC / process-control coupling — the UI and the collector share only these tables.

CREATE SCHEMA IF NOT EXISTS scenario;

-- Singleton config + soft kill switch. The collector reads this at the start of every tick.
CREATE TABLE IF NOT EXISTS scenario.config (
  id               SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
  enabled          BOOLEAN NOT NULL DEFAULT true,   -- ★ soft kill switch (no systemd needed to stop)
  chunk_minutes    INTEGER NOT NULL DEFAULT 30,     -- move-log time-chunk size for backfill
  offpeak_only     BOOLEAN NOT NULL DEFAULT false,  -- restrict collection to the off-peak window
  offpeak_start_h  SMALLINT NOT NULL DEFAULT 1,     -- MYT hour, inclusive
  offpeak_end_h    SMALLINT NOT NULL DEFAULT 6,     -- MYT hour, exclusive
  oracle_timeout_s INTEGER NOT NULL DEFAULT 45,     -- ★ SHORTER than critical (90s): never hold the lock long
  retention_days   INTEGER NOT NULL DEFAULT 45,
  updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO scenario.config (id) VALUES (1) ON CONFLICT (id) DO NOTHING;

-- One row per run (a collector tick, a backfill, or an assembly job) = the run-state snapshot.
CREATE TABLE IF NOT EXISTS scenario.gen_run (
  run_id       BIGSERIAL PRIMARY KEY,
  kind         TEXT NOT NULL,                        -- 'collect' | 'backfill' | 'assemble'
  state        TEXT NOT NULL DEFAULT 'running',      -- 'running'|'done'|'error'|'skipped'
  phase        TEXT,                                 -- 'extract'|'transform'|'assemble'|'validate'
  window_start TIMESTAMPTZ,
  window_end   TIMESTAMPTZ,
  progress     JSONB NOT NULL DEFAULT '{}'::jsonb,   -- {chunks_total,chunks_done,cursor_ts,pct}
  load_stats   JSONB NOT NULL DEFAULT '{}'::jsonb,   -- {queries,rows_read,rows_per_s,avg_ms,timeouts,offpeak_ok}
  collection   JSONB NOT NULL DEFAULT '{}'::jsonb,   -- {vessels,containers,by_type,blocks,...}
  health       JSONB NOT NULL DEFAULT '{}'::jsonb,   -- {failed,retries,cache_hits,warnings}
  error_text   TEXT,
  started_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  finished_at  TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS scenario_gen_run_kind_idx ON scenario.gen_run (kind, started_at DESC);

-- Append-only journal (the run-state event stream): telemetry + logs.
CREATE TABLE IF NOT EXISTS scenario.gen_event (
  event_id BIGSERIAL PRIMARY KEY,
  run_id   BIGINT REFERENCES scenario.gen_run(run_id) ON DELETE CASCADE,
  ts       TIMESTAMPTZ NOT NULL DEFAULT now(),
  level    TEXT NOT NULL DEFAULT 'info',             -- info|warn|error
  kind     TEXT NOT NULL,                            -- 'query'|'chunk'|'heartbeat'|'skip'|'tick_failed'
  payload  JSONB NOT NULL DEFAULT '{}'::jsonb
);
CREATE INDEX IF NOT EXISTS scenario_gen_event_run_idx ON scenario.gen_event (run_id, ts);

-- Collector watermark (isolated from the critical etl_watermark).
CREATE TABLE IF NOT EXISTS scenario.watermark (
  source     TEXT PRIMARY KEY,                       -- 'move_hist' | 'snapshot' | ...
  cursor_evt TEXT,                                   -- last collected TOS event "YYYYMMDDHHMMSS" (MYT text,
                                                     -- lexicographic order; matches JOB_HIST_DATE||JOB_HIST_TIME)
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Coverage catalog: which [start,end) windows are collected & available to assemble (수집관리 UI).
CREATE TABLE IF NOT EXISTS scenario.coverage (
  source       TEXT NOT NULL DEFAULT 'move_hist',
  window_start TIMESTAMPTZ NOT NULL,
  window_end   TIMESTAMPTZ NOT NULL,
  rows         BIGINT,
  collected_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (source, window_start, window_end)
);

-- UI -> worker intents (backfill a gap, retention). Kill switch is in config (above).
CREATE TABLE IF NOT EXISTS scenario.command (
  cmd_id       BIGSERIAL PRIMARY KEY,
  kind         TEXT NOT NULL,                        -- 'backfill' | 'retention'
  args         JSONB NOT NULL DEFAULT '{}'::jsonb,
  state        TEXT NOT NULL DEFAULT 'pending',      -- pending|claimed|done|error
  requested_by TEXT,
  requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  claimed_at   TIMESTAMPTZ,
  finished_at  TIMESTAMPTZ,
  result       JSONB
);
CREATE INDEX IF NOT EXISTS scenario_command_pending_idx ON scenario.command (state, requested_at);

-- On-demand assembly jobs (period -> scenario + emulator JSON). LOCAL slice, zero Oracle.
CREATE TABLE IF NOT EXISTS scenario.assembly_job (
  job_id       BIGSERIAL PRIMARY KEY,
  window_start TIMESTAMPTZ NOT NULL,
  window_end   TIMESTAMPTZ NOT NULL,
  state        TEXT NOT NULL DEFAULT 'pending',      -- pending|running|done|error
  scenario_out JSONB,                                -- assembled scenario JSON
  emulator_out JSONB,                                -- assembled emulator spec JSON
  summary      JSONB,
  error_text   TEXT,
  requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  finished_at  TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS scenario_assembly_state_idx ON scenario.assembly_job (state, requested_at);

-- Collected raw move stream (append history) = the scenario backbone. Minimal for the skeleton;
-- container attrs / stowage cells / as-of snapshots are added in later migrations.
CREATE TABLE IF NOT EXISTS scenario.move_hist (
  comp_ts     TIMESTAMPTZ NOT NULL,                  -- move completion (terminal-local, MYT anchor)
  contno      TEXT NOT NULL,
  jobtype     TEXT NOT NULL,                         -- DS | LD  (restow derived later)
  vessel      TEXT,
  voyage      TEXT,
  yard_block  TEXT,                                  -- parsed from YT_TOPOS
  machno      TEXT,
  captured_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (contno, comp_ts, jobtype)
);
CREATE INDEX IF NOT EXISTS scenario_move_hist_ts_idx  ON scenario.move_hist (comp_ts);
CREATE INDEX IF NOT EXISTS scenario_move_hist_vvd_idx ON scenario.move_hist (vessel, voyage);

-- NOTE(deploy): if crates/api connects as a distinct read role, grant it:
--   GRANT USAGE ON SCHEMA scenario TO <read_role>;
--   GRANT SELECT ON ALL TABLES IN SCHEMA scenario TO <read_role>;
-- (single-DB-user deployments need nothing here.)
