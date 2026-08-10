-- Local nightly-day production for K_MPH_REALTIME (Oracle original:
-- sql/c07_k_mph_realtime.sql via params::render_day). qc_move_log carries
-- business_date directly (populated at ingestion, MYT calendar day) so no UTC
-- window math is needed here -- same simplification as the other _day.sql files.
-- $1 = business date.
SELECT vessel,
       voyage,
       machno                                                        AS qc_machno,
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
       to_char(min(comp_ts) AT TIME ZONE 'Asia/Kuala_Lumpur', 'YYYYMMDDHH24MISS') AS first_move,
       to_char(max(comp_ts) AT TIME ZONE 'Asia/Kuala_Lumpur', 'YYYYMMDDHH24MISS') AS last_move
  FROM qc_move_log
 WHERE machno ~ '^(C|CR|DC|M|Z)[0-9]+$'
   AND jobtype IN ('LD', 'DS')
   AND business_date = $1
 GROUP BY vessel, voyage, machno
 ORDER BY moves DESC
-- (LIMIT 30 제거 2026-08-10 — 탐색 질의 상한 잔재)
