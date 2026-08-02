-- Coverage view: fold the crane dimension before comparing the archive against what the call
-- declared. Same fold the rest of the subsystem already uses; this view was the last place summing
-- crane rows straight.
--
-- WHY. `qc` is part of a plan row's identity (PK is vessel, voyage, qc, queuename, rev), so a bay
-- reassigned mid-call is archived once under EACH crane it was assigned to. The mig-0110 view took
-- the latest rev per (qc, queuename) and summed that, which counted every reassigned bay once per
-- crane. Measured on the live archive: 558 bays carry more than one crane row (one carries five),
-- and 26 of the 80 live calls in a 7-day window read above 105% of their declared totals because of
-- it -- the worst at 316%. CASM 002/2026 read 6748 boxes against 3553 declared; folded it reads
-- exactly 3553, and CLTO 005/2026 likewise lands exactly on its 1512.
--
-- The fold is not a new idea here. The seal check in qc_plan.rs and scenario.qc_plan_remaining
-- (mig 0112) both already collapse crane rows with max(total_qty) per queue name -- and that is
-- precisely why a call could seal 'complete' at >=99% and then be displayed at 300% by this view.
-- The seal was right; the monitor was wrong. Keeping the same fold in both places is the point: a
-- monitor that disagrees with the thing it monitors teaches people to ignore it.
--
-- max(), not sum() or first(): the crane rows for one bay describe the same physical work, so the
-- bay counts once. 491 of the 558 duplicated bays carry identical total_qty across their cranes;
-- for the 67 where a revision moved the number, the larger figure is the safe one for a monitor
-- looking for a SHORT archive -- it will not manufacture a shortfall that is not there.
--
-- Split visits ("26H-L1") keep their own queue name and still sum. One bay worked in pieces really
-- is that many boxes, and each piece costs its own gantry move. Only the crane dimension collapses.
--
-- Column list is unchanged from mig 0110 so this is a plain CREATE OR REPLACE.
CREATE OR REPLACE VIEW scenario.qc_plan_coverage AS
SELECT c.vessel, c.voyage, c.actber_ts, c.sealed_ts, c.sealed_reason, c.source,
       c.disvan, c.loadvan,
       p.arch_d, p.arch_l,
       CASE WHEN coalesce(c.disvan, 0) > 0 THEN round(100.0 * coalesce(p.arch_d, 0) / c.disvan, 1) END AS pct_d,
       CASE WHEN coalesce(c.loadvan, 0) > 0 THEN round(100.0 * coalesce(p.arch_l, 0) / c.loadvan, 1) END AS pct_l
  FROM scenario.qc_plan_call c
  LEFT JOIN (
      SELECT vessel, voyage,
             sum(tq) FILTER (WHERE dl = 'D') AS arch_d,
             sum(tq) FILTER (WHERE dl = 'L') AS arch_l
        FROM (
            -- 2: collapse the crane dimension, so a reassigned bay is counted once
            SELECT vessel, voyage, queuename,
                   max(disload)   AS dl,
                   max(total_qty) AS tq
              FROM (
                  -- 1: newest revision of each (call, crane, queue)
                  SELECT DISTINCT ON (vessel, voyage, qc, queuename)
                         vessel, voyage, qc, queuename, disload, total_qty
                    FROM scenario.qc_plan
                   ORDER BY vessel, voyage, qc, queuename, rev DESC
              ) latest
             GROUP BY vessel, voyage, queuename
        ) folded
       GROUP BY vessel, voyage
  ) p ON p.vessel = c.vessel AND p.voyage = c.voyage;

COMMENT ON VIEW scenario.qc_plan_coverage IS
  'Archived plan vs the call''s own declared van counts. Crane dimension folded (mig 0113): a bay '
  'reassigned between cranes counts once, matching the seal check and scenario.qc_plan_remaining. '
  'Healthy calls land at 99-101%; the coverage alert fires outside 95-105%.';
