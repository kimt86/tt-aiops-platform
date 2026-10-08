-- 내보낸 짝 고정 점검 (2026-10-08 · critique) — 같은 작업목록 세대 안에서 한 트럭에 다른 상자 지시가 나갔나.
-- 사용: psql -v from="'…'" -v to="'…'" -f scripts/hold_conflict_check.sql
--
-- 세대 = 작업목록 착지 틱(wake_src landing/startup/NULL) 하나부터 다음 착지 틱 전까지. 그 사이 틱(free·fallback)은
-- 같은 작업목록을 쓴다. 연계 후엔 이 사이에 우리가 보낸 짝이 TOS 반영으로 보이지 않으므로, 한 트럭에 1계층 상자가
-- 둘 이상 나가면 '두 번째 지시'다. 같은 상자에 트럭 둘이 붙은 것도 센다.
-- 분모: 창 안 (트럭, 세대) 중 1계층 추천이 한 번이라도 나간 것 / (상자, 세대) 중 1계층 추천이 나간 것.
-- ⚠ 고정은 '세대'가 아니라 '보낸 뒤 찍힌 목록'으로 푼다(as_of+2초) — 착지가 보낸 직후라 아직 반영 전이면
--   다음 세대에도 고정이 이어진다. 그래서 고정 후에도 세대 경계를 넘는 바뀜은 0이 아닐 수 있다(그건 정상).
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL statement_timeout = '120s';
WITH t AS (
  SELECT ts, wake_src FROM stage2_solver_shadow WHERE ts >= :from AND ts < :to AND match_ver = 1
), g AS (
  SELECT ts, max(ts) FILTER (WHERE wake_src IS DISTINCT FROM 'free' AND wake_src IS DISTINCT FROM 'fallback')
               OVER (ORDER BY ts) AS gen
    FROM t
), r AS (
  SELECT m.ytno, m.contno, g.gen
    FROM stage2_match_shadow m JOIN g ON g.ts = m.ts
   WHERE m.match_tier = 1 AND m.match_ver = 1 AND g.gen IS NOT NULL
)
SELECT (SELECT count(*) FROM (SELECT DISTINCT ytno, gen FROM r) a)                                    AS "(트럭,세대)",
       (SELECT count(*) FROM (SELECT ytno, gen FROM r GROUP BY 1,2 HAVING count(DISTINCT contno) > 1) a) AS "트럭에 상자 2개+",
       (SELECT count(*) FROM (SELECT DISTINCT contno, gen FROM r) a)                                  AS "(상자,세대)",
       (SELECT count(*) FROM (SELECT contno, gen FROM r GROUP BY 1,2 HAVING count(DISTINCT ytno) > 1) a)  AS "상자에 트럭 2대+",
       (SELECT count(*) FROM t)                                                                        AS "틱",
       (SELECT count(*) FROM t WHERE wake_src = 'free')                                                AS "free 틱";
ROLLBACK;
