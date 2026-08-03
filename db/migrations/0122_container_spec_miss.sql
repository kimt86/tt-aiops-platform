-- Remember which container numbers the yard inventory did NOT answer for, so a question that can
-- never be answered is not asked again on every tick.
--
-- WHY THIS IS NEEDED NOW AND WAS NOT BEFORE. cont_spec had no miss table on purpose: both of its
-- sources carry their own natural bound. The standing yard bounds itself (leaving the yard deletes
-- the row), and the recent-gate source is bounded by LOOKBACK_H — a box that had already left when
-- we asked gets a few more chances inside that window and then falls out on its own. The comment in
-- cont_spec.rs says exactly this, and it was right.
--
-- The source being added has no such bound. public.live_workpool is a live snapshot of queued work,
-- and a box can sit in it while never appearing in TOSADM.CYY_CONTAINER — the load moves whose boxes
-- are already aboard are the case measured earlier at a 40.7% answer rate. Without a record of the
-- misses, those unanswerable numbers would refill the batch every tick, crowding out the ones that
-- can answer. That is the failure the existing comment warns about ("a low-hit source silently fills
-- the batch with questions that can never answer"), so the guard goes in FIRST, before the source.
--
-- NOT A PERMANENT BLACKLIST. Containers come back: a box that gates out and returns is in the yard
-- again, and its size is then knowable. So a miss suppresses re-asking only until the box shows
-- FRESH activity — the candidate query compares the box's newest yard/gate/queue timestamp against
-- last_asked, and anything newer makes it eligible again. No cooldown constant to tune, and no way
-- for a miss to blind us to a box that really did come back.
CREATE TABLE IF NOT EXISTS scenario.container_spec_miss (
    contno     text        NOT NULL PRIMARY KEY,
    last_asked timestamptz NOT NULL DEFAULT now(),
    attempts   int         NOT NULL DEFAULT 1
);

-- Housekeeping: a miss is only useful while it is suppressing a question. Once the box has been
-- quiet longer than any source's lookback, the row is inert, and cont_spec prunes it on its own tick
-- so this table cannot become the fifth scenario table with no retention.
CREATE INDEX IF NOT EXISTS container_spec_miss_asked_idx ON scenario.container_spec_miss (last_asked);
