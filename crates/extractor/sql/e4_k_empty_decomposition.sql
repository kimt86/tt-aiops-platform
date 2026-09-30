-- K_EMPTY_R decomposition (jobtype x shift). Source: tos-db-research e4, Phase E.
-- Only change: JOB_HIST_DATE = '{{DAY_STR}}' (index-safe). Load: ~240K rows PK range. MEDIUM.

WITH jobs AS (
  SELECT JOB_HIST_DATE,
         JOB_HIST_JOBTYPE,
         JOB_HIST_CONTNO, JOB_HIST_POINT, JOB_HIST_SEQNO,
         CASE
           WHEN SUBSTR(MIN(JOB_HIST_TIME), 1, 2) BETWEEN '00' AND '07' THEN 'Night'
           WHEN SUBSTR(MIN(JOB_HIST_TIME), 1, 2) BETWEEN '08' AND '15' THEN 'Day'
           WHEN SUBSTR(MIN(JOB_HIST_TIME), 1, 2) BETWEEN '16' AND '23' THEN 'Evening'
         END                                            AS shift,
         MAX(CRNT_PSN_IDX_NO1)                          AS block_id,
         MAX(LNDN_TRV_RNG)                              AS lndn,
         MAX(UN_LNDN_TRV_RNG)                           AS un_lndn,
         COUNT(*)                                       AS transitions
    FROM TOSADM.JOB_ORDER_HISTORY
   WHERE JOB_HIST_DATE = '{{DAY_STR}}'
     {{TIME_PREDICATE}}
     AND LNDN_TRV_RNG    BETWEEN 0 AND 5000
     AND UN_LNDN_TRV_RNG BETWEEN 0 AND 5000
   GROUP BY JOB_HIST_DATE, JOB_HIST_JOBTYPE,
            JOB_HIST_CONTNO, JOB_HIST_POINT, JOB_HIST_SEQNO
)
SELECT
       JOB_HIST_JOBTYPE                                                       AS jobtype,
       shift,
       COUNT(*)                                                               AS jobs,
       ROUND(SUM(un_lndn) / NULLIF(SUM(lndn + un_lndn), 0), 4)                AS k_empty_ratio,
       ROUND(AVG(un_lndn), 1)                                                 AS avg_empty_m,
       ROUND(AVG(lndn), 1)                                                    AS avg_laden_m,
       ROUND(SUM(un_lndn), 0)                                                 AS total_empty_m,
       ROUND(SUM(lndn), 0)                                                    AS total_laden_m,
       COUNT(DISTINCT block_id)                                               AS distinct_blocks
  FROM jobs
 GROUP BY JOB_HIST_JOBTYPE, shift
 HAVING COUNT(*) >= 50
 ORDER BY total_empty_m DESC
 FETCH FIRST 50 ROWS ONLY
