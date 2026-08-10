-- K_QC_Q / K_QC_NOMOVE (shift path). $1,$2 = window start/end (timestamptz UTC), on comp_ts.
--
-- ★2026-08-10 재정의(사용자 지시): 바쁨 구간의 시작을 st_ts 로 쓰지 않는다.
-- qc_move_log.dispatch_ts(구명 st_ts·mig0147 개명)는 크레인 시작이 아니라 **트럭 배정 시각**(TOS ST_DT 소급기입)이라,
-- [st_ts, comp_ts] 를 바쁨으로 치면 트럭이 달려오는 시간(양하 중앙 ~7분·적하 ~24분)까지
-- 크레인이 일한 것이 되어 굶김을 1/10 로 숨겼다(실측: 5분+ 갭 34~87 → 649~831건/일).
-- TOS 는 크레인 물리 시작을 어디에도 기록하지 않으므로(2026-08-10 발굴조사: MCH_OPERATION
-- 행은 완료 시 통째 삽입·CYC_HISTORY 안벽 이벤트도 완료 시점) **추정**한다:
--   추정 시작 = greatest(같은 (크레인,항차) 직전 들어올림 완료, 완료 − 학습 무브시간)
-- 트윈(상자 2개·들어올림 1회, 완료 0~2초 차 연속 2행)은 들어올림 하나로 접는다.
-- 무브시간은 learn_qc_move_time(shift='ALL'·들어올림 단위 학습, l_qc_move_time.sql).
-- ⚠주의: 학습 무브시간이 중앙값이라 그보다 느린 정상 무브가 1~2분 이하의 가짜 갭을
-- 만든다 — 버킷 delayed_5_10m + extended_10_30m + over_30m(5분+)이 신뢰 구간이다.
--
-- 이하 병합·갭·버킷·HAVING·LIMIT 골격은 원본(f2)과 동일 — 출력 모양 불변이라
-- 소비자(shift.rs 가중평균 → kpi_shift / api agg.rs·routes.rs)는 코드 변경 없이
-- 값만 정직해진다. 같은 산식 3본: 이 파일 + l_qc_q_day.sql + l_util_crane_day.sql(QC 절).
WITH m0 AS (
  SELECT machno AS qc, vessel, voyage, queuename AS qn, jobtype AS jt, comp_ts,
         CASE WHEN EXTRACT(EPOCH FROM (comp_ts - LAG(comp_ts) OVER (PARTITION BY machno ORDER BY comp_ts))) <= 2
              THEN 0 ELSE 1 END AS new_lift
    FROM qc_move_log
   WHERE machno ~ '^(C|CR|DC|M|Z)[0-9]+$'
     AND jobtype IN ('LD', 'DS')
     AND comp_ts >= $1 AND comp_ts < $2
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
moves AS (  -- s = 추정 물리 시작 (직전 들어올림 완료 밑으로는 내려가지 않는다 — 크레인은 직렬)
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
HAVING count(*) >= 2  -- mirrors the Oracle shift-path QCQ_HAVING=2 (day path uses 10)
 ORDER BY count(*) DESC
-- (LIMIT 30 제거 2026-08-10 — 원본(f2)과 동시 제거, 패리티 유지)
