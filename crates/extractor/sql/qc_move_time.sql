-- Per-crane per-jobtype median move interval (active cadence) over a rolling 3-day window, split by
-- SHIFT (D=Day 06–17, N=Night 18–05 terminal-local) PLUS an 'ALL' bucket (all hours). The interval
-- between consecutive completed moves of a crane, capped to 1-300s so meal/idle gaps and bay/hatch
-- transitions are excluded — leaving the genuine per-container handling cadence. Quay cranes only
-- (C/M/Z prefixes), discharge (DS) vs load (LD) separately. Self-contained (SYSDATE window), small
-- result. Feeds learn_qc_move_time (qc, jobtype, shift). 'ALL' is the fallback when a shift is sparse.
WITH m AS (
  SELECT MCH_OPER_MACHNO AS qc,
         MCH_OPER_JOBTYPE AS jt,
         TO_DATE(MCH_OPER_COMPDATE || MCH_OPER_COMPTIME, 'YYYYMMDDHH24MISS') AS e,
         SUBSTR(MCH_OPER_COMPTIME, 1, 2) AS hh
  FROM TOSADM.MCH_OPERATION
  WHERE MCH_OPER_COMPDATE BETWEEN TO_CHAR(SYSDATE - 3, 'YYYYMMDD') AND TO_CHAR(SYSDATE, 'YYYYMMDD')
    AND REGEXP_LIKE(MCH_OPER_MACHNO, '^(C|CR|DC|M|Z)[0-9]+$')
    AND MCH_OPER_JOBTYPE IN ('LD', 'DS')
),
g AS (
  SELECT qc, jt, hh, (e - LAG(e) OVER (PARTITION BY qc ORDER BY e)) * 86400 AS gap FROM m
),
gs AS (
  SELECT qc, jt, CASE WHEN hh BETWEEN '06' AND '17' THEN 'D' ELSE 'N' END AS shift, gap
  FROM g WHERE gap BETWEEN 1 AND 300
)
SELECT /*+ NO_PARALLEL */
  qc,
  jt AS jobtype,
  CASE WHEN GROUPING(shift) = 1 THEN 'ALL' ELSE shift END AS shift,
  ROUND(MEDIAN(gap)) AS med_sec,
  COUNT(*)           AS n
FROM gs
GROUP BY GROUPING SETS ((qc, jt, shift), (qc, jt))
HAVING COUNT(*) >= 30
