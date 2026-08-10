-- 배차 마감 두 축 비교 — 옛 축(매처가 쓰는 값) vs 설계 ②(크레인 시작 − 트럭 준비시간). mig0120.
--   psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/deadline_axis_compare.sql
--
-- ■ 무엇을 판정하나
-- 두 마감 중 **어느 쪽이 크레인이 실제로 그 작업을 하는 시각을 잘 맞히는가**.
--   옛 축 : max(크레인 시작, 지금) + (크레인당 트럭 상한 ÷ 2) × 무브시간
--           → deadline_slack_s + od_p90_s = 추천 시각으로부터 마감까지 남은 초
--   새 축 : 크레인 시작 − 트럭 준비시간(learn_dispatch_lead.realized_lead_s)
--           → dd_slack_s = 추천 시각으로부터 배차마감까지 남은 초
--
-- ■ ⚠ 이 비교의 한계 (먼저 읽을 것)
-- 정답으로 삼는 "그 큐의 다음 크레인 핸드오버"는 **버킷 단위 추천을 컨테이너 단위 사건으로**
-- 채점하는 프록시다. 2026-08-03 에 이 혼동으로 한 번 오독했다(project_work_eta_target_choice 철회).
-- 그래서 아래 ③에 **위약(placebo) 대조**를 넣었다 — 두 축의 차이가 정답지와 무관하게도 나오는지.
-- ③에서 위약이 같은 크기의 차이를 만들면 ①②의 결론을 신뢰하지 말 것.
--
-- ■ 전환 기준
-- 새 축의 절대 오차가 옛 축보다 **뚜렷하게 작고**(양쪽 작업유형에서), 위약이 그걸 설명하지 못할 때만
-- 판정을 새 축으로 옮긴다. 애매하면 옮기지 않는다.

\set win '16 hours'
\pset null '-'

\echo ''
\echo '════ 0. 표본 ════'
SELECT jobtype AS 작업, count(*) AS 추천,
       count(dd_slack_s) AS "새 축 있는 행",
       min(ts)::timestamp(0) AS 시작, max(ts)::timestamp(0) AS 끝,
       round(avg(dd_lead_s)) AS "뺀 준비시간(초)"
  FROM stage2_match_shadow
 WHERE ts > now() - :'win'::interval AND jobtype IN ('DS','LD')
 GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ 1. 두 마감이 서로 얼마나 다른가 ════'
SELECT jobtype AS 작업, count(*) AS n,
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY deadline_slack_s + od_p90_s)) AS "옛 마감까지(초)",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY dd_slack_s))                  AS "새 마감까지(초)",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY (deadline_slack_s + od_p90_s) - dd_slack_s)) AS "옛 축이 늦은 정도",
       round(100.0*count(*) FILTER (WHERE dd_slack_s < 0)/count(*),1) AS "새 축 기준 이미 늦음 %"
  FROM stage2_match_shadow
 WHERE ts > now() - :'win'::interval AND jobtype IN ('DS','LD') AND dd_slack_s IS NOT NULL
 GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ 2. ★핵심 — 어느 축이 실제 크레인 작업 시각을 잘 맞히나 ════'
\echo '   (정답 = 그 큐에서 추천 시각 이후 처음 일어난 크레인↔트럭 핸드오버)'
WITH m AS (
  SELECT ts, qc, queuename, jobtype,
         deadline_slack_s + od_p90_s AS old_h,
         dd_slack_s                  AS new_h
    FROM stage2_match_shadow
   WHERE ts > now() - :'win'::interval AND jobtype IN ('DS','LD')
     AND queuename IS NOT NULL AND dd_slack_s IS NOT NULL
), t AS (
  SELECT m.*, extract(epoch FROM d.comp - m.ts) AS truth_h
    FROM m JOIN LATERAL (
      SELECT q.comp_ts AS comp FROM qc_move_log q
       WHERE q.machno = m.qc AND q.queuename = m.queuename AND q.dispatch_ts > m.ts
       ORDER BY q.dispatch_ts LIMIT 1) d ON true
)
SELECT jobtype AS 작업, count(*) AS n,
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY truth_h))            AS "실제(초)",
       round(avg(abs(old_h - truth_h)))                                       AS "옛 축 절대오차",
       round(avg(abs(new_h - truth_h)))                                       AS "새 축 절대오차",
       round(100.0*(avg(abs(old_h - truth_h)) - avg(abs(new_h - truth_h)))
             / NULLIF(avg(abs(old_h - truth_h)),0),1)                         AS "새 축 개선 %",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY old_h - truth_h))    AS "옛 축 치우침",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY new_h - truth_h))    AS "새 축 치우침"
  FROM t GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ 3. 위약 대조 — 정답지와 무관한 이동으로도 같은 차이가 나오나 ════'
\echo '   (옛 축에서 단순히 상수를 빼기만 한 가짜 축. 이게 새 축만큼 좋으면 ②의 이득은 허수다)'
WITH m AS (
  SELECT ts, qc, queuename, jobtype,
         deadline_slack_s + od_p90_s AS old_h, dd_slack_s AS new_h,
         (deadline_slack_s + od_p90_s) - CASE WHEN jobtype='LD' THEN 1448 ELSE 455 END AS placebo_h
    FROM stage2_match_shadow
   WHERE ts > now() - :'win'::interval AND jobtype IN ('DS','LD')
     AND queuename IS NOT NULL AND dd_slack_s IS NOT NULL
), t AS (
  SELECT m.*, extract(epoch FROM d.comp - m.ts) AS truth_h
    FROM m JOIN LATERAL (
      SELECT q.comp_ts AS comp FROM qc_move_log q
       WHERE q.machno = m.qc AND q.queuename = m.queuename AND q.dispatch_ts > m.ts
       ORDER BY q.dispatch_ts LIMIT 1) d ON true
)
SELECT jobtype AS 작업, count(*) AS n,
       round(avg(abs(old_h - truth_h)))     AS "옛 축",
       round(avg(abs(new_h - truth_h)))     AS "새 축",
       round(avg(abs(placebo_h - truth_h))) AS "위약(상수만 뺀 것)"
  FROM t GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ 4. 시간대별 안정성 — 야간/주간에서 결론이 뒤집히나 ════'
WITH m AS (
  SELECT ts, qc, queuename, jobtype,
         deadline_slack_s + od_p90_s AS old_h, dd_slack_s AS new_h
    FROM stage2_match_shadow
   WHERE ts > now() - :'win'::interval AND jobtype IN ('DS','LD')
     AND queuename IS NOT NULL AND dd_slack_s IS NOT NULL
), t AS (
  SELECT m.*, extract(epoch FROM d.comp - m.ts) AS truth_h
    FROM m JOIN LATERAL (
      SELECT q.comp_ts AS comp FROM qc_move_log q
       WHERE q.machno = m.qc AND q.queuename = m.queuename AND q.dispatch_ts > m.ts
       ORDER BY q.dispatch_ts LIMIT 1) d ON true
)
SELECT to_char(date_trunc('hour', ts AT TIME ZONE 'Asia/Kuala_Lumpur'), 'HH24시') AS "현지 시각",
       jobtype AS 작업, count(*) AS n,
       round(avg(abs(old_h - truth_h))) AS "옛 축",
       round(avg(abs(new_h - truth_h))) AS "새 축"
  FROM t GROUP BY 1,2 HAVING count(*) >= 30 ORDER BY 1,2;

\echo ''
\echo '════ 5. 참고 — 현장 크레인 기아율 (판정의 현실 대조군) ════'
SELECT round(100.0*count(*) FILTER (WHERE starving_real)/count(*),1) AS "기아 %",
       count(*) AS 표본
  FROM qc_wait_qc_sample WHERE ts > now() - :'win'::interval;
