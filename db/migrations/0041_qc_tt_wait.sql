-- K_QC_TT_WAIT: same-bay QC idle (≈ true "QC waits for truck") split out of K_QC_NOMOVE.
-- A QC idle gap where the bay/queue (MCH_OPER_QUEUENAME) is UNCHANGED across it ≈ the crane waiting
-- for the next truck at the same bay; a bay change across the gap = gantry move/hatch (NOT truck
-- wait). TOS probe (5d): ~45% of ≤30min gaps are bay changes, so K_QC_NOMOVE overstates true
-- truck-wait ~1.8x. Same source/grain as raw_k_qc_q (per QC per day) → just add columns.
ALTER TABLE raw_k_qc_q
  ADD COLUMN IF NOT EXISTS same_bay_periods   INTEGER,
  ADD COLUMN IF NOT EXISTS same_bay_avg_sec   NUMERIC(10,1),
  ADD COLUMN IF NOT EXISTS same_bay_med_sec   NUMERIC(10,1),
  ADD COLUMN IF NOT EXISTS same_bay_total_sec NUMERIC(14,1);
