-- Emit one JSON object summarizing a business day's KPIs from L0, for the static
-- snapshot report. Headline aggregation follows the plan's L0->L1 rules.
-- Usage: psql "$DATABASE_URL" -tAc -v d="2026-06-04" -f scripts/snapshot.sql
SELECT json_build_object(
  'as_of', :'d',
  'headline', json_build_object(
    'K_UTIL',    (SELECT round(avg(k_util_capped)*100, 1) FROM raw_k_util_tt WHERE snapshot_date = :'d'),
    'K_EMPTY',   (SELECT round(sum(total_empty_m)/nullif(sum(jobs),0)/1000, 3) FROM raw_k_empty WHERE snapshot_date = :'d'),
    'K_EMPTY_R', (SELECT round(sum(total_empty_m)/nullif(sum(total_empty_m+total_laden_m),0)*100, 1) FROM raw_k_empty WHERE snapshot_date = :'d'),
    'K_CYCLE',   (SELECT round(sum(avg_sec*jobs)/nullif(sum(jobs),0), 0) FROM raw_k_cycle WHERE snapshot_date = :'d'),
    'K_RTG_Q', (SELECT round(sum(k_crane_q_avg_sec*in_range)/nullif(sum(in_range),0), 0) FROM raw_k_crane_q_daily WHERE work_date = :'d'),
    'K_MPH',     (SELECT round(sum(k_mph_per_active_hour*active_hours)/nullif(sum(active_hours),0), 1) FROM raw_k_mph_realtime WHERE snapshot_date = :'d')
  ),
  'sample_n', json_build_object(
    'K_UTIL',    (SELECT count(*) FROM raw_k_util_tt WHERE snapshot_date = :'d'),
    'K_EMPTY',   (SELECT sum(jobs)::int FROM raw_k_empty WHERE snapshot_date = :'d'),
    'K_CYCLE',   (SELECT sum(jobs)::int FROM raw_k_cycle WHERE snapshot_date = :'d'),
    'K_RTG_Q', (SELECT sum(in_range)::int FROM raw_k_crane_q_daily WHERE work_date = :'d'),
    'K_MPH',     (SELECT count(distinct vessel||voyage) FROM raw_k_mph_realtime WHERE snapshot_date = :'d')
  ),
  'crane_q_hour', (SELECT coalesce(json_agg(json_build_object('hour', hour, 'avg', avg_sec, 'p95', p95) ORDER BY hour), '[]')
                     FROM raw_k_crane_q_hour WHERE snapshot_date = :'d'),
  'cycle', (SELECT coalesce(json_agg(json_build_object('jobtype', jobtype, 'med', med_sec, 'p95', p95_sec, 'jobs', jobs) ORDER BY jobs DESC), '[]')
              FROM raw_k_cycle WHERE snapshot_date = :'d'),
  'empty', (SELECT coalesce(json_agg(json_build_object('jobtype', jobtype, 'shift', shift, 'ratio', round(k_empty_ratio*100,1)) ORDER BY k_empty_ratio DESC), '[]')
              FROM raw_k_empty WHERE snapshot_date = :'d'),
  'util_crane', (SELECT coalesce(json_agg(json_build_object('type', machine_type, 'pct', pct, 'n', n)), '[]')
                   FROM (SELECT machine_type, round(avg(k_util_merged_24h)*100,1) AS pct, count(*) AS n
                           FROM raw_k_util_crane WHERE snapshot_date = :'d' GROUP BY machine_type) s),
  'mph_qc', (SELECT coalesce(json_agg(json_build_object('qc', qc_machno, 'mph', k_mph_per_active_hour, 'moves', moves) ORDER BY moves DESC), '[]')
               FROM raw_k_mph_realtime WHERE snapshot_date = :'d')
);
