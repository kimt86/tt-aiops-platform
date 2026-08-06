-- CHUNK 4-1: widen handover + rtg_move_log SELECTs (Oracle load unchanged, columns land).
-- Idempotent: IF NOT EXISTS on every column. No CHECK constraints (RULES 2).
ALTER TABLE tos_handover_label ADD COLUMN IF NOT EXISTS vessel TEXT;
ALTER TABLE tos_handover_label ADD COLUMN IF NOT EXISTS voyage TEXT;
ALTER TABLE tos_handover_label ADD COLUMN IF NOT EXISTS trv_rng BIGINT;  -- k_empty용(LNDN_TRV_RNG)
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos1 BIGINT;  -- CRNT_PSN_IDX_NO1(블록)
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos2 BIGINT;  -- NO2(베이)
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos3 BIGINT;  -- NO3(열)
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos4 BIGINT;  -- NO4(단)
