-- Local parity/production for K_MPH_REALTIME shift panel (Oracle original:
-- sql/c07_k_mph_realtime.sql, MCH_OPERATION, grouped by vessel/voyage/machno).
-- Unlike l_mph.sql (machno-only, headline parity), this reproduces the ORIGINAL
-- grouping exactly (vessel, voyage, qc_machno) so it can replace src_mph_vessels'
-- Oracle fetch outright (CHUNK6 6-1) -- the returned rows feed BOTH the K_MPH
-- headline fold AND the vessel panel (crate::vessel::write_vessel_shift).
-- $1,$2 = window start/end (timestamptz UTC). first_move/last_move formatted as
-- YYYYMMDDHH24MISS in terminal-local (MYT) time to match the existing TEXT
-- convention (db/migrations/0008_vessel_shift.sql, 0002_raw_tables.sql) so
-- lexical min/max comparisons in vessel.rs still work.
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
 WHERE machno ~ '^C[0-9]+$'
   AND jobtype IN ('LD', 'DS')
   AND comp_ts >= $1 AND comp_ts < $2
 GROUP BY vessel, voyage, machno
 ORDER BY moves DESC
 LIMIT 30
