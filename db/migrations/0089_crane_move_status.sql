-- Add container fill status to the crane handover logs. TOS `MCH_OPER_STATUS` data-dictionary
-- comment = "Container Status(Full,Empty...)": F = Full, M = empty (MT). ~27% of moves are empty
-- containers (empty repositioning / returns). NOT needed for the pickup-correction match (empty
-- container numbers are unique too — verified rows_per_cont ≈ 1.0), but a useful empty-vs-laden
-- trip feature (cost model: empties are lighter/faster; ops: empty-repositioning volume).
-- Populated going forward by the rtg-moves / qc-moves extractors; existing rows stay NULL.
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS status TEXT;  -- F=Full / M=empty(MT)
ALTER TABLE qc_move_log  ADD COLUMN IF NOT EXISTS status TEXT;  -- F=Full / M=empty(MT)
