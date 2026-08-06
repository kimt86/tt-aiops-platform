-- Local nightly-day production for K_CRANE_Q_HOUR (Oracle original:
-- sql/e5_k_crane_q_by_hour.sql). Same crane_q_sec definition and window handling
-- as sql/local/l_crane_q_day.sql; bucketed by hour-of-day (MYT) of comp_ts (the
-- local mirror of JOB_HIST_TIME). $1,$2 = window start/end (timestamptz UTC).
WITH q AS (
  SELECT armgc,
         to_char(comp_ts AT TIME ZONE 'Asia/Kuala_Lumpur', 'HH24') AS hour,
         EXTRACT(EPOCH FROM (actv_ts - dis_ts)) AS crane_q_sec
    FROM tos_handover_label
   WHERE dis_ts IS NOT NULL
     AND actv_ts IS NOT NULL
     AND comp_ts >= $1 AND comp_ts < $2
),
valid AS (
  SELECT * FROM q WHERE crane_q_sec BETWEEN 0 AND 1800
)
SELECT hour,
       count(*)::float8                                                             AS events,
       round(avg(crane_q_sec)::numeric, 1)::float8                                  AS avg_sec,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY crane_q_sec))::numeric, 1)::float8 AS med_sec,
       round(stddev(crane_q_sec)::numeric, 1)::float8                               AS std_sec,
       round((percentile_cont(0.25) WITHIN GROUP (ORDER BY crane_q_sec))::numeric, 1)::float8 AS p25,
       round((percentile_cont(0.75) WITHIN GROUP (ORDER BY crane_q_sec))::numeric, 1)::float8 AS p75,
       round((percentile_cont(0.95) WITHIN GROUP (ORDER BY crane_q_sec))::numeric, 1)::float8 AS p95,
       round((avg(crane_q_sec) + 2 * stddev(crane_q_sec))::numeric, 1)::float8       AS alert_threshold_sec,
       count(DISTINCT armgc)::float8                                                AS distinct_cranes
  FROM valid
 GROUP BY hour
 ORDER BY hour
