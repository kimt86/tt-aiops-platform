-- Phase-2 correction: pin the PICKUP completion (③ 픽업 떠남 = 상차 완료) from the TOS crane
-- ground truth. A crane's comp_ts is truck-relevant ONLY for LOAD-onto-truck ops (the pickups):
--   DS pickup = QC discharge (ship→truck)  → qc_move_log (jobtype DS)
--   LD pickup = RTG load    (block→truck)  → rtg_move_log (jobtype LD)
-- (Drops are UNLOAD-from-truck, so comp_ts is the container landing on ship/block AFTER the truck
--  was freed → NOT correctable here; truck-free stays GPS-estimated.) We KEEP the live GPS estimate
-- (pickup_left_at) and add the ground-truth completion alongside, so the dashboard shows truth when
-- present and we can measure how good the estimate was. Matched by (truck, container) via
-- tt_cycle_log.container ⨝ crane_log.contno. Populated by the API's cycle-pickup-correct loop.
ALTER TABLE tt_cycle_v2 ADD COLUMN IF NOT EXISTS pickup_done_at  TIMESTAMPTZ;  -- crane comp_ts (상차 완료)
ALTER TABLE tt_cycle_v2 ADD COLUMN IF NOT EXISTS pickup_done_src TEXT;         -- 'qc' | 'rtg'
-- match join hits crane logs by (trk_id, comp_ts). qc_move_log already has qc_move_log_trk_idx (0087).
CREATE INDEX IF NOT EXISTS rtg_move_log_trk_idx ON rtg_move_log (trk_id, comp_ts);
