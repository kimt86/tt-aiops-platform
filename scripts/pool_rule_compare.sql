-- 설계 ③ 1단계 판독 — 마감 기준 후보 풀(새 규칙) vs 현행 풀. mig0121.
--   psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/pool_rule_compare.sql
--
-- 새 규칙: 모든 작업을 `마감 − 여유(300초)` 가 이른 순으로 줄 세우고 그 시각이 지난 것만 담는다.
--          담을 게 트럭보다 적으면 트럭을 남긴다.
-- 현행:    굶은 크레인 → 출항 티어 → 크레인 도달 이른 순, 트럭 수만큼 절단(크레인당 상한 있음).
--
-- ⚠ 새 규칙은 아직 **계산만** 한다. 배차 판정은 종전 그대로다.

\set win '12 hours'
\pset null '-'

\echo ''
\echo '════ 0. ★조용히 빠지는 작업 (사용자 지시로 신설) ════'
SELECT count(*) AS 틱,
       round(avg(works_raw),1)       AS "후보로 들어옴",
       round(avg(works_no_eta),1)    AS "작업시작 시각 없음",
       round(avg(works_no_coord),1)  AS "좌표 없음",
       round(100.0*avg(works_no_eta+works_no_coord)
             /NULLIF(avg(works_raw+works_no_eta+works_no_coord),0),1) AS "제외 %",
       max(works_no_eta) AS "시각없음 최대", max(works_no_coord) AS "좌표없음 최대"
  FROM stage2_solver_shadow
 WHERE ts > now() - :'win'::interval AND works_no_eta IS NOT NULL;

\echo ''
\echo '════ 1. 두 풀의 크기와 겹침 ════'
SELECT count(*) AS 틱,
       round(avg(n_trucks),1)      AS 트럭,
       round(avg(n_works),1)       AS "현행 풀",
       round(avg(pool_new_n),1)    AS "새 규칙 풀",
       round(avg(pool_overlap_n),1) AS 겹침,
       round(100.0*avg(pool_overlap_n)/NULLIF(avg(pool_new_n),0),1) AS "새 풀 중 겹치는 %",
       round(avg(trucks_held_n),1) AS "남기는 트럭",
       round(avg(pool_overdue_n),1) AS "마감 지난 슬롯"
  FROM stage2_solver_shadow
 WHERE ts > now() - :'win'::interval AND pool_new_n IS NOT NULL;

\echo ''
\echo '════ 2. ★마감이 지난 슬롯이 쌓이나 (0 이 아니면 선단이 수요를 못 따라감) ════'
SELECT to_char(date_trunc('hour', ts AT TIME ZONE 'Asia/Kuala_Lumpur'),'HH24시') AS "현지 시각",
       count(*) AS 틱, round(avg(n_trucks),1) AS 트럭,
       round(avg(pool_overdue_n),1) AS "마감 지남", round(avg(trucks_held_n),1) AS "남긴 트럭"
  FROM stage2_solver_shadow
 WHERE ts > now() - :'win'::interval AND pool_new_n IS NOT NULL
 GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ 3. 새 규칙만 담은 작업 / 현행만 담은 작업 — 무엇이 다른가 ════'
SELECT CASE WHEN in_new_pool AND NOT in_current_pool THEN '새 규칙만'
            WHEN in_current_pool AND NOT in_new_pool THEN '현행만'
            ELSE '둘 다' END AS 구분,
       jobtype AS 작업, count(*) AS 묶음,
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY dd_slack_s)) AS "마감까지(초)",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY n))          AS "묶음 크기",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY due_slots))  AS "도래 슬롯"
  FROM stage2_pool_shadow
 WHERE ts > now() - :'win'::interval AND jobtype IN ('DS','LD')
 GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '════ 4. 조기 배차 편향이 사라지나 — 담긴 작업의 마감 여유 ════'
SELECT CASE WHEN in_new_pool THEN '새 규칙 풀' ELSE '현행 풀만' END AS 구분,
       jobtype AS 작업, count(*) AS 묶음,
       round(percentile_cont(0.10) WITHIN GROUP (ORDER BY dd_slack_s)) AS p10,
       round(percentile_cont(0.50) WITHIN GROUP (ORDER BY dd_slack_s)) AS "여유 중앙",
       round(percentile_cont(0.90) WITHIN GROUP (ORDER BY dd_slack_s)) AS p90
  FROM stage2_pool_shadow
 WHERE ts > now() - :'win'::interval AND jobtype IN ('DS','LD') AND dd_slack_s IS NOT NULL
 GROUP BY 1,2 ORDER BY 1,2;
