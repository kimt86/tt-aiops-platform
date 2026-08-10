-- QC idle: gap between quay-crane merged active groups = QC no-move idle (K_QC_NOMOVE).
-- ALSO emits same-bay idle (K_QC_TT_WAIT, approx true QC-waits-for-truck): a gap is same-bay when
-- the queue/bay (MCH_OPER_QUEUENAME) is UNCHANGED across it; a bay change = gantry move/hatch (not
-- truck wait). Source tos-db-research phase_f f2, interval merged. Date predicate index-safe. LOW.

WITH moves AS (
  SELECT MCH_OPER_MACHNO AS qc,
         MCH_OPER_VESSEL AS vessel,
         MCH_OPER_VOYAGE AS voyage,
         TO_DATE(SUBSTR(ST_DT, 1, 14), 'YYYYMMDDHH24MISS') AS s,
         TO_DATE(MCH_OPER_COMPDATE || MCH_OPER_COMPTIME, 'YYYYMMDDHH24MISS') AS e,
         MCH_OPER_QUEUENAME AS qn
    FROM TOSADM.MCH_OPERATION
   WHERE MCH_OPER_COMPDATE = '{{DAY_STR}}'
     {{TIME_PREDICATE}}
     AND REGEXP_LIKE(MCH_OPER_MACHNO, '^(C|CR|DC|M|Z)[0-9]+$')
     AND MCH_OPER_JOBTYPE IN ('LD', 'DS')
     AND ST_DT IS NOT NULL AND LENGTH(ST_DT) >= 14
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
         MIN(s) AS gs, MAX(e) AS ge, COUNT(*) AS moves_in_grp, MIN(qn) AS qn
    FROM grouped
   GROUP BY qc, vessel, voyage, gid
),
gaps AS (
  SELECT qc, vessel, voyage,
         ge AS prev_end,
         LEAD(gs) OVER (PARTITION BY qc, vessel, voyage ORDER BY gs) AS next_start,
         (LEAD(gs) OVER (PARTITION BY qc, vessel, voyage ORDER BY gs) - ge) * 86400 AS idle_sec,
         qn AS cur_qn,
         LEAD(qn) OVER (PARTITION BY qc, vessel, voyage ORDER BY gs) AS nxt_qn
    FROM merged
)
SELECT /*+ NO_PARALLEL */
       qc,
       COUNT(*)                                                          AS idle_periods,
       SUM(CASE WHEN idle_sec BETWEEN 0   AND 60   THEN 1 END)           AS quick_under_1m,
       SUM(CASE WHEN idle_sec BETWEEN 60  AND 300  THEN 1 END)           AS normal_1_5m,
       SUM(CASE WHEN idle_sec BETWEEN 300 AND 600  THEN 1 END)           AS delayed_5_10m,
       SUM(CASE WHEN idle_sec BETWEEN 600 AND 1800 THEN 1 END)           AS extended_10_30m,
       SUM(CASE WHEN idle_sec > 1800 THEN 1 END)                         AS over_30m,
       ROUND(AVG(CASE WHEN idle_sec BETWEEN 0 AND 1800 THEN idle_sec END), 1)    AS avg_idle_sec,
       ROUND(MEDIAN(CASE WHEN idle_sec BETWEEN 0 AND 1800 THEN idle_sec END), 1) AS med_idle_sec,
       SUM(CASE WHEN idle_sec BETWEEN 0 AND 600 THEN idle_sec END)       AS total_tt_wait_sec,
       SUM(CASE WHEN idle_sec BETWEEN 0 AND 1800 THEN idle_sec END)      AS total_idle_30m_sec,
       COUNT(CASE WHEN cur_qn = nxt_qn AND idle_sec BETWEEN 0 AND 1800 THEN 1 END)            AS same_bay_periods,
       ROUND(AVG(CASE WHEN cur_qn = nxt_qn AND idle_sec BETWEEN 0 AND 1800 THEN idle_sec END), 1)    AS same_bay_avg_sec,
       ROUND(MEDIAN(CASE WHEN cur_qn = nxt_qn AND idle_sec BETWEEN 0 AND 1800 THEN idle_sec END), 1) AS same_bay_med_sec,
       ROUND(SUM(CASE WHEN cur_qn = nxt_qn AND idle_sec BETWEEN 0 AND 1800 THEN idle_sec END), 1)    AS same_bay_total_sec
  FROM gaps
 WHERE next_start IS NOT NULL
 GROUP BY qc
HAVING COUNT(*) >= {{QCQ_HAVING}}
 ORDER BY COUNT(*) DESC
-- (FETCH FIRST 30 제거 2026-08-10 — QC 가 61대라 크레인 상한이 실측을 자름)
