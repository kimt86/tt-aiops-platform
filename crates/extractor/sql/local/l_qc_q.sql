-- Local parity for K_QC_Q / K_QC_NOMOVE (Oracle original: sql/f2_k_qc_q.sql).
-- ★정정(PLAN-extractor.md 2-2, 1차 실행 후): the earlier "consecutive comp_ts gap"
-- shortcut was wrong (−20% vs Oracle) -- qc_move_log.st_ts IS the local copy of ST_DT, so
-- the original's INTERVAL MERGE is reproduced exactly here: per (qc, vessel, voyage),
-- flag a new merge-group whenever a move's st_ts starts after the running max of all
-- prior comp_ts (running-max island-gap technique, same window function shape as the
-- Oracle CTE chain moves->flagged->grouped->merged->gaps), then take the gap between
-- merged blocks' end and the next block's start. Same buckets/caps/same-bay logic as
-- the original. $1,$2 = window start/end (timestamptz UTC), applied on comp_ts (the
-- move's completion instant, matching the Oracle shift TIME_PREDICATE column).
WITH moves AS (
  SELECT machno AS qc, vessel, voyage, st_ts AS s, comp_ts AS e, queuename AS qn
    FROM qc_move_log
   WHERE machno ~ '^C[0-9]+$'
     AND jobtype IN ('LD', 'DS')
     AND st_ts IS NOT NULL
     AND comp_ts >= $1 AND comp_ts < $2
),
flagged AS (
  SELECT qc, vessel, voyage, s, e, qn,
         CASE WHEN s > MAX(e) OVER (PARTITION BY qc, vessel, voyage
                                     ORDER BY s
                                     ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING)
              THEN 1 ELSE 0 END AS new_grp
    FROM moves
),
grouped AS (
  SELECT qc, vessel, voyage, s, e, qn,
         SUM(new_grp) OVER (PARTITION BY qc, vessel, voyage ORDER BY s) AS gid
    FROM flagged
),
merged AS (
  SELECT qc, vessel, voyage, gid,
         MIN(s) AS gs, MAX(e) AS ge, MIN(qn) AS qn
    FROM grouped
   GROUP BY qc, vessel, voyage, gid
),
gaps AS (
  SELECT qc, vessel, voyage,
         ge AS prev_end,
         LEAD(gs) OVER (PARTITION BY qc, vessel, voyage ORDER BY gs) AS next_start,
         EXTRACT(EPOCH FROM (LEAD(gs) OVER (PARTITION BY qc, vessel, voyage ORDER BY gs) - ge)) AS idle_sec,
         qn AS cur_qn,
         LEAD(qn) OVER (PARTITION BY qc, vessel, voyage ORDER BY gs) AS nxt_qn
    FROM merged
)
SELECT qc,
       count(*)::float8                                                                        AS idle_periods,
       count(*) FILTER (WHERE idle_sec BETWEEN 0   AND 60)::float8                              AS quick_under_1m,
       count(*) FILTER (WHERE idle_sec BETWEEN 60  AND 300)::float8                             AS normal_1_5m,
       count(*) FILTER (WHERE idle_sec BETWEEN 300 AND 600)::float8                             AS delayed_5_10m,
       count(*) FILTER (WHERE idle_sec BETWEEN 600 AND 1800)::float8                            AS extended_10_30m,
       count(*) FILTER (WHERE idle_sec > 1800)::float8                                          AS over_30m,
       round(avg(idle_sec)    FILTER (WHERE idle_sec BETWEEN 0 AND 1800)::numeric, 1)::float8    AS avg_idle_sec,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY idle_sec)
              FILTER (WHERE idle_sec BETWEEN 0 AND 1800))::numeric, 1)::float8                   AS med_idle_sec,
       sum(idle_sec) FILTER (WHERE idle_sec BETWEEN 0 AND 600)::float8                           AS total_tt_wait_sec,
       sum(idle_sec) FILTER (WHERE idle_sec BETWEEN 0 AND 1800)::float8                          AS total_idle_30m_sec,
       count(*) FILTER (WHERE cur_qn = nxt_qn AND idle_sec BETWEEN 0 AND 1800)::float8               AS same_bay_periods,
       round(avg(idle_sec) FILTER (WHERE cur_qn = nxt_qn AND idle_sec BETWEEN 0 AND 1800)::numeric, 1)::float8 AS same_bay_avg_sec,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY idle_sec)
              FILTER (WHERE cur_qn = nxt_qn AND idle_sec BETWEEN 0 AND 1800))::numeric, 1)::float8   AS same_bay_med_sec,
       round(sum(idle_sec) FILTER (WHERE cur_qn = nxt_qn AND idle_sec BETWEEN 0 AND 1800)::numeric, 1)::float8 AS same_bay_total_sec
  FROM gaps
 WHERE next_start IS NOT NULL
 GROUP BY qc
HAVING count(*) >= 2  -- mirrors the Oracle shift-path QCQ_HAVING=2 (day path uses 10)
 ORDER BY count(*) DESC
 LIMIT 30  -- mirrors the original's `FETCH FIRST 30 ROWS ONLY`
