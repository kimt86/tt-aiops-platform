-- Local nightly-day production for K_QC_NOMOVE / K_QC_Q (Oracle original:
-- sql/f2_k_qc_q.sql via params::render_day, HAVING=10). Same interval-merge
-- technique as sql/local/l_qc_q.sql (shift path); here windowed by
-- qc_move_log.business_date instead of a comp_ts range. $1 = business date.
WITH moves AS (
  SELECT machno AS qc, vessel, voyage, st_ts AS s, comp_ts AS e, queuename AS qn
    FROM qc_move_log
   WHERE machno ~ '^C[0-9]+$'
     AND jobtype IN ('LD', 'DS')
     AND st_ts IS NOT NULL
     AND business_date = $1
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
HAVING count(*) >= 10  -- mirrors the Oracle day-path QCQ_HAVING=10
 ORDER BY count(*) DESC
 LIMIT 30
