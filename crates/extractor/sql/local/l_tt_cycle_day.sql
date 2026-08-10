-- K_TT_CYCLE (nightly-day path) → raw_k_tt_cycle. $1 = business date.
-- ★2026-08-10 재정의 — 산식·근거·보존 규칙은 l_tt_cycle.sql(shift path) 머리를 볼 것.
-- (요지: 사이클 = 배정 dispatch_ts → 트럭 자유 free_ts, tt_move_log, 트윈 1회 계수,
--  캡 없음. 08-10 이전 raw 행은 옛 c10 값 그대로 보존 — 기간 조회 단차는 KC 고지.)
WITH trips AS (
  SELECT ytno, jobtype AS jt, cycle_s::float8 AS cyc
    FROM tt_move_log
   WHERE business_date = $1
     AND cycle_s IS NOT NULL
     AND twin_leg_seq = 1
)
SELECT count(DISTINCT ytno)::float8                                                        AS trucks,
       count(*)::float8                                                                    AS samples,
       round(avg(cyc)::numeric, 1)::float8                                                 AS avg_sec,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY cyc))::numeric, 1)::float8        AS med_sec,
       round((percentile_cont(0.25) WITHIN GROUP (ORDER BY cyc))::numeric, 1)::float8       AS p25_sec,
       round((percentile_cont(0.75) WITHIN GROUP (ORDER BY cyc))::numeric, 1)::float8       AS p75_sec,
       count(*) FILTER (WHERE jt = 'DS')::float8                                            AS ds_samples,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY cyc)
              FILTER (WHERE jt = 'DS'))::numeric, 1)::float8                                AS ds_med_sec,
       count(*) FILTER (WHERE jt = 'LD')::float8                                            AS ld_samples,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY cyc)
              FILTER (WHERE jt = 'LD'))::numeric, 1)::float8                                AS ld_med_sec
  FROM trips
