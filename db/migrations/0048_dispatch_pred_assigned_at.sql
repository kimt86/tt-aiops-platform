-- Capture TOS's actual dispatch time (D_tos) on each prediction row, to compare against our
-- recommended dispatch deadline (D_us = dispatch_deadline_ts). There is NO dedicated dispatch
-- timestamp in TOS (JOB_ORDER_LIST has only CRE_DT, UPD_DT, YT_DIS_DT=after-arrival), so D_tos is
-- captured two ways at the first tick we observe the container assigned (ytno present):
--   became_assigned_at : when our 2-min logger first SAW it assigned (poll-lagged, ≤~3.5min late)
--   tos_upd_dt         : the row's TOS UPD_DT at that sighting (server-side last-update ≈ the
--                        assignment itself — the PRECISE D_tos; prefer this, fall back to the above)
--   became_assigned_tick : logger tick number of first-assigned sighting (debug/ordering)
-- NULL on all three = never observed assigned within its open episode (treat as "unobserved" in
-- analysis, NOT as "very late" — could be a fast Q→A→worked transition the polling missed).
ALTER TABLE dispatch_pred_sample ADD COLUMN IF NOT EXISTS became_assigned_at   timestamptz;
ALTER TABLE dispatch_pred_sample ADD COLUMN IF NOT EXISTS became_assigned_tick bigint;
ALTER TABLE dispatch_pred_sample ADD COLUMN IF NOT EXISTS tos_upd_dt           timestamptz;
-- speeds the per-tick "open & not-yet-assigned" capture UPDATE
CREATE INDEX IF NOT EXISTS dispatch_pred_unassigned ON dispatch_pred_sample (contno)
  WHERE resolved_at IS NULL AND became_assigned_at IS NULL;
