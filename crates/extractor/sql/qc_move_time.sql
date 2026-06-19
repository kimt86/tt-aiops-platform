-- Per-crane per-jobtype median move interval (active cadence) over a rolling 3-day window.
-- The interval between consecutive completed moves of a crane, capped to 1-300s so meal/idle gaps
-- and bay/hatch transitions are excluded — leaving the genuine per-container handling cadence.
-- Quay cranes only (C/M/Z prefixes), discharge (DS) vs load (LD) separately. Self-contained
-- (SYSDATE window), small result (a few dozen rows). Feeds learn_qc_move_time.
WITH m AS (
  SELECT MCH_OPER_MACHNO AS qc,
         MCH_OPER_JOBTYPE AS jt,
         TO_DATE(MCH_OPER_COMPDATE || MCH_OPER_COMPTIME, 'YYYYMMDDHH24MISS') AS e
  FROM TOSADM.MCH_OPERATION
  WHERE MCH_OPER_COMPDATE BETWEEN TO_CHAR(SYSDATE - 3, 'YYYYMMDD') AND TO_CHAR(SYSDATE, 'YYYYMMDD')
    AND REGEXP_LIKE(MCH_OPER_MACHNO, '^[CMZ][0-9]+$')
    AND MCH_OPER_JOBTYPE IN ('LD', 'DS')
),
g AS (
  SELECT qc, jt, (e - LAG(e) OVER (PARTITION BY qc ORDER BY e)) * 86400 AS gap FROM m
)
SELECT /*+ NO_PARALLEL */
  qc,
  jt AS jobtype,
  ROUND(MEDIAN(CASE WHEN gap BETWEEN 1 AND 300 THEN gap END)) AS med_sec,
  COUNT(CASE WHEN gap BETWEEN 1 AND 300 THEN 1 END)           AS n
FROM g
GROUP BY qc, jt
HAVING COUNT(CASE WHEN gap BETWEEN 1 AND 300 THEN 1 END) >= 30
