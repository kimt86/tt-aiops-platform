-- K_CYCLE refined (per jobtype, 2-pass, mean+2sd outlier). Source: tos-db-research e3b v2.
-- Only change: JOB_HIST_DATE = '{{DAY_STR}}' (index-safe).

WITH job_events AS (
  SELECT JOB_HIST_JOBTYPE,
         JOB_HIST_CONTNO, JOB_HIST_POINT, JOB_HIST_SEQNO,
         COUNT(*) AS transitions,
         MIN(JOB_HIST_DATE||JOB_HIST_TIME)   AS first_evt,
         MAX(JOB_HIST_DATE||JOB_HIST_TIME)   AS last_evt
    FROM TOSADM.JOB_ORDER_HISTORY
   WHERE JOB_HIST_DATE = '{{DAY_STR}}'
   {{TIME_PREDICATE}}
   GROUP BY JOB_HIST_JOBTYPE, JOB_HIST_CONTNO, JOB_HIST_POINT, JOB_HIST_SEQNO
),
cycles AS (
  SELECT JOB_HIST_JOBTYPE,
         transitions,
         (TO_DATE(SUBSTR(last_evt,1,14),'YYYYMMDDHH24MISS')
        - TO_DATE(SUBSTR(first_evt,1,14),'YYYYMMDDHH24MISS')) * 86400 AS cycle_sec
    FROM job_events
   WHERE transitions > 1
),
stats AS (
  SELECT JOB_HIST_JOBTYPE,
         COUNT(*)                AS jobs,
         AVG(cycle_sec)          AS avg_cyc,
         STDDEV(cycle_sec)       AS std_cyc,
         MEDIAN(cycle_sec)       AS med_cyc,
         AVG(cycle_sec) + 2 * STDDEV(cycle_sec) AS thr,
         PERCENTILE_CONT(0.25) WITHIN GROUP (ORDER BY cycle_sec) AS p25,
         PERCENTILE_CONT(0.75) WITHIN GROUP (ORDER BY cycle_sec) AS p75,
         PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY cycle_sec) AS p95,
         AVG(transitions)        AS avg_trans
    FROM cycles
   GROUP BY JOB_HIST_JOBTYPE
)
SELECT
       s.JOB_HIST_JOBTYPE                    AS jobtype,
       s.jobs,
       ROUND(s.avg_cyc, 1)                   AS avg_sec,
       ROUND(s.med_cyc, 1)                   AS med_sec,
       ROUND(s.std_cyc, 1)                   AS std_sec,
       ROUND(s.p25, 1)                       AS p25_sec,
       ROUND(s.p75, 1)                       AS p75_sec,
       ROUND(s.p95, 1)                       AS p95_sec,
       ROUND(s.thr, 1)                       AS outlier_threshold_sec,
       (SELECT COUNT(*) FROM cycles c
         WHERE c.JOB_HIST_JOBTYPE = s.JOB_HIST_JOBTYPE
           AND c.cycle_sec > s.thr)          AS outlier_n,
       ROUND(s.avg_trans, 2)                 AS avg_transitions
  FROM stats s
 ORDER BY s.jobs DESC
 FETCH FIRST 15 ROWS ONLY
