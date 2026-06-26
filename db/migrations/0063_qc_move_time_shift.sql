-- Per-crane move cadence, now split by SHIFT (Day/Night) plus an 'ALL' bucket. Night vs day crews
-- handle containers at different speeds (~±30%), so a shift-aware median makes the work-ETA (→ deadline)
-- more accurate. 'ALL' is kept as a fallback for cranes with too few samples in a given shift.
ALTER TABLE learn_qc_move_time ADD COLUMN IF NOT EXISTS shift text NOT NULL DEFAULT 'ALL';
-- repoint the PK to include shift
ALTER TABLE learn_qc_move_time DROP CONSTRAINT IF EXISTS learn_qc_move_time_pkey;
ALTER TABLE learn_qc_move_time ADD PRIMARY KEY (qc, jobtype, shift);
