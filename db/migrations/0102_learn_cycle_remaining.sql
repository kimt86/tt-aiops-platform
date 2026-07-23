-- 0102_learn_cycle_remaining.sql
-- Learned "candidate vehicle completion-time" predictor: median seconds from PICKUP to FREE
-- (label = free_ts - pickup_ts, from tt_move_log), for a candidate truck selected at its
-- move-log pickup event. Trained on the FULL tt_move_log population (label available 100%,
-- GPS-independent) so it is NOT biased toward the +22%-longer GPS-covered trips (Step-1 review).
--
-- Keying (validated in Step-2 review):
--   * (jobtype, n_containers) is the whole precision floor for location — exact pickup->drop OD
--     pairs and dense destination-only keys add <=1.4% and the exact pair slightly HURTS
--     (sparse-median noise). So the baseline conditions ONLY on jobtype + container count.
--   * dest_inflight_bucket is the extension slot for the one real live lever (LD only): the count
--     of trucks currently dispatched-and-not-yet-free to the destination QC at pickup time
--     (out-of-sample -16% medAE for LD; DS has no live lever). -1 = baseline (not conditioned).
--
-- n_containers capped at 2 (twins; label is +39% DS / +84% LD vs single — must be conditioned).
-- Refreshed by scripts/populate_learn_cycle_remaining.sql on a rolling 14-day window.
CREATE TABLE IF NOT EXISTS learn_cycle_remaining (
  jobtype              text        NOT NULL,          -- 'DS' | 'LD'
  n_containers         int         NOT NULL,          -- 1 (single) | 2 (twin)
  dest_inflight_bucket int         NOT NULL DEFAULT -1, -- -1 = baseline; >=0 = LD dest-QC in-flight tier
  n_samples            int         NOT NULL,
  remaining_p50        int         NOT NULL,          -- median seconds pickup -> free
  remaining_p90        int         NOT NULL,
  computed_at          timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (jobtype, n_containers, dest_inflight_bucket)
);
