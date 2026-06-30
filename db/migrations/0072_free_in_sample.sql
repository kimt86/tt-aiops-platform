-- free_in (remaining-time-until-the-truck-frees) training + verification set. Every 60s we snapshot each
-- BUSY truck (carrying / near drop) with its features AND our current prediction + whether we called it
-- "soon-idle"; later we BACKFILL the ACTUAL free moment (its next drop) → actual_remaining_s. That single
-- column is both the training LABEL and the verification ("we said soon-idle, it actually freed N s later").
-- Today free_in is a crude (state,jobtype)→constant; this powers a real model. Filled by spawn_free_in_logger.
CREATE TABLE IF NOT EXISTS free_in_sample (
  ts                 timestamptz NOT NULL,
  ytno               text        NOT NULL,
  state              text,                 -- delivering / approaching / wait_rtg / soon_idle
  jobtype            text,
  qc                 text,
  container          text,
  secs_carrying      int,                  -- time since it picked up the current container (laden elapsed)
  nearest_rtg_m      double precision,     -- distance to nearest RTG (proxy for "near drop")
  pred_free_in_s     int,                  -- our CURRENT prediction (crude constant) at this moment
  soon_idle          boolean,              -- did we call it soon-idle now
  actual_free_at     timestamptz,          -- BACKFILLED: when the truck actually freed (next drop)
  actual_remaining_s int,                  -- BACKFILLED: actual_free_at - ts  (LABEL + verification)
  PRIMARY KEY (ytno, ts)
);
CREATE INDEX IF NOT EXISTS free_in_sample_unfilled ON free_in_sample (ts) WHERE actual_free_at IS NULL;
CREATE INDEX IF NOT EXISTS free_in_sample_ts ON free_in_sample (ts);
