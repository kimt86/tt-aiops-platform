-- 0104_cycle_pred_shadow_inflight.sql
-- Add the LD "destination-QC in-flight" lever to the candidate completion-time shadow.
--   in-flight = # OTHER LD trips dispatched-and-not-yet-free to the SAME destination QC
--   (tt_move_log.free_crane) at the candidate's pickup instant. It is an AS-OF-PICKUP quantity:
--   a pure function of state observable at pickup (other trucks dispatched before, not yet freed),
--   so recording it is leak-free (it never uses the target's own future free_ts).
--
-- Why reconstructed at BACKFILL (not captured live at insert): the predictor buckets are trained on
-- tt_move_log as-of reconstruction (learn_cycle_remaining, mig 0102), and at the live pickup instant
-- the concurrently-in-flight trucks are NOT yet in tt_move_log (it holds only COMPLETED trips), so a
-- live count would be a DIFFERENT feature than the one trained on → inconsistent. Reconstructing the
-- same as-of value at backfill keeps train/serve identical. (A true live-insert snapshot would need a
-- live assignment source + dest-QC resolution at LD pickup + retraining — a separate follow-up.)
--
-- pred_remaining_s now holds the BUCKETED (production) prediction; pred_baseline_s keeps the
-- baseline (dest_inflight_bucket=-1) prediction on the SAME row, so the live shadow is a standing
-- A/B: err_s vs err_baseline_s measures the lever's ongoing accuracy gain. Shadow-only, no dispatch.
ALTER TABLE cycle_pred_shadow
  ADD COLUMN IF NOT EXISTS dest_qc              text,   -- LD destination QC (free_crane); DS: drop block
  ADD COLUMN IF NOT EXISTS dest_inflight        int,    -- LD only: raw in-flight count at pickup; DS NULL
  ADD COLUMN IF NOT EXISTS dest_inflight_bucket int,    -- observed bucket (LD 0..6); -1 = DS/not-conditioned
  ADD COLUMN IF NOT EXISTS pred_baseline_s      int,    -- baseline (bucket -1) prediction, for the live A/B
  ADD COLUMN IF NOT EXISTS err_baseline_s       int;    -- pred_baseline_s - actual_remaining_s (signed)
