-- Fold a queue's crane rows by preferring a REAL crane, not by taking the largest number.
--
-- THE BUG. TOS writes the same bay-job under more than one crane — a real one and a placeholder.
-- Measured on WLHD 001/2026: every one of its 49 load queues appears twice, once under C29 and once
-- under DC02/DC03/DC04. Six such placeholders exist in the whole archive (CR4, DC01..DC05, 435 rows
-- against 10,324 real ones), and the extractor already knows they are not machines — it selects quay
-- cranes with REGEXP_LIKE(MCH_OPER_MACHNO, '^(C|M|Z)[0-9]') and CR4 never appears in qc_move_log.
--
-- Folding with max(total_qty) survives the duplicate only while both rows agree. When they disagree
-- the larger wins, and the larger is often the placeholder: 10D-L is 54 under C29 and 59 under DC03.
-- WLHD then archives 2,107 loading containers against a declared 1,818 and an actual 1,823 moves —
-- 289 boxes of plan that were never real. That is what the coverage alert has been firing on.
--
-- THE FIX. Rank real cranes first, then quantity, and take the head. A placeholder can only win when
-- the queue has no real crane at all, which is 319 of 8,187 queues — dropping those instead would
-- delete real planned work, so the fallback matters as much as the preference.
--
-- NOT A COLLECTION BUG. The archive is faithful; TOS really does emit both rows. What was wrong is
-- reading them as two separate pieces of work. Both readers are fixed here together, because they
-- are the two places that answer "how much was planned" and an answer that differs between them is
-- worse than either being wrong alone.

-- 1) Coverage view — what the alert compares against the call's declared counts.
CREATE OR REPLACE VIEW scenario.qc_plan_coverage AS
SELECT c.vessel, c.voyage, c.actber_ts, c.sealed_ts, c.sealed_reason, c.source,
       c.disvan, c.loadvan,
       p.arch_d, p.arch_l,
       CASE WHEN coalesce(c.disvan, 0) > 0 THEN round(100.0 * coalesce(p.arch_d, 0) / c.disvan, 1) END AS pct_d,
       CASE WHEN coalesce(c.loadvan, 0) > 0 THEN round(100.0 * coalesce(p.arch_l, 0) / c.loadvan, 1) END AS pct_l
  FROM scenario.qc_plan_call c
  LEFT JOIN (
      SELECT vessel, voyage,
             sum(qty) FILTER (WHERE disload = 'D') AS arch_d,
             sum(qty) FILTER (WHERE disload = 'L') AS arch_l
        FROM (
            -- One row per (call, queue): the newest revision of the row belonging to a real crane,
            -- falling back to a placeholder only when the queue has none.
            SELECT vessel, voyage, queuename,
                   (array_agg(disload   ORDER BY (qc ~ '^(C|M|Z)[0-9]') DESC, total_qty DESC))[1] AS disload,
                   (array_agg(total_qty ORDER BY (qc ~ '^(C|M|Z)[0-9]') DESC, total_qty DESC))[1] AS qty
              FROM (
                  SELECT DISTINCT ON (vessel, voyage, qc, queuename)
                         vessel, voyage, qc, queuename, disload, total_qty
                    FROM scenario.qc_plan
                   ORDER BY vessel, voyage, qc, queuename, rev DESC
              ) latest
             GROUP BY vessel, voyage, queuename
        ) folded
       GROUP BY vessel, voyage
  ) p ON p.vessel = c.vessel AND p.voyage = c.voyage;

-- 2) The subtraction function — same fold, so "how much was planned" cannot differ between the two.
CREATE OR REPLACE FUNCTION scenario.qc_plan_remaining(t0 timestamptz)
RETURNS TABLE (
    vessel      text,
    voyage      text,
    qkey        text,
    bay         int,
    dh          text,
    job         text,
    planned     int,
    done_before int,
    remaining   int,
    visits      int,
    qcs         text[],
    min_seq     int,
    done_known  boolean
) LANGUAGE sql STABLE AS $fn$
WITH latest AS (
    SELECT DISTINCT ON (p.vessel, p.voyage, p.qc, p.queuename)
           p.vessel, p.voyage, p.qc, p.queuename, p.disload, p.seq, p.total_qty
      FROM scenario.qc_plan p
     WHERE p.captured_at <= t0
     ORDER BY p.vessel, p.voyage, p.qc, p.queuename, p.rev DESC
),
folded AS (
    -- Real crane first, then quantity. max() was wrong here: a placeholder row carrying a bigger
    -- number silently became the plan (see this migration's header).
    SELECT vessel, voyage, queuename,
           (array_agg(disload   ORDER BY (qc ~ '^(C|M|Z)[0-9]') DESC, total_qty DESC))[1] AS disload,
           (array_agg(total_qty ORDER BY (qc ~ '^(C|M|Z)[0-9]') DESC, total_qty DESC))[1] AS total_qty,
           min(seq) AS min_seq
      FROM latest
     GROUP BY vessel, voyage, queuename
),
cranes AS (
    -- The crane list keeps only real machines; a placeholder is not somewhere a box can be worked.
    -- Emitted as the initial assignment, never as a join key.
    SELECT vessel, voyage,
           regexp_replace(queuename, '^([0-9]+[HD]-[DL])[0-9]+$', '\1') AS qkey,
           array_agg(DISTINCT qc ORDER BY qc) FILTER (WHERE qc ~ '^(C|M|Z)[0-9]') AS qcs
      FROM latest
     GROUP BY 1, 2, 3
),
plan AS (
    SELECT f.vessel, f.voyage,
           regexp_replace(f.queuename, '^([0-9]+[HD]-[DL])[0-9]+$', '\1') AS qkey,
           max(f.disload) AS job,
           sum(f.total_qty)::int AS planned,
           count(*)::int AS visits,
           min(f.min_seq) AS min_seq
      FROM folded f
     GROUP BY 1, 2, 3
),
done AS (
    SELECT COALESCE(q.vessel, vv.vessel) AS vessel,
           COALESCE(q.voyage, vv.voyage) AS voyage,
           regexp_replace(q.queuename, '^([0-9]+[HD]-[DL])[0-9]+$', '\1') AS qkey,
           count(*)::int AS n
      FROM public.qc_move_log q
      LEFT JOIN LATERAL (
            SELECT m.vessel, m.voyage FROM scenario.move_hist m
             WHERE m.contno = q.contno AND m.jobtype = q.jobtype
             ORDER BY abs(extract(epoch FROM m.comp_ts - q.comp_ts))
             LIMIT 1
      ) vv ON q.vessel IS NULL
     WHERE q.comp_ts < t0
       AND q.jobtype IN ('DS', 'LD')
       AND q.queuename IS NOT NULL
     GROUP BY 1, 2, 3
),
measurable AS (
    SELECT COALESCE(q.vessel, vv.vessel) AS vessel,
           COALESCE(q.voyage, vv.voyage) AS voyage
      FROM public.qc_move_log q
      LEFT JOIN LATERAL (
            SELECT m.vessel, m.voyage FROM scenario.move_hist m
             WHERE m.contno = q.contno AND m.jobtype = q.jobtype
             ORDER BY abs(extract(epoch FROM m.comp_ts - q.comp_ts))
             LIMIT 1
      ) vv ON q.vessel IS NULL
     WHERE q.queuename IS NOT NULL AND q.jobtype IN ('DS', 'LD')
     GROUP BY 1, 2
)
SELECT p.vessel, p.voyage, p.qkey,
       substring(p.qkey from '^([0-9]+)')::int          AS bay,
       substring(p.qkey from '^[0-9]+([HD])')           AS dh,
       p.job,
       p.planned,
       COALESCE(d.n, 0)                                 AS done_before,
       p.planned - COALESCE(d.n, 0)                     AS remaining,
       p.visits, c.qcs, p.min_seq,
       (m.vessel IS NOT NULL)                           AS done_known
  FROM plan p
  LEFT JOIN cranes c
         ON c.vessel = p.vessel AND c.voyage = p.voyage AND c.qkey = p.qkey
  LEFT JOIN done d
         ON d.vessel = p.vessel AND d.voyage = p.voyage AND d.qkey = p.qkey
  LEFT JOIN measurable m
         ON m.vessel = p.vessel AND m.voyage = p.voyage
$fn$;
