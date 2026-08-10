-- K_QC_NOMOVE / K_QC_Q (nightly-day path) → raw_k_qc_q. $1 = business date.
-- 산식은 l_qc_q.sql(shift path)과 동일 — 그 파일 머리의 ★2026-08-10 재정의 설명을 볼 것.
-- (요지: 배정시각(dispatch_ts·구명 st_ts)은 쓰지 않는다. 트윈을 들어올림으로 접고, 추정 물리 시작 =
--  greatest(직전 들어올림 완료, 완료 − learn_qc_move_time) 으로 바쁨 구간을 만든다.)
-- 다른 점: 창이 business_date 하나·HAVING=10(원본 day-path 그대로).
-- ⚠ 2026-08-10 이전의 raw_k_qc_q 행은 옛 산식(st_ts) 값 그대로 보존 — 기간 조회가
--    이 날짜를 걸치면 단차가 섞인다(KC kpi 문서 고지).
WITH m0 AS (
  SELECT machno AS qc, vessel, voyage, queuename AS qn, jobtype AS jt, comp_ts,
         CASE WHEN EXTRACT(EPOCH FROM (comp_ts - LAG(comp_ts) OVER (PARTITION BY machno ORDER BY comp_ts))) <= 2
              THEN 0 ELSE 1 END AS new_lift
    FROM qc_move_log
   WHERE machno ~ '^[CMZ][0-9]+$'
     AND jobtype IN ('LD', 'DS')
     AND business_date = $1
),
lifts AS (
  SELECT qc, MIN(vessel) AS vessel, MIN(voyage) AS voyage, MIN(qn) AS qn, MIN(jt) AS jt,
         MAX(comp_ts) AS e
    FROM (SELECT m0.*, SUM(new_lift) OVER (PARTITION BY qc ORDER BY comp_ts) AS lift_id FROM m0) x
   GROUP BY qc, lift_id
),
mt AS (
  SELECT qc, jobtype, med_sec FROM learn_qc_move_time WHERE shift = 'ALL'
),
jt_med AS (
  SELECT jobtype, percentile_cont(0.5) WITHIN GROUP (ORDER BY med_sec) AS med FROM mt GROUP BY jobtype
),
moves AS (
  SELECT l.qc, l.vessel, l.voyage, l.qn, l.e,
         GREATEST(
           l.e - make_interval(secs => COALESCE(t.med_sec, j.med, 100)),
           COALESCE(MAX(l.e) OVER (PARTITION BY l.qc, l.vessel, l.voyage ORDER BY l.e
                                   ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING),
                    l.e - make_interval(secs => COALESCE(t.med_sec, j.med, 100)))
         ) AS s
    FROM lifts l
    LEFT JOIN mt t     ON t.qc = l.qc AND t.jobtype = l.jt
    LEFT JOIN jt_med j ON j.jobtype = l.jt
),
flagged AS (
  SELECT qc, vessel, voyage, s, e, qn,
         CASE WHEN s > MAX(e) OVER (PARTITION BY qc, vessel, voyage
                                     ORDER BY s
                                     ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING)
              THEN 1 ELSE 0 END AS new_grp
    FROM moves
),
grouped AS (
  SELECT qc, vessel, voyage, s, e, qn,
         SUM(new_grp) OVER (PARTITION BY qc, vessel, voyage ORDER BY s) AS gid
    FROM flagged
),
merged AS (
  SELECT qc, vessel, voyage, gid,
         MIN(s) AS gs, MAX(e) AS ge, MIN(qn) AS qn
    FROM grouped
   GROUP BY qc, vessel, voyage, gid
),
gaps AS (
  SELECT qc, vessel, voyage,
         ge AS prev_end,
         LEAD(gs) OVER (PARTITION BY qc, vessel, voyage ORDER BY gs) AS next_start,
         EXTRACT(EPOCH FROM (LEAD(gs) OVER (PARTITION BY qc, vessel, voyage ORDER BY gs) - ge)) AS idle_sec,
         qn AS cur_qn,
         LEAD(qn) OVER (PARTITION BY qc, vessel, voyage ORDER BY gs) AS nxt_qn
    FROM merged
)
SELECT qc,
       count(*)::float8                                                                        AS idle_periods,
       count(*) FILTER (WHERE idle_sec BETWEEN 0   AND 60)::float8                              AS quick_under_1m,
       count(*) FILTER (WHERE idle_sec BETWEEN 60  AND 300)::float8                             AS normal_1_5m,
       count(*) FILTER (WHERE idle_sec BETWEEN 300 AND 600)::float8                             AS delayed_5_10m,
       count(*) FILTER (WHERE idle_sec BETWEEN 600 AND 1800)::float8                            AS extended_10_30m,
       count(*) FILTER (WHERE idle_sec > 1800)::float8                                          AS over_30m,
       round(avg(idle_sec)    FILTER (WHERE idle_sec BETWEEN 0 AND 1800)::numeric, 1)::float8    AS avg_idle_sec,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY idle_sec)
              FILTER (WHERE idle_sec BETWEEN 0 AND 1800))::numeric, 1)::float8                   AS med_idle_sec,
       sum(idle_sec) FILTER (WHERE idle_sec BETWEEN 0 AND 600)::float8                           AS total_tt_wait_sec,
       sum(idle_sec) FILTER (WHERE idle_sec BETWEEN 0 AND 1800)::float8                          AS total_idle_30m_sec,
       count(*) FILTER (WHERE cur_qn = nxt_qn AND idle_sec BETWEEN 0 AND 1800)::float8               AS same_bay_periods,
       round(avg(idle_sec) FILTER (WHERE cur_qn = nxt_qn AND idle_sec BETWEEN 0 AND 1800)::numeric, 1)::float8 AS same_bay_avg_sec,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY idle_sec)
              FILTER (WHERE cur_qn = nxt_qn AND idle_sec BETWEEN 0 AND 1800))::numeric, 1)::float8   AS same_bay_med_sec,
       round(sum(idle_sec) FILTER (WHERE cur_qn = nxt_qn AND idle_sec BETWEEN 0 AND 1800)::numeric, 1)::float8 AS same_bay_total_sec
  FROM gaps
 WHERE next_start IS NOT NULL
 GROUP BY qc
HAVING count(*) >= 10  -- mirrors the Oracle day-path QCQ_HAVING=10
 ORDER BY count(*) DESC
-- (LIMIT 30 제거 2026-08-10 — 원본(f2)과 동시 제거)
