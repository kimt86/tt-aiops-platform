-- K_MPH realtime per-QC (MCH_OPERATION). Source: tos-db-research phase_c/07.
-- moves_per_active_hour = COUNT(*) / COUNT(DISTINCT hour-of-day), LD/DS only, QC only.
-- Only change: MCH_OPER_COMPDATE = '{{DAY_STR}}' (index-safe). Load: LOW.

SELECT /*+ NO_PARALLEL */
       MCH_OPER_VESSEL                                              AS vessel,
       MCH_OPER_VOYAGE                                              AS voyage,
       MCH_OPER_MACHNO                                              AS qc_machno,
       COUNT(*)                                                     AS moves,
       SUM(CASE WHEN MCH_OPER_JOBTYPE = 'LD' THEN 1 END)            AS load_moves,
       SUM(CASE WHEN MCH_OPER_JOBTYPE = 'DS' THEN 1 END)            AS discharge_moves,
       COUNT(DISTINCT SUBSTR(MCH_OPER_COMPTIME, 1, 2))              AS active_hours,
       ROUND(COUNT(*) / NULLIF(COUNT(DISTINCT SUBSTR(MCH_OPER_COMPTIME, 1, 2)), 0), 2) AS k_mph_per_active_hour,
       COUNT(DISTINCT TRK_ID)                                       AS distinct_trucks,
       COUNT(DISTINCT MCH_OPER_CONTNO)                              AS distinct_containers,
       MIN(MCH_OPER_COMPDATE || MCH_OPER_COMPTIME)                  AS first_move,
       MAX(MCH_OPER_COMPDATE || MCH_OPER_COMPTIME)                  AS last_move
  FROM TOSADM.MCH_OPERATION
 WHERE MCH_OPER_COMPDATE = '{{DAY_STR}}'
   {{TIME_PREDICATE}}
   AND REGEXP_LIKE(MCH_OPER_MACHNO, '^[CMZ][0-9]+$')
   AND MCH_OPER_JOBTYPE IN ('LD', 'DS')
 GROUP BY MCH_OPER_VESSEL, MCH_OPER_VOYAGE, MCH_OPER_MACHNO
 ORDER BY moves DESC
-- (FETCH FIRST 30 제거 2026-08-10 — 사전조사(tos-db-research phase_c/07) 탐색 질의의 상한이
--  그대로 살아남아, 교대 후반 (선박·항차·크레인) 그룹이 30개를 넘으면 작은 선박이 통째로
--  잘렸다. 하루 그룹 수십 개 수준이라 부하 차이는 무시 가능.)
