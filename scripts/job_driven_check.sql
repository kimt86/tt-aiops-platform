-- 작업 기준 배차(mig 0163·match_ver=1) 완료 기준 점검 — HANDOFF.md DONE CRITERIA 의 숫자를 낸다.
--
-- 쓰는 법:  psql -v from="'2026-10-06 15:00+08'" -f scripts/job_driven_check.sql
--   from = 배포 경계(또는 그 뒤 아무 시각). 판별은 match_ver=1 로 하므로 경계를 조금 앞으로 잡아도 섞이지 않는다.
-- 분모는 절마다 한 줄로 적었다. ①~⑥은 0이어야 하는 값, ⑦은 ≥ 80%, ⑧은 보고용.
\if :{?from}
\else
  \echo '사용법: psql -v from="\'2026-10-06 15:00+08\'" -f scripts/job_driven_check.sql  (from = 배포 경계)'
  \quit
\endif
BEGIN;
SET LOCAL statement_timeout = '120s';

\echo '① 내보내는 짝(1계층)에 지금 일하는 트럭 — 분모: match_ver=1 의 1계층 추천 행. 0이어야 한다.'
SELECT count(*) AS "1계층 행", count(*) FILTER (WHERE veh_state NOT IN ('free_tos','free_gps')) AS "일하는 트럭 배정"
  FROM stage2_match_shadow WHERE ts >= :from::timestamptz AND match_ver = 1 AND match_tier = 1;

\echo '② 내보내는 짝 중 마감이 안 온 작업 — 분모: 같은 1계층 행. ⚠구조상 0(1계층은 마감 ≤ 틱+300초로만 뽑힌다) — 배선 확인용이지 반증 수단이 아니다.'
SELECT count(*) FILTER (WHERE dispatch_deadline_ts > ts + interval '300 seconds') AS "마감 안 온 작업을 내보냄"
  FROM stage2_match_shadow WHERE ts >= :from::timestamptz AND match_ver = 1 AND match_tier = 1;

\echo '③ 재지향 — 분모: match_ver=1 의 모든 추천 행. 0이어야 한다.'
SELECT count(*) FILTER (WHERE redirected_from IS NOT NULL OR veh_state = 'redirectable') AS "재지향"
  FROM stage2_match_shadow WHERE ts >= :from::timestamptz AND match_ver = 1;

\echo '④ 덮개 감소·순서 위반 — 분모: match_ver=1 의 매칭 틱. ★덮개 감소 틱이 진짜 계기(0이어야 한다).'
\echo '   순서 위반은 우선 덮개를 자기 자신과 대조하므로 구조상 0 — 배선 확인용. 도달 불가 슬롯은 위반이 아니라 따로 센다.'
SELECT count(*) AS "틱", sum(t1_skip_n) AS "순서 위반 합(구조상 0)",
       count(*) FILTER (WHERE t1_cov <> t1_cov_alone) AS "함께 풀어 덮개가 줄어든 틱",
       sum(t1_unreach_n) AS "갈 트럭 없던 급한 슬롯", round(avg(n_soon_drop), 1) AS "내릴 자리 아는 곧빌/틱",
       sum(t1_cov_alone) AS "1계층 덮개 합", sum(n_free) AS "지금 빈 트럭 합"
  FROM stage2_solver_shadow WHERE ts >= :from::timestamptz AND match_ver = 1;

\echo '⑤ 스왑 억제 조건 — 분모: 스왑 추천 행. 이득<180초·목적지 500m 안 은 0이어야 한다.'
SELECT count(*) AS "스왑", count(*) FILTER (WHERE gain_s < 180) AS "이득 미달",
       count(*) FILTER (WHERE a_dist_m < 500 OR b_dist_m < 500) AS "500m 안",
       count(*) FILTER (WHERE ytno_a = ytno_b) AS "자기 자신"
  FROM stage2_swap_shadow WHERE ts >= :from::timestamptz;

\echo '⑥ 한 배차에 스왑 2회 이상 — 분모: 스왑에 든 (트럭, 스왑) 쌍. 같은 트럭의 연속 두 스왑 사이에 자유(내림)가 없으면 위반. 0이어야 한다.'
WITH s AS (
  SELECT ytno_a AS ytno, ts FROM stage2_swap_shadow WHERE ts >= :from::timestamptz
  UNION ALL SELECT ytno_b, ts FROM stage2_swap_shadow WHERE ts >= :from::timestamptz
), o AS (
  SELECT ytno, ts, lag(ts) OVER (PARTITION BY ytno ORDER BY ts) AS prev_ts FROM s
)
SELECT count(*) AS "(트럭,스왑) 쌍",
       count(*) FILTER (WHERE prev_ts IS NOT NULL AND NOT EXISTS (
         SELECT 1 FROM qc_move_log q WHERE q.trk_id = o.ytno AND q.jobtype = 'LD' AND q.comp_ts > o.prev_ts AND q.comp_ts < o.ts
         UNION ALL
         SELECT 1 FROM tos_handover_label h WHERE h.ytno = o.ytno AND h.jobtype = 'DS' AND h.comp_ts > o.prev_ts AND h.comp_ts < o.ts
       )) AS "자유 없이 다시 스왑"
  FROM o;
\echo '   (보조) 스왑 트럭이 같은 순간 후보 풀에 있었나 — 있으면 배차됨·픽업 전 판정이 샌 것. 0이어야 한다.'
SELECT count(*) AS "풀에도 있던 스왑 트럭"
  FROM stage2_swap_shadow s
 WHERE s.ts >= :from::timestamptz
   AND EXISTS (SELECT 1 FROM stage2_pool_truck_shadow p
                WHERE p.ytno IN (s.ytno_a, s.ytno_b) AND p.ts BETWEEN s.ts - interval '20 seconds' AND s.ts);

\echo '⑦ 계획에 넣은 곧 빌 트럭이 실제로 그 작업 배차 마감 안에 짐을 내렸나 — 분모: match_ver=1·2계층·곧 빌 트럭(veh_state 가 free_* 아님)·상자 단위 행 중 마감+30분이 지난 것. ≥ 80% 이어야 한다.'
WITH r AS (
  SELECT m.ts, m.ytno, m.veh_jobtype, m.dispatch_deadline_ts AS dd, m.free_q80_s
    FROM stage2_match_shadow m
   WHERE m.ts >= :from::timestamptz AND m.match_ver = 1 AND m.match_tier = 2
     AND m.veh_state NOT IN ('free_tos','free_gps') AND m.contno IS NOT NULL
     AND m.dispatch_deadline_ts < now() - interval '30 minutes'
), d AS MATERIALIZED (
  SELECT r.*, CASE r.veh_jobtype
     WHEN 'LD' THEN (SELECT min(q.comp_ts) FROM qc_move_log q WHERE q.trk_id = r.ytno AND q.jobtype = 'LD' AND q.comp_ts > r.ts AND q.comp_ts <= r.dd + interval '30 minutes')
     WHEN 'DS' THEN (SELECT min(h.comp_ts) FROM tos_handover_label h WHERE h.ytno = r.ytno AND h.jobtype = 'DS' AND h.comp_ts > r.ts AND h.comp_ts <= r.dd + interval '30 minutes')
   END AS drop_ts FROM r
)
SELECT veh_jobtype AS "트럭 작업", count(*) AS "계획 행", count(DISTINCT ytno) AS "트럭",
       round(100.0 * count(*) FILTER (WHERE drop_ts <= dd) / count(*), 1) AS "마감 안에 내림 %",
       round(100.0 * count(*) FILTER (WHERE drop_ts IS NULL) / count(*), 1) AS "마감+30분에도 안 내림 %",
       percentile_cont(0.5) WITHIN GROUP (ORDER BY free_q80_s) AS "자격 시각 중앙(초)"
  FROM d GROUP BY ROLLUP (1) ORDER BY 1;

\echo '⑧ 효과(보고용) — 분모: match_ver=1 의 매칭 틱. 함께 풀기(joint) vs 따로 풀기(seq), 스왑.'
SELECT count(*) AS "틱",
       round(avg(n_free), 1) AS "빈 트럭/틱", round(avg(n_soon_ok), 1) AS "자격 곧빌/틱",
       sum(seq_t1_cost_s) / 60 AS "따로 1계층 주행(분)", sum(joint_t1_cost_s) / 60 AS "함께 1계층 주행(분)",
       sum(seq_t2_n) AS "따로 계획 수", sum(joint_t2_n) AS "함께 계획 수",
       sum(seq_t2_cost_s) / 60 AS "따로 계획 주행(분)", sum(joint_t2_cost_s) / 60 AS "함께 계획 주행(분)",
       sum(swap_n) AS "스왑 수", sum(swap_gain_s) / 60 AS "스왑으로 아낀 주행(분)",
       round(sum(swap_n)::numeric / NULLIF(extract(epoch FROM max(ts) - min(ts)) / 3600, 0), 1) AS "스왑/시간",
       round(avg(swap_cand_n), 1) AS "스왑 후보/틱"
  FROM stage2_solver_shadow WHERE ts >= :from::timestamptz AND match_ver = 1;
\echo '   한 트럭의 최대 스왑 횟수(창 전체):'
SELECT max(c) AS "트럭당 최대 스왑" FROM (
  SELECT ytno, count(*) c FROM (SELECT ytno_a ytno FROM stage2_swap_shadow WHERE ts >= :from::timestamptz
                                UNION ALL SELECT ytno_b FROM stage2_swap_shadow WHERE ts >= :from::timestamptz) u GROUP BY 1) x;
ROLLBACK;
