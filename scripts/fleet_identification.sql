-- 근무 중 함대 특정 — "지금 이 순간 근무 중인 트럭을 우리가 바로 특정할 수 있나".
--   psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/fleet_identification.sql   (약 2~3분)
--
-- ■ 왜 (2026-08-19 사용자 질문)
-- "근무 중인 모든 트럭에 매 틱 다음 작업을 배정한다"는 설계는 근무 중 함대를 실시간으로 특정할 수 있어야
-- 성립한다. 조 시작에 명단이 정해지면 쉽고, 중간 투입·퇴근이 잦으면 어렵다. 이 스크립트가 그 답이다.
--
-- ■ 먼저 확인한 것 (Oracle·2026-08-19)
-- TOS 에 "이번 조 트럭 명단" 표는 없다. 가장 가까운 `MCH_WORKTIME`(오퍼레이터 로그인 세션)은 **끝난 구간만**
-- 기록된다 — 오늘 4,695세션/536대 중 열린 세션 0건, 최근 60분에 끝난 트럭 119대 vs 같은 시각 실제 활동
-- 450대. 실시간 "지금 근무 중" 판정에는 못 쓴다(사후 검증용).
--
-- ■ 그래서 재는 것
--   ③④ 활동 기록으로 근무 세션(3시간 공백 기준)을 잘라 교대 시각·세션 길이를 본다.
--   ⑤   규칙 "최근 N시간 TOS 활동(배차 또는 자유) 있는 트럭 = 근무 중" 의 재현율·정밀도를 시각마다 잰다.
--       재현율 = 다음 60분에 요청하는 트럭 중 규칙에 잡힌 비율(놓치면 답을 못 준다).
--       정밀도 = 규칙이 잡은 트럭 중 60분 안에 요청한 비율(낮으면 슬롯을 낭비할 뿐, pull 에선 해가 작다).
--   ⑥   3h 규칙이 놓친 트럭의 정체(신규 투입 vs 장기 공백 복귀).
--   ⑦   GPS 출현을 합치면 얼마나 더 잡히나(`truck_pos_hist` 2일 보관이라 2일 창).
--   ⑧   규칙이 잡았지만 안 온 트럭 — 퇴근인가 긴 공백인가.
--
-- ■ 분모
-- 재현율의 분모 = (표본 시각 × 그 뒤 60분 안에 요청한 트럭). 표본 시각은 매시 정각(7일 = 168개, ⑦은 48개).
-- 요청 = `tt_move_log` DS/LD 왕복 1건(twin 첫 레그).

\set ON_ERROR_STOP on
\pset null '-'

BEGIN;
SET LOCAL statement_timeout = '300s';

CREATE TEMP TABLE ev AS
SELECT ytno, dispatch_ts AS ts FROM tt_move_log WHERE dispatch_ts > now()-interval '8 days'
UNION ALL SELECT ytno, free_ts FROM tt_move_log WHERE free_ts > now()-interval '8 days';
CREATE INDEX ON ev (ytno, ts); CREATE INDEX ON ev (ts); ANALYZE ev;

CREATE TEMP TABLE req AS
SELECT ytno, dispatch_ts FROM tt_move_log WHERE dispatch_ts > now()-interval '7 days' AND twin_leg_seq=1 AND jobtype IN ('DS','LD');
CREATE INDEX ON req (dispatch_ts); ANALYZE req;

CREATE TEMP TABLE pts AS SELECT generate_series(date_trunc('hour', now()-interval '7 days'), date_trunc('hour', now()-interval '1 hour'), interval '1 hour') AS t;
CREATE TEMP TABLE r AS SELECT p.t, q.ytno, min(q.dispatch_ts) AS first_req FROM pts p JOIN req q ON q.dispatch_ts > p.t AND q.dispatch_ts <= p.t + interval '60 minutes' GROUP BY 1,2;
CREATE INDEX ON r (ytno, t); ANALYZE r;
CREATE TEMP TABLE last_ev AS
SELECT p.t, e.ytno, max(e.ts) AS last_ts FROM pts p JOIN ev e ON e.ts <= p.t AND e.ts > p.t - interval '24 hours' GROUP BY 1,2;
CREATE INDEX ON last_ev (t, ytno); ANALYZE last_ev;

\echo ''
\echo '════ ③ 근무 세션(활동 3h 공백으로 구분) 시작/끝이 하루 중 언제 몰리나 (7일·MYT) — 봉우리 = 교대 ════'
WITH o AS (SELECT ytno, ts, lag(ts) OVER (PARTITION BY ytno ORDER BY ts) AS prev FROM ev),
     f AS (SELECT ytno, ts, CASE WHEN prev IS NULL OR ts - prev > interval '3 hours' THEN 1 ELSE 0 END AS new_s FROM o),
     g AS (SELECT ytno, ts, sum(new_s) OVER (PARTITION BY ytno ORDER BY ts) AS sid FROM f),
     ses AS (SELECT ytno, sid, min(ts) AS s_start, max(ts) AS s_end FROM g GROUP BY 1,2)
SELECT h AS "시(MYT)",
  (SELECT count(*) FROM ses WHERE EXTRACT(hour FROM s_start AT TIME ZONE 'Asia/Kuala_Lumpur')=h AND s_start > now()-interval '7 days') AS 세션시작,
  (SELECT count(*) FROM ses WHERE EXTRACT(hour FROM s_end AT TIME ZONE 'Asia/Kuala_Lumpur')=h AND s_end < now()-interval '3 hours' AND s_end > now()-interval '7 days') AS 세션끝
 FROM generate_series(0,23) h ORDER BY 1;

\echo ''
\echo '════ ④ 세션 길이 — 차량은 교대를 넘겨 계속 도는가 ════'
WITH o AS (SELECT ytno, ts, lag(ts) OVER (PARTITION BY ytno ORDER BY ts) AS prev FROM ev),
     f AS (SELECT ytno, ts, CASE WHEN prev IS NULL OR ts - prev > interval '3 hours' THEN 1 ELSE 0 END AS new_s FROM o),
     g AS (SELECT ytno, ts, sum(new_s) OVER (PARTITION BY ytno ORDER BY ts) AS sid FROM f),
     ses AS (SELECT ytno, sid, min(ts) AS s_start, max(ts) AS s_end FROM g GROUP BY 1,2)
SELECT count(*) AS "완결 세션(7일)", count(DISTINCT ytno) AS 트럭,
  round(percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM s_end-s_start)/3600)::numeric,1) AS "길이 중앙(h)",
  round(percentile_cont(0.9) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM s_end-s_start)/3600)::numeric,1) AS "p90(h)",
  round(100.0*count(*) FILTER (WHERE s_end-s_start < interval '4 hours')/count(*),1) AS "4h 미만 %"
 FROM ses WHERE s_end < now()-interval '3 hours' AND s_start > now()-interval '7 days';

\echo ''
\echo '════ ⑤ ★규칙 "최근 N시간 TOS 활동 = 근무 중" — 다음 60분 요청 트럭 재현율·정밀도 (168 표본 시각·7일) ════'
SELECT n.hrs AS "N시간",
  round(100.0*count(*) FILTER (WHERE le.last_ts > r.t - (n.hrs||' hours')::interval)/count(*),1) AS "재현율 %",
  (SELECT round(count(*)::numeric/(SELECT count(*) FROM pts),0) FROM last_ev l2 WHERE l2.last_ts > l2.t - (n.hrs||' hours')::interval) AS "잡은 트럭 평균/시각",
  round(100.0*count(*) FILTER (WHERE le.last_ts > r.t - (n.hrs||' hours')::interval)
        / (SELECT count(*) FROM last_ev l2 WHERE l2.last_ts > l2.t - (n.hrs||' hours')::interval),1) AS "정밀도 %(잡은 트럭 중 60분 안 요청)"
 FROM (VALUES ('0.5'),('1'),('2'),('3'),('6'),('12'),('24')) AS n(hrs)
 CROSS JOIN r LEFT JOIN last_ev le ON le.t=r.t AND le.ytno=r.ytno
 GROUP BY n.hrs ORDER BY n.hrs::numeric;

\echo ''
\echo '════ ⑥ 3h 규칙이 놓친 요청 트럭 — 마지막 활동이 얼마나 오래됐나 ════'
SELECT CASE WHEN le.last_ts IS NULL THEN 'a) 24h 안 활동 없음(신규/장기 복귀)'
            WHEN le.last_ts <= r.t - interval '12 hours' THEN 'b) 12~24h 전'
            WHEN le.last_ts <= r.t - interval '6 hours' THEN 'c) 6~12h 전'
            ELSE 'd) 3~6h 전' END AS "마지막 활동",
       count(*) AS "놓친(시각×트럭)", round(100.0*count(*)/sum(count(*)) OVER (),1) AS "%"
  FROM r LEFT JOIN last_ev le ON le.t=r.t AND le.ytno=r.ytno
 WHERE le.last_ts IS NULL OR le.last_ts <= r.t - interval '3 hours'
 GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ⑦ GPS 출현을 합치면 (2일·48 표본 시각 — truck_pos_hist 보관 한계) ════'
WITH x AS (
  SELECT r.*,
    EXISTS (SELECT 1 FROM ev e WHERE e.ytno=r.ytno AND e.ts <= r.t AND e.ts > r.t - interval '3 hours') AS tos3h,
    EXISTS (SELECT 1 FROM truck_pos_hist p WHERE p.ytno=r.ytno AND p.ts <= r.t AND p.ts > r.t - interval '30 minutes') AS gps30m,
    EXISTS (SELECT 1 FROM truck_pos_hist p WHERE p.ytno=r.ytno AND p.ts <= r.t AND p.ts > r.t - interval '2 hours') AS gps2h,
    EXISTS (SELECT 1 FROM truck_pos_hist p WHERE p.ytno=r.ytno AND p.ts <= r.first_req AND p.ts > r.first_req - interval '10 minutes') AS gps_before_req
  FROM r WHERE r.t > now() - interval '2 days'
)
SELECT count(*) AS "요청(시각×트럭)",
  round(100.0*count(*) FILTER (WHERE tos3h)/count(*),1) AS "TOS 3h %",
  round(100.0*count(*) FILTER (WHERE tos3h OR gps30m)/count(*),1) AS "TOS 3h ∪ GPS 30분 %",
  round(100.0*count(*) FILTER (WHERE tos3h OR gps2h)/count(*),1) AS "TOS 3h ∪ GPS 2h %",
  round(100.0*count(*) FILTER (WHERE NOT tos3h AND gps_before_req)/nullif(count(*) FILTER (WHERE NOT tos3h),0),1) AS "3h 놓친 것 중 요청 10분 전 GPS 있음 %"
 FROM x;

\echo ''
\echo '════ ⑧ 정밀도 꼬리 — 3h 규칙이 잡았지만 60분 안 요청 없던 트럭은 그 뒤 언제 왔나 ════'
WITH act AS (
  SELECT p.t, e.ytno FROM pts p JOIN ev e ON e.ts <= p.t AND e.ts > p.t - interval '3 hours' WHERE p.t > now()-interval '2 days' GROUP BY 1,2
), nx AS (
  SELECT a.t, a.ytno, (SELECT min(dispatch_ts) FROM req q WHERE q.ytno=a.ytno AND q.dispatch_ts > a.t) AS next_req FROM act a
   WHERE NOT EXISTS (SELECT 1 FROM r WHERE r.t=a.t AND r.ytno=a.ytno)
)
SELECT count(*) AS "잡았지만 60분 안 요청 없음",
  round(100.0*count(*) FILTER (WHERE next_req <= t + interval '2 hours')/count(*),1) AS "1~2h 안 요청 %",
  round(100.0*count(*) FILTER (WHERE next_req > t + interval '2 hours' AND next_req <= t + interval '6 hours')/count(*),1) AS "2~6h %",
  round(100.0*count(*) FILTER (WHERE next_req IS NULL OR next_req > t + interval '6 hours')/count(*),1) AS "6h+ 또는 없음(사실상 퇴근) %"
 FROM nx;

ROLLBACK;
