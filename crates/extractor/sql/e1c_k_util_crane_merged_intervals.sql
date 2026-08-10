-- K_UTIL (QC + YC) — interval-merged. Source: tos-db-research e1c, validated Phase E.
-- Only change vs original: date predicate uses the literal {{DAY_STR}} (index-safe;
-- MCH_OPER_COMPDATE equality unchanged). Load: ~90K rows. LOW-MEDIUM.

WITH moves AS (
  SELECT MCH_OPER_MACHNO AS machno,
         TO_DATE(SUBSTR(ST_DT,1,14),                    'YYYYMMDDHH24MISS') AS start_dt,
         TO_DATE(MCH_OPER_COMPDATE || MCH_OPER_COMPTIME, 'YYYYMMDDHH24MISS') AS end_dt
    FROM TOSADM.MCH_OPERATION
   WHERE MCH_OPER_COMPDATE = '{{DAY_STR}}'
     AND (REGEXP_LIKE(MCH_OPER_MACHNO, '^[CMZ][0-9]+$') OR MCH_OPER_MACHNO LIKE 'RTG%')
     AND ST_DT IS NOT NULL AND LENGTH(ST_DT) >= 14
     AND LENGTH(MCH_OPER_COMPDATE || MCH_OPER_COMPTIME) = 14
),
flagged AS (
  SELECT machno, start_dt, end_dt,
         CASE WHEN start_dt > MAX(end_dt) OVER (PARTITION BY machno
                                                ORDER BY start_dt
                                                ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING)
              THEN 1 ELSE 0 END AS new_grp_flag
    FROM moves
),
grouped AS (
  SELECT machno, start_dt, end_dt,
         SUM(new_grp_flag) OVER (PARTITION BY machno ORDER BY start_dt) AS grp_id
    FROM flagged
),
merged AS (
  SELECT machno, grp_id,
         MIN(start_dt) AS grp_start,
         MAX(end_dt)   AS grp_end,
         COUNT(*)      AS moves_in_grp
    FROM grouped
   GROUP BY machno, grp_id
)
SELECT /*+ NO_PARALLEL */
       machno,
       CASE
         WHEN REGEXP_LIKE(machno, '^[CMZ][0-9]+$') THEN 'QC'
         WHEN machno LIKE 'RTG%'                THEN 'YC'
       END                                                                AS machine_type,
       COUNT(*)                                                            AS interval_groups,
       SUM(moves_in_grp)                                                   AS total_moves,
       ROUND(SUM((grp_end - grp_start) * 86400))                           AS active_sec_merged,
       ROUND(SUM((grp_end - grp_start) * 86400) / 86400.0, 4)              AS k_util_merged_24h,
       ROUND(AVG((grp_end - grp_start) * 86400), 1)                        AS avg_grp_sec,
       MAX((grp_end - grp_start) * 86400)                                  AS longest_grp_sec
  FROM merged
 GROUP BY machno
 ORDER BY total_moves DESC
-- (FETCH FIRST 60 제거 2026-08-10 — QC 61대+RTG 로 상한 절단 위험)
