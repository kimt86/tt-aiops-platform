-- TOS 가 '배차 가능'이라고 본 트럭을 우리 풀이 '지금 빈 트럭'으로 잡았나 (2026-10-13 · pool_ver 10/11 반대쪽 비용 점검).
-- 사용: psql -v from="'…'" -v to="'…'" -f scripts/tos_avail_pool_catch.sql
--
-- 정답지 = tos_avail_interval(TOS 배차기 getMoverList 의 커밋 단위 재생·mig 0162).
-- ⚠ pool_ver 10 까지만 독립 측정이다. **pool_ver 11 부터는 같은 표가 풀 판정에 들어간다**(등재 뒤 열린 구간이면 빈 트럭) —
--   11 의 값은 대부분 구조상 맞는 값이라 9·10 과 한 줄로 비교하면 안 된다(2차 리뷰: 11 의 2h 포착 93구간 중 58 이 그 규칙 덕분).
--   배차 가능 목록 재생 자체의 오류도 이 지표로는 안 보인다. 판 사이 표본 구성(로그인 비중)도 다르다.
-- 분모: 창 안에 시작한 배차 가능 구간 중 30초 이상 머문 것(exit_cause gap 은 끝을 몰라 뺀다) 가운데, 시작 15초 뒤 ~ 끝 사이에
--   우리 매칭 틱이 하나라도 있었던 것. 15초 = 자유 기록이 우리 표에 들어오는 시간(중앙 ~9초) 여유.
--   TOS 는 보통 2~5초 만에 다시 배차하므로 30초+ 구간은 소수다 — '오래 놀던 빈 트럭'의 표본이다.
-- 분자: 그 사이 틱에서 그 트럭이 '지금 빈 트럭'(reason free_tos · gps_free)으로 풀에 있었다.
-- 판으로 가른다: 틱의 pool_ver.
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL statement_timeout = '120s';
WITH iv AS (
  SELECT ytno, enter_ts, exit_ts, enter_cause, pool
    FROM tos_avail_interval
   WHERE enter_ts >= :from AND enter_ts < :to AND exit_ts IS NOT NULL
     AND exit_ts - enter_ts >= interval '30 seconds' AND coalesce(exit_cause, '') <> 'gap'
), j AS (
  SELECT iv.*, p.pool_ver,
         bool_or(p.reason IN ('free_tos', 'gps_free')) AS caught
    FROM iv
    JOIN stage2_pool_truck_shadow p
      ON p.ts >= iv.enter_ts + interval '15 seconds' AND p.ts < iv.exit_ts
   WHERE p.ytno = iv.ytno
   GROUP BY iv.ytno, iv.enter_ts, iv.exit_ts, iv.enter_cause, iv.pool, p.pool_ver
), denom AS (
  -- 그 사이에 틱이 있었는데 그 트럭이 풀에 아예 없던 구간도 분모에 넣는다(놓친 것)
  SELECT iv.*, (SELECT max(q.pool_ver) FROM stage2_pool_truck_shadow q
                 WHERE q.ts >= iv.enter_ts + interval '15 seconds' AND q.ts < iv.exit_ts) AS pool_ver
    FROM iv
)
SELECT d.pool_ver, d.enter_cause,
       count(*)                                   AS "배차 가능 30초+ 구간(틱 있음)",
       count(*) FILTER (WHERE j.caught)           AS "지금 빈 트럭으로 잡음",
       round(100.0 * count(*) FILTER (WHERE j.caught) / nullif(count(*), 0), 1) AS "잡은 %"
  FROM denom d
  LEFT JOIN j USING (ytno, enter_ts)
 WHERE d.pool_ver IS NOT NULL
 GROUP BY ROLLUP (d.pool_ver, d.enter_cause)
 ORDER BY 1, 2;
ROLLBACK;
