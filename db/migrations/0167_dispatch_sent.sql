-- Every pair and swap the matcher sends out is written here FIRST, and the matcher rebuilds its
-- "sent pair hold" from this table when it starts.
--
-- WHY. The hold (2026-10-08, crates/api/src/livemap.rs `held`) lived only in memory. A restart
-- emptied it, and the first tick after the restart ran on the work list that was already there —
-- landed BEFORE the restart, so it did not yet show what had just been sent. Measured on the
-- 2026-10-12 13:04:04 MYT restart (old binary): in that list generation 31 of 72 trucks got a second,
-- different box (`scripts/hold_conflict_check.sql`); in a normal 30-minute window it is 0.
-- Every deploy is an API restart. Design: docs/cycles/2026-10-12-sender-design.md (reviewed twice).
--
-- SHADOW ONLY FOR NOW. Nothing is sent to TOS yet: rows are mode='shadow', sent_at stays NULL and
-- the hold uses written_at as the sent time. tos_result/tos_detail stay NULL until the sender exists.
-- (The design's advisory-lock write connection is also left for the sender cycle.)
--
-- RELEASE. A hold is released only on evidence, never by time alone:
--   'listed'      a work list taken after the send arrived and shows the truck on that box
--   'not_listed'  a work list taken after the send arrived and does not
--   'truck_busy'  a TOS signal (pickup / new dispatch / dispatch-list entry) after the send
--   'superseded'  a newer row for the same truck won on restore
--   'expired_on_restore'  still open after 1 hour at restore time (should never happen — alerted)
--   'cancelled'   manual
--
-- Size: the design counted 42,714 new tier-1 pairs + 849 swaps a day (2026-10-11). Pruned at 21 days.
CREATE TABLE IF NOT EXISTS dispatch_sent (
  msg_id        uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
  kind          text        NOT NULL CHECK (kind IN ('assign', 'swap')),
  ytno          text        NOT NULL,
  contno        text        NOT NULL,
  jobtype       text,
  qc            text,
  swap_peer     uuid        REFERENCES dispatch_sent(msg_id) ON DELETE SET NULL,
  tick          bigint,
  basis_as_of   timestamptz,
  written_at    timestamptz NOT NULL DEFAULT clock_timestamp(),
  sent_at       timestamptz,
  tos_result    text,
  tos_detail    text,
  released_at   timestamptz,
  release_why   text,
  mode          text        NOT NULL CHECK (mode IN ('live', 'shadow'))
);
CREATE INDEX IF NOT EXISTS dispatch_sent_open  ON dispatch_sent (written_at) WHERE released_at IS NULL;
CREATE INDEX IF NOT EXISTS dispatch_sent_truck ON dispatch_sent (ytno, written_at);
CREATE INDEX IF NOT EXISTS dispatch_sent_written ON dispatch_sent (written_at);
CREATE INDEX IF NOT EXISTS dispatch_sent_peer  ON dispatch_sent (swap_peer) WHERE swap_peer IS NOT NULL;

COMMENT ON TABLE dispatch_sent IS
  'Pairs (kind=assign) and swaps (kind=swap, one row per truck linked by swap_peer) the matcher sent, written BEFORE sending; the matcher restores its hold from open rows (released_at IS NULL) at start. mig 0167, 2026-10-12. mode=shadow: nothing actually sent.';
COMMENT ON COLUMN dispatch_sent.basis_as_of IS 'When the work list the decision used was taken (live_workpool.as_of_ts).';
COMMENT ON COLUMN dispatch_sent.written_at IS 'clock_timestamp() of the write. In shadow (and while sent_at is NULL) this is the hold''s sent time.';
COMMENT ON COLUMN dispatch_sent.sent_at IS 'When sending FINISHED. NULL = written but not known to be sent (always NULL in shadow).';
COMMENT ON COLUMN dispatch_sent.release_why IS 'listed | not_listed | truck_busy | superseded | expired_on_restore | cancelled. Never released by time alone.';
