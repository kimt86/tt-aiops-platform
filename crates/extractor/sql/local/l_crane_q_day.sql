-- Local nightly-day production for K_CRANE_Q daily (Oracle original:
-- sql/c08_k_crane_q.sql). tos_handover_label has no business_date column, so the
-- day window is expressed as [start,end) timestamptz bounds on comp_ts (the local
-- mirror of the JOB_HIST_DATE||JOB_HIST_TIME predicate column, per
-- params::TimeCol::JobHist / sql/local/l_crane_q.sql). Grouped by jobtype only
-- (single business day per call, unlike the Oracle SELECT which also groups by
-- JOB_HIST_DATE -- collapses to one value here, same as l_crane_q.sql's shift path).
-- $1,$2 = window start/end (timestamptz UTC), $3 = work_date text (YYYYMMDD).
WITH q AS (
  SELECT jobtype,
         EXTRACT(EPOCH FROM (actv_ts - dis_ts)) AS crane_q_sec
    FROM tos_handover_label
   WHERE dis_ts IS NOT NULL
     AND actv_ts IS NOT NULL
     AND comp_ts >= $1 AND comp_ts < $2
)
SELECT $3::text                                                                     AS work_date,
       jobtype,
       count(*)::float8                                                             AS events_nn,
       count(*) FILTER (WHERE crane_q_sec BETWEEN 0 AND 1800)::float8               AS in_range,
       round(avg(crane_q_sec)    FILTER (WHERE crane_q_sec BETWEEN 0 AND 1800)::numeric, 1)::float8 AS k_crane_q_avg_sec,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY crane_q_sec)
              FILTER (WHERE crane_q_sec BETWEEN 0 AND 1800))::numeric, 1)::float8    AS k_crane_q_med_sec,
       round(stddev(crane_q_sec) FILTER (WHERE crane_q_sec BETWEEN 0 AND 1800)::numeric, 1)::float8 AS k_crane_q_std_sec,
       (min(crane_q_sec) FILTER (WHERE crane_q_sec BETWEEN 0 AND 1800))::float8     AS min_sec,
       (max(crane_q_sec) FILTER (WHERE crane_q_sec BETWEEN 0 AND 1800))::float8     AS max_sec,
       count(*) FILTER (WHERE crane_q_sec < 0)::float8                              AS anomaly_negative,
       count(*) FILTER (WHERE crane_q_sec > 1800)::float8                           AS anomaly_over_30m
  FROM q
 GROUP BY jobtype
 ORDER BY in_range DESC NULLS LAST
 LIMIT 20
