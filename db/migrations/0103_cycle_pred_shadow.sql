-- 0103_cycle_pred_shadow.sql
-- SHADOW validation of the candidate-vehicle completion-time predictor (learn_cycle_remaining).
-- At each move-log PICKUP event (DS = qc_move_log, LD = rtg_move_log) a candidate is "selected";
-- we log the prediction MADE AT THAT MOMENT (pred_remaining_s from learn_cycle_remaining, keyed on
-- jobtype + inferred container count), then later backfill the ACTUAL free time from tt_move_log to
-- measure LIVE accuracy. Shadow-only — never drives dispatch. Pure Postgres (no Oracle).
--
-- captured_at = when the shadow first saw the pickup (≈ detection latency vs pickup_ts).
-- err_s = pred_remaining_s - actual_remaining_s  (signed: + = over-predicted / truck freed sooner).
CREATE TABLE IF NOT EXISTS cycle_pred_shadow (
  ytno               text        NOT NULL,
  pickup_ts          timestamptz NOT NULL,          -- crane pickup completion (= tt_move_log.pickup_ts)
  jobtype            text        NOT NULL,          -- 'DS' | 'LD'
  n_containers       int         NOT NULL,          -- provisional 1 at insert; corrected to TOS twin_group_size at backfill
  src                text        NOT NULL,          -- 'qc' (DS) | 'rtg' (LD)
  contno             text,
  pred_remaining_s   int         NOT NULL,          -- predicted seconds pickup -> free
  pred_free_at       timestamptz NOT NULL,
  captured_at        timestamptz NOT NULL DEFAULT now(),
  actual_free_at     timestamptz,
  actual_remaining_s int,
  err_s              int,
  PRIMARY KEY (ytno, pickup_ts)
);

CREATE INDEX IF NOT EXISTS cycle_pred_shadow_captured_idx ON cycle_pred_shadow (captured_at);
