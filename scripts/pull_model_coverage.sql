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

\echo ''
\echo '════ ⑫ ★풀 재현율 (mig 0154 · pool_ver 1 · 2026-08-19~) — 트럭이 요청한 순간, 직전 틱(≤150초) 우리 후보 풀에 있었나 ════'
\echo '    분모 = 풀 기록이 시작된 뒤의 요청(DS/LD 왕복 1건). 이번 사이클(pull 1/2)의 합격 기준 ≥ 95%. 슬롯은 안 바꿨으므로 커버리지(①)와 다르다.'
CREATE TEMP TABLE pool AS
SELECT ts, ytno, reason, free_in_s, pos_src FROM stage2_pool_truck_shadow
 WHERE ts > now() - interval '7 days' AND pool_ver = (SELECT max(pool_ver) FROM stage2_pool_truck_shadow);  -- 최신 판만
CREATE INDEX ON pool (ytno, ts); CREATE INDEX ON pool (ts); ANALYZE pool;
WITH win AS (SELECT min(ts) AS t0, max(ts) AS t1 FROM pool),
e AS (
  SELECT e.ytno, e.jobtype, e.dispatch_ts FROM ev e, win
   WHERE e.twin_leg_seq = 1 AND e.dispatch_ts > win.t0 + interval '150 seconds' AND e.dispatch_ts <= win.t1
), x AS (
  SELECT e.*, p.reason, p.free_in_s, p.pos_src
    FROM e LEFT JOIN LATERAL (SELECT reason, free_in_s, pos_src FROM pool
                               WHERE pool.ytno = e.ytno AND pool.ts <= e.dispatch_ts AND pool.ts > e.dispatch_ts - interval '150 seconds'
                               ORDER BY pool.ts DESC LIMIT 1) p ON true
)
SELECT jobtype AS 작업, count(*) AS 요청,
       round(100.0*count(reason)/count(*),1) AS "★풀 재현율 %",
       round(100.0*count(*) FILTER (WHERE reason='free_tos')/count(*),1) AS "free_tos %",
       round(100.0*count(*) FILTER (WHERE reason LIKE 'inflight%')/count(*),1) AS "inflight %",
       round(100.0*count(*) FILTER (WHERE reason='gps_free')/count(*),1) AS "gps_free %",
       round(100.0*count(*) FILTER (WHERE reason IS NULL)/count(*),1) AS "풀 밖 %",
       (SELECT to_char(t0 AT TIME ZONE 'Asia/Kuala_Lumpur','MM-DD HH24:MI') FROM win) AS "창 시작(MYT)",
       (SELECT to_char(t1 AT TIME ZONE 'Asia/Kuala_Lumpur','MM-DD HH24:MI') FROM win) AS "창 끝"
  FROM x GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ⑬ 풀 크기 분포 (틱당 트럭 수 · 사유별) — 합격 기준 중앙 ≤ 300 ════'
WITH t AS (
  SELECT ts, count(*) AS n, count(*) FILTER (WHERE reason='free_tos') AS n_free, count(*) FILTER (WHERE reason LIKE 'inflight%') AS n_inf,
         count(*) FILTER (WHERE reason='gps_free') AS n_gps, count(*) FILTER (WHERE pos_src='pos_hist') AS n_hist
    FROM pool GROUP BY ts)
SELECT count(*) AS 틱, round(percentile_cont(0.5) WITHIN GROUP (ORDER BY n)::numeric,0) AS "풀 중앙", max(n) AS 최대,
       round(avg(n_free),0) AS "free_tos 평균", round(avg(n_inf),0) AS "inflight 평균", round(avg(n_gps),0) AS "gps_free 평균",
       round(avg(n_hist),0) AS "위치=pos_hist 평균"
  FROM t;

\echo ''
\echo '════ ⑭ 풀 밖에서 요청한 트럭 — 그 순간 TOS 로는 무엇이었나 (자유 뒤 얼마 / 배차 중) ════'
WITH win AS (SELECT min(ts) AS t0, max(ts) AS t1 FROM pool),
e2 AS (
  SELECT ytno, dispatch_ts, jobtype, lag(free_ts) OVER (PARTITION BY ytno ORDER BY dispatch_ts) AS prev_free
    FROM tt_move_log WHERE dispatch_ts > now()-interval '8 days' AND twin_leg_seq=1
), m2 AS (
  SELECT e2.*, EXTRACT(epoch FROM dispatch_ts - prev_free) AS free_for
    FROM e2, win WHERE e2.jobtype IN ('DS','LD') AND e2.dispatch_ts > win.t0 + interval '150 seconds' AND e2.dispatch_ts <= win.t1
     AND NOT EXISTS (SELECT 1 FROM pool p WHERE p.ytno=e2.ytno AND p.ts <= e2.dispatch_ts AND p.ts > e2.dispatch_ts - interval '150 seconds')
)
SELECT CASE WHEN prev_free IS NULL THEN 'a) 8일 안 이전 자유 없음(신규)'
            WHEN free_for <= 60 THEN 'b) 빈 지 ≤60초(직전 틱엔 작업 중 — 예측이 못 잡음)'
            WHEN free_for <= 180 THEN 'c) 빈 지 1~3분(원천 신호 지연 구간)'
            WHEN free_for <= 10800 THEN 'd) 빈 지 3분~3h(신호 있었는데 못 넣음 — 위치 없음?)'
            ELSE 'e) 빈 지 3h+(명단 밖)' END AS "풀 밖 이유",
       count(*) AS 건, round(100.0*count(*)/sum(count(*)) OVER (),1) AS "%"
  FROM m2 GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ⑮ ★★풀 재현율 — 요청 순간을 "자유 뒤 배차 목록에 처음 실린 틱"으로 (mig 0155 · 2026-08-19 14:42~) ════'
\echo '    tt_move_log.dispatch_ts 는 최종 배차만 남긴다(재배정되면 첫 배차가 사라짐 — TT1272 실증). pull 에서 트럭이 물어본 순간은'
\echo '    자유(원천 드랍 로그) 뒤 live_assigned_tt 에 처음 나타난 스냅샷이다. 분모 = 그런 (자유→첫 등재) 사건. 재현율 = 그 등재 틱 직전(≤150초)에 풀에 있었나.'
-- ★판별자: 풀 규칙 판(pool_ver)이 바뀌면 모집단이 바뀐다. 최신 판의 첫 틱 이후만 잰다.
CREATE TEMP TABLE pv AS SELECT max(pool_ver) AS ver, min(ts) FILTER (WHERE pool_ver = (SELECT max(pool_ver) FROM stage2_pool_truck_shadow)) AS t_from FROM stage2_pool_truck_shadow;
CREATE TEMP TABLE ah AS
SELECT as_of_ts, ytno, jobstatus,
       lag(as_of_ts) OVER (PARTITION BY ytno ORDER BY as_of_ts) AS prev_as_of   -- 같은 트럭의 직전 등재 스냅샷
  FROM assigned_tt_hist WHERE as_of_ts > (SELECT t_from FROM pv);
CREATE INDEX ON ah (ytno, as_of_ts); ANALYZE ah;
CREATE TEMP TABLE fr AS
SELECT ytno, f FROM (
  SELECT trk_id ytno, comp_ts f FROM qc_move_log WHERE jobtype='LD' AND comp_ts > (SELECT min(as_of_ts) FROM ah) - interval '1 hour' AND trk_id IS NOT NULL
  UNION ALL SELECT ytno, comp_ts FROM tos_handover_label WHERE jobtype='DS' AND comp_ts > (SELECT min(as_of_ts) FROM ah) - interval '1 hour' AND ytno IS NOT NULL) u;
CREATE INDEX ON fr (ytno, f); ANALYZE fr;
WITH win AS (SELECT min(as_of_ts) t0, max(as_of_ts) t1 FROM ah),
ev2 AS (
  -- 자유 사건마다: 그 뒤 처음 등재된 스냅샷 = 자유 뒤 첫 등재이면서 그 직전 등재가 자유 이전(또는 없음). 인덱스 LATERAL.
  SELECT f.ytno, f.f AS free_ts, a.as_of_ts AS ask_ts
    FROM fr f, win
    LEFT JOIN LATERAL (SELECT as_of_ts FROM ah WHERE ah.ytno=f.ytno AND ah.as_of_ts > f.f AND (ah.prev_as_of IS NULL OR ah.prev_as_of < f.f)
                        ORDER BY as_of_ts LIMIT 1) a ON true
   WHERE f.f > win.t0 AND f.f < win.t1 - interval '2 minutes'
), ev3 AS (
  SELECT e.*, p.reason FROM ev2 e
    LEFT JOIN LATERAL (SELECT reason FROM pool WHERE pool.ytno=e.ytno AND pool.ts <= e.ask_ts AND pool.ts > e.ask_ts - interval '150 seconds' ORDER BY pool.ts DESC LIMIT 1) p ON e.ask_ts IS NOT NULL
)
SELECT count(*) AS "자유 사건", count(ask_ts) AS "그 뒤 등재됨(=물어봄)",
       round(100.0*count(reason)/nullif(count(ask_ts),0),1) AS "★풀 재현율 %",
       round(100.0*count(*) FILTER (WHERE reason='free_tos')/nullif(count(ask_ts),0),1) AS "free_tos %",
       round(100.0*count(*) FILTER (WHERE reason LIKE 'inflight%')/nullif(count(ask_ts),0),1) AS "inflight %",
       round(100.0*count(*) FILTER (WHERE reason='gps_free')/nullif(count(ask_ts),0),1) AS "gps_free %",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM ask_ts - free_ts))::numeric,0) AS "자유→등재 중앙(초)",
       (SELECT ver FROM pv) AS pool_ver,
       (SELECT to_char(t0 AT TIME ZONE 'Asia/Kuala_Lumpur','MM-DD HH24:MI') FROM win) AS "창 시작(MYT)",
       (SELECT to_char(t1 AT TIME ZONE 'Asia/Kuala_Lumpur','MM-DD HH24:MI') FROM win) AS "창 끝"
  FROM ev3;

\echo ''
\echo '════ ⑯ ⑮에서 놓친 것 — 등재 직전 틱에서 그 트럭을 우리는 어떻게 보고 있었나 ════'
WITH win AS (SELECT min(as_of_ts) t0, max(as_of_ts) t1 FROM ah),
ev2 AS (
  SELECT f.ytno, f.f AS free_ts, a.as_of_ts AS ask_ts
    FROM fr f, win
    LEFT JOIN LATERAL (SELECT as_of_ts FROM ah WHERE ah.ytno=f.ytno AND ah.as_of_ts > f.f AND (ah.prev_as_of IS NULL OR ah.prev_as_of < f.f)
                        ORDER BY as_of_ts LIMIT 1) a ON true
   WHERE f.f > win.t0 AND f.f < win.t1 - interval '2 minutes'
), miss AS (
  SELECT e.* FROM ev2 e WHERE e.ask_ts IS NOT NULL
     AND NOT EXISTS (SELECT 1 FROM pool WHERE pool.ytno=e.ytno AND pool.ts <= e.ask_ts AND pool.ts > e.ask_ts - interval '150 seconds')
)
SELECT CASE WHEN ask_ts - free_ts <= interval '90 seconds' THEN 'a) 자유 뒤 90초 안 등재(직전 틱엔 작업 중 — 예측이 못 잡음)'
            WHEN EXISTS (SELECT 1 FROM pool WHERE pool.ytno=miss.ytno AND pool.ts BETWEEN miss.free_ts AND miss.ask_ts) THEN 'b) 자유~등재 사이 풀에 있던 적 있음(직전 틱만 빠짐)'
            ELSE 'c) 자유~등재 내내 풀 밖(위치 없음/명단 밖/오판)' END AS 이유,
       count(*) AS 건, round(100.0*count(*)/sum(count(*)) OVER (),1) AS "%",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM ask_ts-free_ts))::numeric,0) AS "자유→등재 중앙(초)"
  FROM miss GROUP BY 1 ORDER BY 1;

ROLLBACK;
