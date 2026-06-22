-- UPD_DT (TOS row last-update, parsed UTC) flows from JOB_ORDER_LIST → live_workpool so the
-- dispatch-prediction logger can record it as the precise D_tos (≈ truck-assignment time) when a
-- container first appears assigned. NULL if unset/malformed.
ALTER TABLE live_workpool ADD COLUMN IF NOT EXISTS upd_ts TIMESTAMPTZ;
