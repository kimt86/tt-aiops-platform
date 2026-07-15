-- tt_move_log: per-TT-per-container completed work leg, anchored on the two TOS-AUTHORITATIVE
-- moments verified against source + data (2026-07-15):
--   dispatch_ts = JOB_ORDER_LIST.YT_DIS_DT (the instant TOS dispatched THIS truck — assignment;
--                 "DIS" = DISpatch, not discharge; confirmed in oss-core jobschedule.relay
--                 insertJobOrderList, and dis_ts ≈ our opened_at within ~14s for DS)
--   free_ts     = the crane handover that FREES the truck (cycle end) — validated ±27–44s vs GPS,
--                 and cycle_s matches tt_cycle_v2 (opened→dropped) within ±5s.
-- One row per finished LD/DS delivery LEG. A TWIN trip (1 truck, 1 dispatch, 2 containers) is 2 rows
-- sharing (ytno, dispatch_ts) — see is_twin/twin_* below; collapse on (ytno,dispatch_ts) for any
-- truck-time/utilization KPI, keep both legs for per-container/throughput.
-- Purpose: verification, KPI (cycle time / utilization), and per-truck work-cycle history.
--
-- Sources (Postgres-side join of already-extracted tables → ZERO extra Oracle load):
--   tos_handover_label : dis_ts (=YT_DIS_DT dispatch) + comp_ts/armgc = the RTG (yard) handover
--                        → LD: RTG=pickup(block) · DS: RTG=drop=free(block)
--   qc_move_log        : comp_ts/machno = the QC (quay) handover
--                        → LD: QC=drop=free(quay) · DS: QC=pickup(quay)

DROP TABLE IF EXISTS tt_move_log;  -- brand-new table (introduced this migration); safe to recreate
CREATE TABLE tt_move_log (
  ytno            TEXT        NOT NULL,   -- truck (= YT / GPS device id, e.g. TT1153)
  contno          TEXT        NOT NULL,   -- container
  jobtype         TEXT        NOT NULL,   -- LD (yard→ship) / DS (ship→yard)
  dispatch_ts     TIMESTAMPTZ NOT NULL,   -- YT_DIS_DT: TOS assigned this truck (authoritative start)
  pickup_ts       TIMESTAMPTZ,            -- crane loaded the truck (LD=RTG block / DS=QC quay)
  free_ts         TIMESTAMPTZ NOT NULL,   -- crane freed the truck = cycle end (authoritative)
  pickup_crane    TEXT,                   -- crane that loaded the truck
  free_crane      TEXT,                   -- crane that freed the truck
  cycle_s         INTEGER,                -- free_ts - dispatch_ts  (full job: assignment → free)
  empty_s         INTEGER,                -- pickup_ts - dispatch_ts. NOTE: NOT clean empty-drive —
                                          -- conflates crane staging + assignment wait (can be ~0 when
                                          -- pre-staged). Do NOT use as an OD empty-drive label.
  laden_s         INTEGER,                -- free_ts - pickup_ts (loaded drive + destination-crane queue)
  status          TEXT,                   -- container status: F=full / M=empty (repositioning)
  is_twin         BOOLEAN     NOT NULL DEFAULT false, -- part of a 2-container twin trip
  twin_group_size SMALLINT    NOT NULL DEFAULT 1,     -- rows sharing (ytno, dispatch_ts)
  twin_leg_seq    SMALLINT    NOT NULL DEFAULT 1,     -- 1..n leg order within the trip (by free_ts)
  business_date   DATE        NOT NULL,   -- terminal-local (MYT, UTC+8) date of free_ts
  shift           TEXT,                   -- D (06–17) / N (18–05) terminal-local
  captured_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (ytno, contno, dispatch_ts) -- one leg per (truck, container, dispatch); idempotent.
                                          -- (twins share ytno+dispatch but differ by contno, and LD
                                          --  twins share free_ts, so free_ts cannot be in the key)
);
CREATE INDEX IF NOT EXISTS tt_move_log_bd_idx   ON tt_move_log (business_date, shift, free_ts);
CREATE INDEX IF NOT EXISTS tt_move_log_yt_idx   ON tt_move_log (ytno, free_ts);
CREATE INDEX IF NOT EXISTS tt_move_log_trip_idx ON tt_move_log (ytno, dispatch_ts);
CREATE INDEX IF NOT EXISTS tt_move_log_cont_idx ON tt_move_log (contno);
CREATE INDEX IF NOT EXISTS tt_move_log_free_idx ON tt_move_log (free_ts);
