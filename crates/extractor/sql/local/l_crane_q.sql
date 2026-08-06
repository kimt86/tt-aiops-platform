-- Local parity for K_RTG_Q / K_CRANE_Q (Oracle original: sql/c08_k_crane_q.sql, per
-- JOB_HIST_DATE x JOB_HIST_JOBTYPE). K_CRANE_Q_sec = (ACTV_DT - YT_DIS_DT), cap 0..1800,
-- same as the original. Local source = tos_handover_label (dis_ts=YT_DIS_DT,
-- actv_ts=JOB_HIST_ACTV_DT). Window filters on comp_ts (the JOB_HIST_DATE||JOB_HIST_TIME
-- equivalent the Oracle shift predicate uses per params.rs TimeCol::JobHist), not on
-- dis_ts/actv_ts themselves -- matches PLAN-extractor.md 2-2 note ("창은 comp_ts 기준").
-- Grouped by jobtype only (work_date collapses to the caller's single shift window).
-- $1,$2 = window start/end (timestamptz UTC).
WITH q AS (
  SELECT jobtype,
         EXTRACT(EPOCH FROM (actv_ts - dis_ts)) AS crane_q_sec
    FROM tos_handover_label
   WHERE dis_ts IS NOT NULL
     AND actv_ts IS NOT NULL
     AND comp_ts >= $1 AND comp_ts < $2
)
SELECT jobtype,
       count(*)::float8                                                             AS events_nn,
       count(*) FILTER (WHERE crane_q_sec BETWEEN 0 AND 1800)::float8               AS in_range,
       round(avg(crane_q_sec)    FILTER (WHERE crane_q_sec BETWEEN 0 AND 1800)::numeric, 1)::float8 AS k_crane_q_avg_sec,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY crane_q_sec)
              FILTER (WHERE crane_q_sec BETWEEN 0 AND 1800))::numeric, 1)::float8    AS k_crane_q_med_sec,
       round(stddev(crane_q_sec) FILTER (WHERE crane_q_sec BETWEEN 0 AND 1800)::numeric, 1)::float8 AS k_crane_q_std_sec,
       min(crane_q_sec) FILTER (WHERE crane_q_sec BETWEEN 0 AND 1800)               AS min_sec,
       max(crane_q_sec) FILTER (WHERE crane_q_sec BETWEEN 0 AND 1800)               AS max_sec,
       count(*) FILTER (WHERE crane_q_sec < 0)::float8                              AS anomaly_negative,
       count(*) FILTER (WHERE crane_q_sec > 1800)::float8                           AS anomaly_over_30m
  FROM q
 GROUP BY jobtype
 ORDER BY in_range DESC NULLS LAST
 LIMIT 20  -- mirrors the original's `FETCH FIRST 20 ROWS ONLY` (inert here: <=jobtype count)
