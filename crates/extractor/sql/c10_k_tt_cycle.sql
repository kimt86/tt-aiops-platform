-- K_TT_CYCLE -- per-truck cycle approximation from MCH_OPERATION (NOT the container
-- handling span). A truck visits a QC once per delivery, so the interval between one
-- trucks consecutive QC moves is approximately one transport cycle (pickup, drive to
-- yard, return, next pickup). Only LD/DS (vessel) moves -- yard moves (MI/MO/LC) are
-- excluded from the headline. Each gap is labelled by the completing move type, so the
-- DS (discharge) and LD (load) cycles are also stored separately. Cap 120..1200s.
-- Type-A template: DAY_STR + TIME_PREDICATE on the completion timestamp (MchOper). LOW.
WITH base AS (
  SELECT TRK_ID, MCH_OPER_JOBTYPE AS jt,
         TO_DATE(MCH_OPER_COMPDATE||MCH_OPER_COMPTIME,'YYYYMMDDHH24MISS') AS comp_ts
    FROM TOSADM.MCH_OPERATION
   WHERE MCH_OPER_COMPDATE = '{{DAY_STR}}'
   {{TIME_PREDICATE}}
     AND TRK_ID IS NOT NULL
     AND REGEXP_LIKE(MCH_OPER_MACHNO,'^[CMZ][0-9]+$')
     AND MCH_OPER_JOBTYPE IN ('LD','DS')
     AND LENGTH(MCH_OPER_COMPTIME) = 6
     AND SUBSTR(MCH_OPER_COMPTIME,1,2) <= '23'
     AND SUBSTR(MCH_OPER_COMPTIME,3,2) <= '59'
),
seq AS (
  SELECT TRK_ID, jt,
         (comp_ts - LAG(comp_ts) OVER (PARTITION BY TRK_ID ORDER BY comp_ts)) * 86400 AS gap_sec
    FROM base
),
capped AS (
  SELECT TRK_ID, jt, gap_sec FROM seq WHERE gap_sec BETWEEN 120 AND 1200
)
SELECT /*+ NO_PARALLEL */
       COUNT(DISTINCT TRK_ID)                                          AS trucks,
       COUNT(*)                                                        AS samples,
       ROUND(AVG(gap_sec), 1)                                          AS avg_sec,
       ROUND(MEDIAN(gap_sec), 1)                                       AS med_sec,
       ROUND(PERCENTILE_CONT(0.25) WITHIN GROUP (ORDER BY gap_sec), 1) AS p25_sec,
       ROUND(PERCENTILE_CONT(0.75) WITHIN GROUP (ORDER BY gap_sec), 1) AS p75_sec,
       COUNT(CASE WHEN jt = 'DS' THEN 1 END)                           AS ds_samples,
       ROUND(MEDIAN(CASE WHEN jt = 'DS' THEN gap_sec END), 1)          AS ds_med_sec,
       COUNT(CASE WHEN jt = 'LD' THEN 1 END)                           AS ld_samples,
       ROUND(MEDIAN(CASE WHEN jt = 'LD' THEN gap_sec END), 1)          AS ld_med_sec
  FROM capped
