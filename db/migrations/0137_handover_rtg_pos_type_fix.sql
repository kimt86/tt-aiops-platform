-- CHUNK 4-1 correction: 0136's BIGINT columns were never populated (Rust code that would
-- have written them was reverted after a live parsing failure). Oracle's actual types
-- (measured via ALL_TAB_COLUMNS): LNDN_TRV_RNG = NUMBER(8,1) -> JSON float (544.3 observed),
-- CRNT_PSN_IDX_NO1..4 = VARCHAR2 -> JSON string ("446" observed). Since the columns are empty,
-- DROP+ADD (metadata-only, instant) instead of ALTER TYPE (table rewrite + lock). No CHECK
-- constraints (RULES 2). vessel/voyage from 0136 are already TEXT and correct -- left untouched.
ALTER TABLE tos_handover_label DROP COLUMN IF EXISTS trv_rng;
ALTER TABLE tos_handover_label ADD  COLUMN IF NOT EXISTS trv_rng DOUBLE PRECISION; -- NUMBER(8,1)

ALTER TABLE rtg_move_log DROP COLUMN IF EXISTS pos1;
ALTER TABLE rtg_move_log DROP COLUMN IF EXISTS pos2;
ALTER TABLE rtg_move_log DROP COLUMN IF EXISTS pos3;
ALTER TABLE rtg_move_log DROP COLUMN IF EXISTS pos4;
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos1 TEXT;  -- VARCHAR2 원형 보존(디코드는 scengen 몫)
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos2 TEXT;
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos3 TEXT;
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos4 TEXT;
