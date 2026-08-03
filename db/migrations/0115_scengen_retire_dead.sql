-- 0115 — retire three dead objects in scenario.*, found by an end-to-end audit of the collector.
-- Approved explicitly by the user on 2026-08-03 after the audit listed them.
--
-- Nothing here is in use. Each was verified twice: no reference anywhere in crates/scengen/src,
-- and no writer (no systemd timer, no call site). They are dropped rather than left in place
-- because a frozen table with plausible contents is worse than no table — the next reader has to
-- rediscover that it stopped, and the status page was already listing one of them as a stream
-- whose cursor simply never advances.
--
--   scenario.crane_deploy  TOS's crane->vessel ASSIGNMENT PLAN (JOB_CRANE_HISTORY). 2,311 rows,
--                          frozen at 2026-07-23 10:04 MYT, never given a timer. Superseded by
--                          deriving deployment from the actual move streams, after measurement
--                          showed the plan put 27% of quay moves on the WRONG ship (whole
--                          crane-shifts misassigned). Kept for a while as a plan-vs-actual
--                          comparison aid that no code ever performed.
--   scenario.coverage      0 rows, zero references. A collection-coverage ledger that was designed
--                          and never wired; per-stream scenario.watermark ages and the in-file
--                          meta.summary percentages do this job instead.
--   scenario.command       0 rows, zero references. Intended as a UI->collector command queue; the
--                          control path went through scenario.config (kill switch) + assembly_job.
--
-- RECOVERABLE. crane_deploy was built from Oracle in a single round trip (that is how its 2,311
-- rows arrived), so re-collecting it is a query, not a restore. The other two were always empty.

BEGIN;

DROP TABLE IF EXISTS scenario.crane_deploy;
DROP TABLE IF EXISTS scenario.coverage;
DROP TABLE IF EXISTS scenario.command;

-- The status page derives "is this stream alive" from watermark age. Leaving the cursor row behind
-- would keep an 11-day-old entry on a page whose whole purpose is to make silence visible.
DELETE FROM scenario.watermark WHERE source = 'crane_deploy';

COMMIT;
