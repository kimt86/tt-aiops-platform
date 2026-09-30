-- K_CRANE_Q hourly distribution + alert threshold. Source: tos-db-research e5, Phase E.
-- Only change: JOB_HIST_DATE = '{{DAY_STR}}' (index-safe). Load: ~240K rows PK range. MEDIUM.

WITH q AS (
  SELECT JOB_HIST_JOBTYPE,
         JOB_HIST_ARMGC,
         JOB_HIST_VESSEL || '/' || JOB_HIST_VOYAGE AS vv,
         SUBSTR(JOB_HIST_TIME, 1, 2) AS hour,
         CASE WHEN YT_DIS_DT IS NOT NULL AND JOB_HIST_ACTV_DT IS NOT NULL
                AND LENGTH(YT_DIS_DT) >= 14 AND LENGTH(JOB_HIST_ACTV_DT) >= 14
              THEN (TO_DATE(SUBSTR(JOB_HIST_ACTV_DT,1,14), 'YYYYMMDDHH24MISS')
                  - TO_DATE(SUBSTR(YT_DIS_DT,1,14),        'YYYYMMDDHH24MISS')) * 86400
         END AS crane_q_sec
    FROM TOSADM.JOB_ORDER_HISTORY
   WHERE JOB_HIST_DATE = '{{DAY_STR}}'
     AND YT_DIS_DT IS NOT NULL AND JOB_HIST_ACTV_DT IS NOT NULL
),
valid AS (
  SELECT *
    FROM q
   WHERE crane_q_sec BETWEEN 0 AND 1800
)
SELECT
       hour,
       COUNT(*)                                                 AS events,
       ROUND(AVG(crane_q_sec), 1)                              AS avg_sec,
       ROUND(MEDIAN(crane_q_sec), 1)                           AS med_sec,
       ROUND(STDDEV(crane_q_sec), 1)                           AS std_sec,
       ROUND(PERCENTILE_CONT(0.25) WITHIN GROUP (ORDER BY crane_q_sec), 1) AS p25,
       ROUND(PERCENTILE_CONT(0.75) WITHIN GROUP (ORDER BY crane_q_sec), 1) AS p75,
       ROUND(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY crane_q_sec), 1) AS p95,
       ROUND(AVG(crane_q_sec) + 2 * STDDEV(crane_q_sec), 1)    AS alert_threshold_sec,
       COUNT(DISTINCT JOB_HIST_ARMGC)                          AS distinct_cranes
  FROM valid
 GROUP BY hour
 ORDER BY hour
