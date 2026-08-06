-- Local parity for K_MPH (Oracle original: sql/c07_k_mph_realtime.sql, MCH_OPERATION).
-- Same filter (machno ^C[0-9]+$, jobtype LD/DS), same window bound as the Oracle shift
-- predicate (comp_ts BETWEEN start/end). Per-crane moves + active_hours(distinct hour
-- bucket of comp_ts) so the caller can fold with the SAME active-hours-weighted formula
-- as src_mph_vessels. $1,$2 = window start/end (timestamptz UTC).
-- ★정정(2차 실행 후): the original has `FETCH FIRST 30 ROWS ONLY` after `ORDER BY moves
-- DESC` -- missed in the first pass. There are commonly >30 distinct active QCs in a
-- shift window (36 measured), so without this cap the local sample_n (Σactive_hours)
-- overcounts vs Oracle's top-30-busiest-cranes truncation (measured 188 vs 173, -8%).
-- LIMIT 30 below reproduces that same truncation so the weight is comparable.
SELECT machno                                                        AS qc_machno,
       count(*)::float8                                              AS moves,
       count(*) FILTER (WHERE jobtype = 'LD')::float8                AS load_moves,
       count(*) FILTER (WHERE jobtype = 'DS')::float8                AS discharge_moves,
       count(DISTINCT date_trunc('hour', comp_ts))::float8           AS active_hours,
       round(
         (count(*)::numeric
            / nullif(count(DISTINCT date_trunc('hour', comp_ts)), 0)), 2
       )::float8                                                     AS k_mph_per_active_hour,
       count(DISTINCT trk_id)::float8                                AS distinct_trucks,
       count(DISTINCT contno)::float8                                AS distinct_containers,
       min(comp_ts)                                                  AS first_move,
       max(comp_ts)                                                  AS last_move
  FROM qc_move_log
 WHERE machno ~ '^C[0-9]+$'
   AND jobtype IN ('LD', 'DS')
   AND comp_ts >= $1 AND comp_ts < $2
 GROUP BY machno
 ORDER BY moves DESC
 LIMIT 30
