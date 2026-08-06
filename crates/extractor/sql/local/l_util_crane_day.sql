-- Local nightly-day production for K_UTIL_CRANE (Oracle original:
-- sql/e1c_k_util_crane_merged_intervals.sql). Same interval-merge technique as
-- sql/local/l_qc_q.sql, but a straight per-machine merge (no vessel/voyage
-- partition -- the original doesn't partition by vessel either). Union of QC
-- (qc_move_log, machno ^C[0-9]+$) and YC (rtg_move_log, machno LIKE 'RTG%' --
-- the ORIGINAL Oracle filter excludes ES%, so this mirrors that exactly, not
-- rtg_move_log's full RTG+ES coverage). $1 = business date.
WITH moves AS (
  SELECT machno, 'QC' AS machine_type, st_ts AS s, comp_ts AS e
    FROM qc_move_log
   WHERE machno ~ '^C[0-9]+$'
     AND st_ts IS NOT NULL
     AND business_date = $1
  UNION ALL
  SELECT machno, 'YC' AS machine_type, st_ts AS s, comp_ts AS e
    FROM rtg_move_log
   WHERE machno LIKE 'RTG%'
     AND st_ts IS NOT NULL
     AND business_date = $1
),
flagged AS (
  SELECT machno, machine_type, s, e,
         CASE WHEN s > MAX(e) OVER (PARTITION BY machno
                                     ORDER BY s
                                     ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING)
              THEN 1 ELSE 0 END AS new_grp
    FROM moves
),
grouped AS (
  SELECT machno, machine_type, s, e,
         SUM(new_grp) OVER (PARTITION BY machno ORDER BY s) AS gid
    FROM flagged
),
merged AS (
  SELECT machno, machine_type, gid,
         MIN(s) AS grp_start, MAX(e) AS grp_end, COUNT(*) AS moves_in_grp
    FROM grouped
   GROUP BY machno, machine_type, gid
)
SELECT machno,
       machine_type,
       count(*)::float8                                                          AS interval_groups,
       sum(moves_in_grp)::float8                                                 AS total_moves,
       round(sum(EXTRACT(EPOCH FROM (grp_end - grp_start)))::numeric)::float8    AS active_sec_merged,
       round((sum(EXTRACT(EPOCH FROM (grp_end - grp_start))) / 86400.0)::numeric, 4)::float8 AS k_util_merged_24h,
       round(avg(EXTRACT(EPOCH FROM (grp_end - grp_start)))::numeric, 1)::float8 AS avg_grp_sec,
       max(EXTRACT(EPOCH FROM (grp_end - grp_start)))::float8                    AS longest_grp_sec
  FROM merged
 GROUP BY machno, machine_type
 ORDER BY total_moves DESC
 LIMIT 60
