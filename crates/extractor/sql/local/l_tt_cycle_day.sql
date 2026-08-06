-- Local nightly-day production for K_TT_CYCLE (Oracle original: sql/c10_k_tt_cycle.sql
-- via params::render_day). Same technique as sql/local/l_tt_cycle.sql (shift path);
-- windowed by qc_move_log.business_date. $1 = business date.
WITH base AS (
  SELECT trk_id, jobtype AS jt, comp_ts
    FROM qc_move_log
   WHERE machno ~ '^C[0-9]+$'
     AND jobtype IN ('LD', 'DS')
     AND trk_id IS NOT NULL
     AND business_date = $1
),
seq AS (
  SELECT trk_id, jt,
         EXTRACT(EPOCH FROM (comp_ts - LAG(comp_ts) OVER (PARTITION BY trk_id ORDER BY comp_ts))) AS gap_sec
    FROM base
),
capped AS (
  SELECT trk_id, jt, gap_sec FROM seq WHERE gap_sec BETWEEN 120 AND 1200
)
SELECT count(DISTINCT trk_id)::float8                                                      AS trucks,
       count(*)::float8                                                                    AS samples,
       round(avg(gap_sec)::numeric, 1)::float8                                             AS avg_sec,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY gap_sec))::numeric, 1)::float8    AS med_sec,
       round((percentile_cont(0.25) WITHIN GROUP (ORDER BY gap_sec))::numeric, 1)::float8   AS p25_sec,
       round((percentile_cont(0.75) WITHIN GROUP (ORDER BY gap_sec))::numeric, 1)::float8   AS p75_sec,
       count(*) FILTER (WHERE jt = 'DS')::float8                                            AS ds_samples,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY gap_sec)
              FILTER (WHERE jt = 'DS'))::numeric, 1)::float8                                AS ds_med_sec,
       count(*) FILTER (WHERE jt = 'LD')::float8                                            AS ld_samples,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY gap_sec)
              FILTER (WHERE jt = 'LD'))::numeric, 1)::float8                                AS ld_med_sec
  FROM capped
