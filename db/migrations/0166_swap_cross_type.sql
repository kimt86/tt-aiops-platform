-- Swaps may now pair a load (LD) truck with a discharge (DS) truck.
--
-- WHY. Since 0163 a swap was only considered between two trucks of the SAME job type — a starting
-- value chosen from field practice, not from a measurement. User decision 2026-10-12 MYT: lift it.
-- A DS truck passing near an LD job's yard block and an LD truck near a DS job's quay crane can both
-- get closer by trading, and the same-type rule could never see that pair. Every other guard is
-- unchanged (gain >= 180 s · both >= 500 m from their pickup · one swap per truck between frees ·
-- deadline protection).
--
-- WHAT CHANGES IN THIS TABLE. `jobtype` used to be the type of BOTH jobs. From now on it is the type
-- of truck A's current job (a_qc/a_queuename/a_contno), and the new `b_jobtype` is the type of truck
-- B's current job. A cross-type swap is the row with b_jobtype <> jobtype.
--
-- NULL b_jobtype = written by the old binary = same-type era, so b_jobtype is the discriminator
-- itself: read old rows as b_jobtype = jobtype. No backfill.
--
-- Nullable, no default -> metadata-only. lock_timeout so the instant ACCESS EXCLUSIVE never becomes
-- the head of a lock queue behind a long reader (the 2026-08-03 stall); just re-run. Apply BEFORE
-- the API binary that names the column — the old binary names its INSERT columns and ignores it.
SET lock_timeout = '3s';
ALTER TABLE stage2_swap_shadow ADD COLUMN IF NOT EXISTS b_jobtype TEXT;
COMMENT ON COLUMN stage2_swap_shadow.jobtype IS
  'Job type of truck A''s current job (a_*). Until 2026-10-12 (mig 0166) swaps were same-type only, so it was both jobs'' type.';
COMMENT ON COLUMN stage2_swap_shadow.b_jobtype IS
  'Job type of truck B''s current job (b_*), mig 0166. NULL = same-type era (read as = jobtype). b_jobtype <> jobtype = cross-type swap.';
