-- K_CRANE_Q daily (per work_date x jobtype). Source: tos-db-research phase_c/08, confirmed.
-- K_CRANE_Q = (ACTV_DT - YT_DIS_DT) * 86400 sec, filter 0..1800.
-- Only change: JOB_HIST_DATE = '{{DAY_STR}}' (index-safe, PK range scan).

WITH q AS (
  SELECT JOB_HIST_DATE,
         JOB_HIST_JOBTYPE,
         JOB_HIST_ARMGC,
         CASE WHEN YT_DIS_DT IS NOT NULL AND JOB_HIST_ACTV_DT IS NOT NULL
                AND LENGTH(YT_DIS_DT) >= 14 AND LENGTH(JOB_HIST_ACTV_DT) >= 14
              THEN (TO_DATE(SUBSTR(JOB_HIST_ACTV_DT,1,14), 'YYYYMMDDHH24MISS')
                  - TO_DATE(SUBSTR(YT_DIS_DT,1,14),        'YYYYMMDDHH24MISS')) * 86400
         END AS crane_q_sec
    FROM TOSADM.JOB_ORDER_HISTORY
   WHERE JOB_HIST_DATE = '{{DAY_STR}}'
     {{TIME_PREDICATE}}
     AND YT_DIS_DT IS NOT NULL
     AND JOB_HIST_ACTV_DT IS NOT NULL
)
SELECT
       JOB_HIST_DATE                                                AS work_date,
       JOB_HIST_JOBTYPE                                             AS jobtype,
       COUNT(*)                                                     AS events_nn,
       SUM(CASE WHEN crane_q_sec BETWEEN 0 AND 1800 THEN 1 END)     AS in_range,
       ROUND(AVG(CASE WHEN crane_q_sec BETWEEN 0 AND 1800 THEN crane_q_sec END), 1)    AS k_crane_q_avg_sec,
       ROUND(MEDIAN(CASE WHEN crane_q_sec BETWEEN 0 AND 1800 THEN crane_q_sec END), 1) AS k_crane_q_med_sec,
       ROUND(STDDEV(CASE WHEN crane_q_sec BETWEEN 0 AND 1800 THEN crane_q_sec END), 1) AS k_crane_q_std_sec,
       MIN(CASE WHEN crane_q_sec BETWEEN 0 AND 1800 THEN crane_q_sec END)              AS min_sec,
       MAX(CASE WHEN crane_q_sec BETWEEN 0 AND 1800 THEN crane_q_sec END)              AS max_sec,
       SUM(CASE WHEN crane_q_sec < 0 THEN 1 END)                                       AS anomaly_negative,
       SUM(CASE WHEN crane_q_sec > 1800 THEN 1 END)                                    AS anomaly_over_30m
  FROM q
 GROUP BY JOB_HIST_DATE, JOB_HIST_JOBTYPE
 ORDER BY in_range DESC NULLS LAST
 FETCH FIRST 20 ROWS ONLY
