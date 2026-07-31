-- 출항 마감 티어 A/B 판정 리포트.
--   사용: PGPASSWORD=wp psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/ab_report.sql
--   전제: tt-api 가 STAGE2_DEP_TIER=ab 로 돌고 있어야 한다(그때만 ab_block 이 채워진다).
--
-- 읽는 법: 두 팔은 같은 조건에서 무작위로 배정된 30분 블록들이다. 차이가 표본 오차보다
-- 크지 않으면 "레버가 효과 없음"이 정직한 결론이고, 그때는 켜 둘 이유가 없다.
-- ⚠ ab_warmup 틱은 제외한다 — 팔이 막 바뀐 직후는 직전 팔의 anti-thrash 잔상이 남아 있다.

\echo '=== 1. 표본 규모 (블록/틱) ==='
SELECT dep_tier_on AS "팔(ON=티어적용)",
       count(DISTINCT ab_block)                     AS 블록,
       count(*)                                     AS 틱,
       min(ts)::timestamp(0)                        AS 시작,
       max(ts)::timestamp(0)                        AS 끝
  FROM stage2_solver_shadow
 WHERE ab_block IS NOT NULL AND NOT ab_warmup
 GROUP BY 1 ORDER BY 1;

\echo ''
\echo '=== 2. 결과 지표 (틱 평균) ==='
-- optimal_miss = 배정했지만 p90 도착이 그 작업의 마감을 넘긴 건수(작을수록 좋음)
-- 비용/배정   = 배정 1건당 평균 빈 차 이동 초(작을수록 좋음) — 레버가 치르는 효율 대가
-- 늦은버킷    = 이미 마감을 넘긴(티어0) 버킷이 최종 풀에 몇 개 들어왔나(레버의 의도된 효과)
-- 잘린비율    = 트럭이 모자라 후보에서 잘린 비율. 두 팔이 비슷해야 조건이 같다는 뜻.
SELECT dep_tier_on                                          AS 팔,
       round(avg(optimal_miss), 2)                          AS "마감미스/틱",
       round(avg(optimal_cost_s::numeric / NULLIF(optimal_n,0)))  AS "비용/배정(초)",
       round(avg(dep_tier0_n), 2)                           AS "늦은버킷/틱",
       round(avg(dep_urgent_slots), 1)                      AS "긴급슬롯/틱",
       round(avg(dep_demoted_n), 2)                         AS "예산강등/틱",
       round(avg(n_trucks), 1)                              AS "트럭/틱",
       round(100.0 * avg(1 - n_works::numeric / NULLIF(works_raw,0)), 1) AS "잘린비율%"
  FROM stage2_solver_shadow
 WHERE ab_block IS NOT NULL AND NOT ab_warmup
 GROUP BY 1 ORDER BY 1;

\echo ''
\echo '=== 3. 재배정(thrash) — 매칭 행 기준 ==='
-- ⚠ tick 으로 조인하지 말 것. tick 은 프로세스 안의 카운터라 **API 를 재시작할 때마다 0부터
-- 다시 시작한다** — 재시작이 한 번이라도 있으면 같은 tick 값이 여러 시각에 존재해 조인이
-- 부풀어 오른다(실측: 8틱 × 45배정 ≈ 360 이어야 할 것이 9,363건으로 26배). 두 INSERT 는 같은
-- `ts` 변수를 바인딩하고 match 의 PK 가 (ts, ytno) 이므로 ts 조인이 정확하고 유일하다.
SELECT s.dep_tier_on                                        AS 팔,
       count(*)                                             AS 배정건,
       round(100.0 * count(*) FILTER (WHERE m.switched) / count(*), 1) AS "전환율%",
       round(100.0 * count(*) FILTER (WHERE NOT m.feasible) / count(*), 1) AS "마감초과%"
  FROM stage2_solver_shadow s
  JOIN stage2_match_shadow  m ON m.ts = s.ts
 WHERE s.ab_block IS NOT NULL AND NOT s.ab_warmup
 GROUP BY 1 ORDER BY 1;

\echo ''
\echo '=== 4. 블록 단위 분포 (표본 오차 감각용) ==='
-- 틱은 서로 독립이 아니다(같은 블록 안에서는 조건이 이어진다). 블록을 하나의 관측으로 보고
-- 흩어짐을 본다 — 두 팔의 평균 차이가 이 흩어짐 안에 묻히면 판정 불가다.
SELECT dep_tier_on AS 팔,
       count(*)                                   AS 블록,
       round(avg(m), 2)                           AS "블록평균 마감미스",
       round(stddev(m), 2)                        AS 표준편차,
       round(min(m), 2)                           AS 최소,
       round(max(m), 2)                           AS 최대
  FROM (SELECT ab_block, dep_tier_on, avg(optimal_miss) AS m
          FROM stage2_solver_shadow
         WHERE ab_block IS NOT NULL AND NOT ab_warmup
         GROUP BY 1, 2) b
 GROUP BY 1 ORDER BY 1;

\echo ''
\echo '=== 5. 판정 (블록 단위 Welch 근사) ==='
-- 틱은 서로 독립이 아니므로 **블록 하나를 관측 하나로** 본다. 각 지표에 대해 팔 간 차이를
-- 표준오차로 나눈 t 를 낸다. |t| < 2 면 표본이 부족하거나 효과가 없다는 뜻이고, 그때
-- "좋아졌다"고 말하면 안 된다 — 이 하네스를 만든 이유가 그 말을 못 하게 하는 것이다.
-- ⚠ 블록이 팔당 10개 미만이면 표준편차 추정 자체가 불안정해 t 도 믿을 수 없다.
WITH b AS (
  SELECT ab_block, dep_tier_on,
         avg(optimal_miss)                                   AS miss,
         avg(optimal_cost_s::numeric / NULLIF(optimal_n,0))  AS cost,
         avg(dep_tier0_n)                                    AS late_served
    FROM stage2_solver_shadow
   WHERE ab_block IS NOT NULL AND NOT ab_warmup
   GROUP BY 1,2
), m AS (
  SELECT 'ON'::text arm, count(*) n, avg(miss) miss, stddev(miss) s_miss,
         avg(cost) cost, stddev(cost) s_cost, avg(late_served) late, stddev(late_served) s_late
    FROM b WHERE dep_tier_on
  UNION ALL
  SELECT 'OFF', count(*), avg(miss), stddev(miss), avg(cost), stddev(cost), avg(late_served), stddev(late_served)
    FROM b WHERE NOT dep_tier_on
), t AS (
  SELECT (SELECT n FROM m WHERE arm='ON')  AS n_on,
         (SELECT n FROM m WHERE arm='OFF') AS n_off,
         (SELECT miss FROM m WHERE arm='OFF') - (SELECT miss FROM m WHERE arm='ON') AS d_miss,
         sqrt((SELECT s_miss^2/NULLIF(n,0) FROM m WHERE arm='ON')
            + (SELECT s_miss^2/NULLIF(n,0) FROM m WHERE arm='OFF'))                  AS se_miss,
         (SELECT cost FROM m WHERE arm='ON') - (SELECT cost FROM m WHERE arm='OFF') AS d_cost,
         sqrt((SELECT s_cost^2/NULLIF(n,0) FROM m WHERE arm='ON')
            + (SELECT s_cost^2/NULLIF(n,0) FROM m WHERE arm='OFF'))                  AS se_cost,
         (SELECT late FROM m WHERE arm='ON') - (SELECT late FROM m WHERE arm='OFF') AS d_late,
         sqrt((SELECT s_late^2/NULLIF(n,0) FROM m WHERE arm='ON')
            + (SELECT s_late^2/NULLIF(n,0) FROM m WHERE arm='OFF'))                  AS se_late
)
SELECT 지표, round(차이::numeric,2) "차이(ON−OFF)", round(se::numeric,2) 표준오차,
       round((차이/NULLIF(se,0))::numeric,2) t,
       CASE WHEN LEAST(n_on,n_off) < 10 THEN '표본부족(팔당 10블록 미만)'
            WHEN abs(차이/NULLIF(se,0)) >= 2 THEN '유의'
            ELSE '판정불가(효과 없거나 표본 부족)' END AS 판정
  FROM t, LATERAL (VALUES
    ('마감미스 감소(+가 좋음)', d_miss, se_miss),
    ('배정당 비용(−가 좋음)',   d_cost, se_cost),
    ('늦은버킷 서빙(+가 의도)', d_late, se_late)
  ) v(지표, 차이, se);

\echo ''
\echo '=== 6. 앞으로 얼마나 더 필요한가 ==='
WITH b AS (
  SELECT ab_block, dep_tier_on, avg(optimal_miss) miss
    FROM stage2_solver_shadow WHERE ab_block IS NOT NULL AND NOT ab_warmup GROUP BY 1,2
)
SELECT round(stddev(miss)::numeric,2)                                    AS "블록간 표준편차",
       count(*) FILTER (WHERE dep_tier_on)                               AS "ON 블록",
       count(*) FILTER (WHERE NOT dep_tier_on)                           AS "OFF 블록",
       ceil(16 * (stddev(miss)/1.0)^2)                                   AS "1.0차이 감지에 필요(팔당)",
       round((ceil(16*(stddev(miss)/1.0)^2)*2*0.5/24)::numeric,1)        AS "그때까지 일수"
  FROM b;
