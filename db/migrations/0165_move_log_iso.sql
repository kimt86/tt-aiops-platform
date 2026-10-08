-- Carry each moved box's ISO 6346 size-type on both crane move logs.
--
-- WHY. The scenario generator (~/projects/tt-scengen) has to state every moved box's size, and the
-- only place it could get one was its own dictionary filled from TOSADM.CYY_CONTAINER — the CURRENT
-- yard inventory. A box can only be looked up there while it sits in the yard, so the dictionary had
-- to be fed continuously, and a box that arrived and left while nobody was looking stayed unknown for
-- good. Measured: on MYT 2026-10-07, after seven weeks without that feed, the dictionary knew 6.6% of
-- the day's load boxes and 8.6% of its gate-out boxes.
--
-- The move row itself carries the answer. MCH_OPER_ISO is VARCHAR2(4) on the same MCH_OPERATION row
-- both streams already fetch, so — exactly as with 0109 — reading it costs no extra Oracle work: same
-- rows, same PK range scan, a few bytes more per row. Measured MYT 2026-10-07 (one full day):
-- QC 23,129/23,129 · RTG 45,853/45,853 · ES 5,676/5,676 · RS 746/747 populated, every value a
-- well-formed four-character code ("22G1", "45R1", "L5G1").
--
-- NULL IS NORMAL FOR OLD ROWS. The insert path is ON CONFLICT DO NOTHING, so rows landed before the
-- new extractor stay NULL, and no backfill is part of this (user decision 2026-10-08: the column is
-- usable from the day it starts filling).
--
-- Widening only, nullable, no default -> metadata-only (PG 17), the same shape as 0109/0089. The
-- lock is still ACCESS EXCLUSIVE for an instant, and it queues behind any long reader of these tables
-- — and every live writer then queues behind IT (the 2026-08-03 stall). lock_timeout makes it give up
-- instead of becoming that queue head; just re-run.
--
-- Ordering matters: apply this BEFORE deploying the extractor that writes the column. The old binary
-- names its INSERT columns explicitly and ignores the new one; a new binary against the old schema
-- fails every tick on undefined_column and freezes both move streams.
SET lock_timeout = '3s';
ALTER TABLE qc_move_log  ADD COLUMN IF NOT EXISTS iso TEXT; -- ISO 6346, e.g. "45R1"
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS iso TEXT;
