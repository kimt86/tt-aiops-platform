-- 차량 요청 시점 커버리지 — "트럭이 배차를 요청한 그 순간, 우리에게 그 트럭에 줄 작업이 있었나".
--   psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/pull_model_coverage.sql
--
-- ■ 왜 이 형태인가 (2026-08-19 사용자 지정 · 이전 판정을 대체한다)
-- 현장 구조는 **작업이 차량을 부르는 것이 아니라 차량이 작업을 고르는 것**이다. 트럭이 비면 TOS 에
-- 배차를 요청하고, TOS 는 그 순간 그 트럭에 작업을 준다. 실측으로도 그렇다: 직전 작업에서 자유로워진
-- 뒤 다음 배차까지 중앙 **양하 15초 / 적하 38초**(3일·twin 첫 레그·52~58%가 60초 안).
--
-- ⇒ 그래서 **"우리가 TOS 보다 몇 분 이른가"는 의미 없는 비교다**(TOS 배차시각은 결정 시점이 아니라
--   트럭이 비는 시점일 뿐이다). 합격 기준은 하나다 — **트럭이 물어본 그 순간 우리에게 답이 있었나.**
--   답이 없으면 pull 구조에서는 그 트럭이 그냥 서 있는다.
--
-- ■ 판정
--   ① 커버리지 = 직전 틱(≤150초)에 그 트럭(ytno)에 대한 우리 추천 행이 있었나.
--   ② 없었다면 왜 — 그 순간 그 트럭이 우리 후보풀 자격 상태였나(idle/soon_idle/wait_rtg)인지,
--      우리가 '작업 중'으로 보고 있었는지. 앞은 **일부러 남긴 것**, 뒤는 **못 본 것**이라 처방이 다르다.
--   ③ (부수) 답이 있었다면 같은 상자였나. pull 구조에서 상자 일치는 합격 기준이 아니다 — 우리는
--      그 트럭에 다른(더 나은) 작업을 줬을 수 있다.
--
-- ■ 분모
-- ①③⑤ = "최근 7일 TOS 배차 사건"(= 트럭이 요청해 작업을 받은 순간), twin 은 첫 레그만 세어 **왕복 1건**.
-- ② = 그 중 트럭 상태 이력이 남아 있는 구간만(`truck_pos_hist` 는 2일 보관) — 분모를 따로 적는다.

\set ON_ERROR_STOP on
\pset null '-'

BEGIN;
SET LOCAL statement_timeout = '60s';

-- 우리 추천 (원표에 (ytno,ts) 인덱스가 없다 — ts 로 잘라 임시표에 다시 색인한다)
CREATE TEMP TABLE rec AS
SELECT ts, ytno, contno, jobtype, veh_state
  FROM stage2_match_shadow
 WHERE ts > now() - interval '7 days' AND ytno IS NOT NULL;
CREATE INDEX ON rec (ytno, ts);
ANALYZE rec;

-- 우리 틱 (추천이 0건인 틱도 포함해야 "틱이 없어서 못 준 것"과 구분된다)
CREATE TEMP TABLE tick AS
SELECT ts FROM stage2_solver_shadow WHERE ts > now() - interval '7 days';
CREATE INDEX ON tick (ts);
ANALYZE tick;

-- 트럭이 요청해 작업을 받은 순간
CREATE TEMP TABLE ev AS
SELECT ytno, contno, jobtype, dispatch_ts, twin_leg_seq, twin_group_size
  FROM tt_move_log
 WHERE dispatch_ts > now() - interval '7 days' AND jobtype IN ('DS','LD');
CREATE INDEX ON ev (dispatch_ts);
ANALYZE ev;

CREATE TEMP TABLE m AS
SELECT e.*,
       t.ts                                   AS tick_ts,
       EXTRACT(epoch FROM e.dispatch_ts - t.ts)::int AS tick_age_s,
       r.ts                                   AS rec_ts,
       r.contno                               AS rec_contno,
       r.veh_state                            AS rec_veh_state,
       s.state                                AS truck_state
  FROM ev e
  LEFT JOIN LATERAL (SELECT ts FROM tick WHERE tick.ts <= e.dispatch_ts
                      ORDER BY ts DESC LIMIT 1) t ON true
  LEFT JOIN LATERAL (SELECT ts, contno, veh_state FROM rec
                      WHERE rec.ytno = e.ytno AND rec.ts <= e.dispatch_ts
                        AND rec.ts > e.dispatch_ts - interval '150 seconds'
                      ORDER BY rec.ts DESC LIMIT 1) r ON true
  LEFT JOIN LATERAL (SELECT state FROM truck_pos_hist p
                      WHERE p.ytno = e.ytno AND p.ts <= e.dispatch_ts
                        AND p.ts > e.dispatch_ts - interval '5 minutes'
                      ORDER BY p.ts DESC LIMIT 1) s ON true;
CREATE INDEX ON m (jobtype);
ANALYZE m;

\echo ''
\echo '════ ⓪ 분모 — 최근 7일 트럭이 요청해 작업을 받은 순간 ════'
SELECT jobtype AS 작업,
       count(*)                                          AS "상자(레그)",
       count(*) FILTER (WHERE twin_leg_seq = 1)          AS "★분모(왕복 1건)",
       count(*) FILTER (WHERE twin_group_size > 1 AND twin_leg_seq = 1) AS "그중 트윈",
       count(*) FILTER (WHERE twin_leg_seq = 1 AND tick_ts IS NULL) AS "직전 우리 틱 없음",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY tick_age_s)::numeric,0) AS "직전 틱 나이 중앙(초)",
       round(percentile_cont(0.99) WITHIN GROUP (ORDER BY tick_age_s)::numeric,0) AS "p99(초)",
       min(dispatch_ts)::timestamp(0) AS 시작, max(dispatch_ts)::timestamp(0) AS 끝
  FROM m GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ① ★헤드라인 — 트럭이 물어본 순간 우리에게 그 트럭에 줄 작업이 있었나 (분모=⓪★) ════'
SELECT jobtype AS 작업, count(*) AS 요청,
       count(rec_ts)                                                  AS "우리도 배차돼 있었음",
       round(100.0*count(rec_ts)/count(*),1)                          AS "★커버리지 %",
       count(*) - count(rec_ts)                                       AS "우리는 줄 게 없었음",
       round(100.0*count(*) FILTER (WHERE rec_ts IS NOT NULL AND rec_contno = contno)/count(*),1) AS "같은 상자 %(부수)"
  FROM m WHERE twin_leg_seq = 1 GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ② 답이 없었던 경우의 분해 — 그 순간 우리가 그 트럭을 어떤 상태로 보고 있었나 ════'
\echo '    후보 자격(idle/soon_idle/wait_rtg) = 볼 수는 있었는데 안 준 것(일부러 남김) · 그 외 = 작업 중으로 봄'
\echo '    분모는 상태 이력이 남아 있는 구간뿐(truck_pos_hist 2일 보관)'
SELECT jobtype AS 작업,
       coalesce(truck_state,'(이력 없음)') AS "그 순간 우리가 본 트럭 상태",
       count(*) AS 건,
       round(100.0*count(*)/sum(count(*)) OVER (PARTITION BY jobtype),1) AS "비중 %"
  FROM m
 WHERE twin_leg_seq = 1 AND rec_ts IS NULL AND dispatch_ts > now() - interval '2 days'
 GROUP BY 1,2 ORDER BY 1,3 DESC;

\echo ''
\echo '════ ②b 같은 창에서의 커버리지 (②와 같은 분모로 비교하기 위해) ════'
SELECT jobtype AS 작업, count(*) AS 요청, round(100.0*count(rec_ts)/count(*),1) AS "커버리지 %",
       round(100.0*count(*) FILTER (WHERE rec_ts IS NULL AND truck_state IN ('idle','soon_idle','wait_rtg'))/count(*),1) AS "후보였는데 안 준 %",
       round(100.0*count(*) FILTER (WHERE rec_ts IS NULL AND truck_state IS NOT NULL AND truck_state NOT IN ('idle','soon_idle','wait_rtg'))/count(*),1) AS "작업중으로 본 %",
       round(100.0*count(*) FILTER (WHERE rec_ts IS NULL AND truck_state IS NULL)/count(*),1) AS "상태 이력 없음 %"
  FROM m WHERE twin_leg_seq = 1 AND dispatch_ts > now() - interval '2 days' GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ③ 우리가 그 트럭을 잡아둔 적은 있나 — 요청 시점 앞뒤로 창을 넓혀본 커버리지 (분모=⓪★) ════'
\echo '    150초(①) → 10분 → 30분. 창을 넓혀도 안 오르면 그 트럭은 우리 출력에 아예 없는 것이다.'
SELECT e.jobtype AS 작업, count(*) AS 요청,
       round(100.0*count(*) FILTER (WHERE EXISTS (SELECT 1 FROM rec r WHERE r.ytno=e.ytno AND r.ts <= e.dispatch_ts AND r.ts > e.dispatch_ts - interval '150 seconds'))/count(*),1) AS "150초 %",
       round(100.0*count(*) FILTER (WHERE EXISTS (SELECT 1 FROM rec r WHERE r.ytno=e.ytno AND r.ts <= e.dispatch_ts AND r.ts > e.dispatch_ts - interval '10 minutes'))/count(*),1)  AS "10분 %",
       round(100.0*count(*) FILTER (WHERE EXISTS (SELECT 1 FROM rec r WHERE r.ytno=e.ytno AND r.ts <= e.dispatch_ts AND r.ts > e.dispatch_ts - interval '30 minutes'))/count(*),1)  AS "30분 %"
  FROM ev e WHERE e.twin_leg_seq = 1 AND e.dispatch_ts > now() - interval '2 days' GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ④ 우리 추천은 어떤 트럭에 나가고 있나 — 추천 시점의 트럭 상태 (분모=최근 2일 추천 행) ════'
\echo '    idle 만이 "지금 당장 보낼 수 있는" 트럭이다. soon_* 은 아직 안 빈 트럭을 미리 잡은 것.'
SELECT veh_state AS "추천 시점 트럭 상태", count(*) AS 행,
       round(100.0*count(*)/sum(count(*)) OVER (),1) AS "비중 %"
  FROM rec WHERE ts > now() - interval '2 days' GROUP BY 1 ORDER BY 2 DESC;

\echo ''
\echo '════ ⑤ 시간대별 커버리지 — 바쁠 때 떨어지는가 (분모=⓪★·MYT 시각) ════'
SELECT EXTRACT(hour FROM dispatch_ts AT TIME ZONE 'Asia/Kuala_Lumpur')::int AS "시(MYT)",
       count(*) AS 요청, round(100.0*count(rec_ts)/count(*),1) AS "커버리지 %"
  FROM m WHERE twin_leg_seq = 1 GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ⑥ 상한이 원인인가 — 요청하는 트럭 수 vs 우리가 지목하는 트럭 수 (최근 2일) ════'
WITH ask AS (
  SELECT date_trunc('minute',dispatch_ts) AS mi, count(DISTINCT ytno) AS n
    FROM tt_move_log WHERE dispatch_ts > now()-interval '2 days' AND jobtype IN ('DS','LD') GROUP BY 1
), rc AS (SELECT ts, count(DISTINCT ytno) AS n FROM rec WHERE ts > now()-interval '2 days' GROUP BY 1)
SELECT '요청 트럭/분' AS 항목, round(avg(n),1) AS 평균, percentile_cont(0.5) WITHIN GROUP (ORDER BY n) AS 중앙, max(n) AS 최대 FROM ask
UNION ALL SELECT '우리가 지목/틱', round(avg(n),1), percentile_cont(0.5) WITHIN GROUP (ORDER BY n), max(n) FROM rc;

\echo ''
\echo '════ ⑦ 함대 사각지대인가 — 요청한 트럭이 우리 출력에 아예 없는가 (최근 2일) ════'
WITH a AS (SELECT DISTINCT ytno FROM tt_move_log WHERE dispatch_ts > now()-interval '2 days' AND jobtype IN ('DS','LD')),
     r AS (SELECT DISTINCT ytno FROM rec WHERE ts > now()-interval '2 days')
SELECT (SELECT count(*) FROM a) AS "요청한 트럭 대수", (SELECT count(*) FROM r) AS "우리 출력에 나온 대수",
       (SELECT count(*) FROM a WHERE NOT EXISTS (SELECT 1 FROM r WHERE r.ytno=a.ytno)) AS "요청했는데 우리 출력엔 전혀 없음";

\echo ''
\echo '════ ⑧ 틱 주기가 원인인가 — 트럭이 우리 직전 틱 이후에 비었나 (최근 2일) ════'
\echo '    "틱 이후 빔" = 우리가 그 틱에서는 구조적으로 낼 수 없었던 건(60초 주기 vs 빈 뒤 15~38초 만에 요청).'
WITH e2 AS (
  SELECT ytno, jobtype, dispatch_ts, lag(free_ts) OVER (PARTITION BY ytno ORDER BY dispatch_ts) AS prev_free
    FROM tt_move_log WHERE dispatch_ts > now()-interval '2 days' AND twin_leg_seq=1
), x AS (
  SELECT e2.*, tk.ts AS tick_ts,
         EXISTS (SELECT 1 FROM rec r WHERE r.ytno=e2.ytno AND r.ts<=e2.dispatch_ts AND r.ts>e2.dispatch_ts-interval '150 seconds') AS covered
    FROM e2 LEFT JOIN LATERAL (SELECT ts FROM tick WHERE tick.ts<=e2.dispatch_ts ORDER BY ts DESC LIMIT 1) tk ON true
   WHERE e2.prev_free IS NOT NULL
)
SELECT jobtype AS 작업, count(*) AS 요청,
  round(100.0*count(*) FILTER (WHERE covered)/count(*),1) AS "커버 %",
  round(100.0*count(*) FILTER (WHERE prev_free > tick_ts AND NOT covered)/nullif(count(*) FILTER (WHERE NOT covered),0),1) AS "놓친 건 중 '틱 이후 빔' %",
  round(100.0*count(*) FILTER (WHERE prev_free <= tick_ts AND NOT covered)/nullif(count(*) FILTER (WHERE NOT covered),0),1) AS "놓친 건 중 '틱 전에 이미 비었음' %",
  round(percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM dispatch_ts-prev_free)) FILTER (WHERE prev_free <= tick_ts AND NOT covered)::numeric,0) AS "후자의 빈 뒤 요청까지 중앙(초)"
 FROM x GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ⑨ GPS 침묵이 원인인가 — 요청 직전 마지막 위치 나이별 커버리지 (최근 2일) ════'
\echo '    트럭은 정지하면 GPS 단말이 침묵한다(기존 확인). 그 트럭이 곧 요청하는 트럭이다.'
WITH e3 AS (
  SELECT ytno, dispatch_ts FROM tt_move_log
   WHERE dispatch_ts > now()-interval '2 days' AND jobtype IN ('DS','LD') AND twin_leg_seq=1
), x AS (
  SELECT e3.*, EXTRACT(epoch FROM e3.dispatch_ts - p.ts)::int AS gps_age_s,
         EXISTS (SELECT 1 FROM rec r WHERE r.ytno=e3.ytno AND r.ts<=e3.dispatch_ts AND r.ts>e3.dispatch_ts-interval '150 seconds') AS covered
    FROM e3 LEFT JOIN LATERAL (SELECT ts FROM truck_pos_hist p WHERE p.ytno=e3.ytno AND p.ts<=e3.dispatch_ts ORDER BY p.ts DESC LIMIT 1) p ON true
)
SELECT CASE WHEN gps_age_s IS NULL THEN 'e) GPS 없음' WHEN gps_age_s<=60 THEN 'a) 60초 이내'
            WHEN gps_age_s<=300 THEN 'b) 1~5분' WHEN gps_age_s<=1800 THEN 'c) 5~30분' ELSE 'd) 30분+' END AS "요청 직전 GPS 나이",
       count(*) AS 요청, round(100.0*count(*) FILTER (WHERE covered)/count(*),1) AS "커버리지 %"
  FROM x GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ⑩ ★놓친 차량은 후보 풀에 있었나, 없었나 (최근 2일·왕복 1건) ════'
\echo '    풀 정의(livemap.rs:4729~): GPS 신선(≤120초) + classify_tt 상태가 idle/soon_idle/wait_rtg 면 후보.'
\echo '    delivering/empty_travel/staging 는 코드에서 continue = 후보 제외. GPS 가 낡으면 아예 분류가 안 된다.'
\echo '    truck_pos_hist.state 는 매처와 같은 classify_tt 출력이고 GPS 신선할 때만 기록되므로 풀 자격을 그대로 재현한다.'
\echo '    (⚠ 침묵 트럭을 붙잡는 가지(soon_idle_held/anchored)는 "짐을 싣고 드랍 지점 120m 안"일 때만 걸리고,'
\echo '     그 상태는 어디에도 기록되지 않는다 — 아래 C 의 일부는 그 가지로 풀에 있었을 수 있다.)'
WITH e AS (
  SELECT ytno, jobtype, dispatch_ts FROM tt_move_log
   WHERE dispatch_ts > now()-interval '2 days' AND jobtype IN ('DS','LD') AND twin_leg_seq=1
), x AS (
  SELECT e.*,
         EXISTS (SELECT 1 FROM rec r WHERE r.ytno=e.ytno AND r.ts<=e.dispatch_ts AND r.ts>e.dispatch_ts-interval '150 seconds') AS covered,
         s.state
    FROM e LEFT JOIN LATERAL (
      SELECT state FROM truck_pos_hist p
       WHERE p.ytno=e.ytno AND p.ts<=e.dispatch_ts AND p.ts>e.dispatch_ts-interval '150 seconds'
       ORDER BY p.ts DESC LIMIT 1) s ON true
), c AS (
  SELECT jobtype, covered,
         CASE WHEN state IN ('idle','soon_idle','wait_rtg') THEN 'A. 풀에 있었음'
              WHEN state IS NOT NULL                        THEN 'B. 풀에서 제외(작업 중으로 분류)'
              ELSE                                               'C. 아예 안 보임(GPS 침묵)' END AS bucket,
         state
    FROM x
)
SELECT jobtype AS 작업, bucket AS 구분,
       count(*) FILTER (WHERE NOT covered) AS "놓친 건",
       round(100.0*count(*) FILTER (WHERE NOT covered)/sum(count(*) FILTER (WHERE NOT covered)) OVER (PARTITION BY jobtype),1) AS "놓친 건 중 %",
       count(*) FILTER (WHERE covered) AS "커버된 건",
       round(100.0*count(*) FILTER (WHERE covered)/nullif(count(*),0),1) AS "이 구분의 커버리지 %"
  FROM c GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '════ ⑪ 위 A(풀에 있었는데 못 준 건) 의 상태 내역 · C(안 보임) 의 침묵 길이 ════'
WITH e AS (
  SELECT ytno, jobtype, dispatch_ts FROM tt_move_log
   WHERE dispatch_ts > now()-interval '2 days' AND jobtype IN ('DS','LD') AND twin_leg_seq=1
), x AS (
  SELECT e.*,
         EXISTS (SELECT 1 FROM rec r WHERE r.ytno=e.ytno AND r.ts<=e.dispatch_ts AND r.ts>e.dispatch_ts-interval '150 seconds') AS covered,
         s.state, EXTRACT(epoch FROM e.dispatch_ts - p.ts)::int AS silence_s
    FROM e
    LEFT JOIN LATERAL (SELECT state FROM truck_pos_hist p WHERE p.ytno=e.ytno AND p.ts<=e.dispatch_ts AND p.ts>e.dispatch_ts-interval '150 seconds' ORDER BY p.ts DESC LIMIT 1) s ON true
    LEFT JOIN LATERAL (SELECT ts FROM truck_pos_hist p WHERE p.ytno=e.ytno AND p.ts<=e.dispatch_ts ORDER BY p.ts DESC LIMIT 1) p ON true
)
SELECT coalesce(state,'(안 보임)') AS "요청 순간 상태", count(*) FILTER (WHERE NOT covered) AS "놓친 건",
       round(100.0*count(*) FILTER (WHERE covered)/count(*),1) AS "커버리지 %",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY silence_s) FILTER (WHERE NOT covered)::numeric,0) AS "놓친 건 침묵 중앙(초)"
  FROM x GROUP BY 1 ORDER BY 2 DESC;

ROLLBACK;
