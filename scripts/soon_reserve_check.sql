-- 곧 빌 트럭이 붙잡은 작업은 얼마나 늦게 보내졌나 (2026-10-15 · match_ver 2 점검).
-- 사용: psql -v from="'…'" -v to="'…'" -f scripts/soon_reserve_check.sql
--
-- 배경: match_ver 2 는 곧 빌 트럭도 데드라인 도래 작업의 후보로 넣고, 그 짝(match_tier=2)은 지시를 내지 않는다. 예측이 틀려
--   트럭이 안 비면 그 작업은 지금 빈 트럭을 못 받고 다음 틱으로 밀린다(KC stage2-design '알아 둘 점'). 그 손해를 잰다.
--
-- 분모(①②): 창 안에 match_ver 2 로 한 번이라도 짝이 지어진 상자(contno 있는 것).
--   ① 곧 빌 트럭에 먼저 잡힌 상자 = 처음 지은 짝이 match_tier=2 인 상자.
--   ② 그 상자가 처음 '보낸 짝'(match_tier=1)이 되기까지 걸린 시간 — 첫 예약 → 첫 지시. 보낸 짝이 끝내 없으면 '안 보냄'으로 센다
--      (TOS 가 먼저 가져갔거나, 데드라인 도래 풀에서 빠졌거나 — 이 질의로는 구분하지 않는다).
--   ③ 그 상자를 처음 잡은 곧 빌 트럭이 실제로 그 상자의 지시를 받았나(같은 트럭이 첫 보낸 짝인가) — 예측대로 비었다는 간접 증거.
-- ⚠ 틱은 분당 ~5회라 '첫 지시까지 시간'은 틱 간격(~12초) 만큼 거칠다. ⚠ 21일 보관.
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL statement_timeout = '120s';
WITH m AS (
  SELECT ts, contno, ytno, match_tier
    FROM stage2_match_shadow
   WHERE ts >= :from::timestamptz AND ts < :to::timestamptz
     AND match_ver = 2 AND contno IS NOT NULL
), firsts AS (
  SELECT contno,
         (array_agg(match_tier ORDER BY ts))[1]                                 AS first_tier,
         min(ts) FILTER (WHERE match_tier = 2)                                  AS first_soon,
         (array_agg(ytno ORDER BY ts) FILTER (WHERE match_tier = 2))[1]         AS soon_truck,
         min(ts) FILTER (WHERE match_tier = 1)                                  AS first_sent,
         (array_agg(ytno ORDER BY ts) FILTER (WHERE match_tier = 1))[1]         AS sent_truck
    FROM m GROUP BY contno
)
SELECT count(*)                                                                  AS "짝이 지어진 상자",
       count(*) FILTER (WHERE first_tier = 2)                                    AS "① 곧 빌 트럭에 먼저 잡힘",
       round(100.0 * count(*) FILTER (WHERE first_tier = 2) / nullif(count(*), 0), 1) AS "① %",
       count(*) FILTER (WHERE first_tier = 2 AND first_sent IS NULL)             AS "② 끝내 안 보냄",
       percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM first_sent - first_soon))
         FILTER (WHERE first_tier = 2 AND first_sent IS NOT NULL)               AS "② 첫 예약→첫 지시 중앙(초)",
       percentile_cont(0.9) WITHIN GROUP (ORDER BY extract(epoch FROM first_sent - first_soon))
         FILTER (WHERE first_tier = 2 AND first_sent IS NOT NULL)               AS "② 상위10%(초)",
       round(100.0 * count(*) FILTER (WHERE first_tier = 2 AND sent_truck = soon_truck)
             / nullif(count(*) FILTER (WHERE first_tier = 2 AND first_sent IS NOT NULL), 0), 1)
                                                                                 AS "③ 같은 트럭이 지시 받음 %"
  FROM firsts;
ROLLBACK;
