-- 굶김 크로스체크 — "우리 마감이 늦어서 크레인이 굶는가" 를 반사실과 독립된 자료로 본다.
--   psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/starvation_crosscheck.sql
--
-- ■ 왜 필요한가
-- `scripts/counterfactual_wait.sql` ① 의 "늦음 %(상한<0)" 은 **종이 위의 늦음**이다 — 우리 트럭이
-- 크레인이 그 상자를 다룬 뒤에 도착한다는 뜻일 뿐, 그동안 크레인이 실제로 놀았는지는 말해주지 않는다.
-- 크레인은 대개 다른 상자를 한다(특히 양하는 상자↔트럭 바인딩이 픽업까지 유동이라 더욱 그렇다).
-- 그래서 **크레인이 진짜로 굶은 기록**과 맞대본다.
--
-- ■ 어느 컬럼을 쓰나 — `starving_real` 아니라 `genuine`
-- `db/migrations/0050_qc_wait_genuine.sql`: `starving_real` 은 정상적인 무브 사이 간격·해치커버·베이
-- 이동까지 굶김으로 세어 **약 53배 과다 탐지**한다. `genuine` 은 여기에 "600m 안에 보낼 수 있는 빈
-- 트럭이 하나도 없었다"(near_idle_tt=0)를 더한 것 = **"크레인이 멈췄는데 보낼 트럭이 없었다"**.
-- mig0050 권고대로 **2틱 지속 필터**(연속 30초 틱 2개 이상)를 걸어 GPS 순간 끊김을 뺀다.
--
-- ■ 분모 (세 절이 서로 다르다 — 섞어 읽지 말 것)
--   ① 크레인이 일하고 있던 30초 틱 전체 (크레인별 1행/틱)
--   ② `counterfactual_wait.sql` ⓪★ 와 같은 분모: 창 안에서 우리가 처음 1계층 추천했고 TOS 도
--      배차했고 크레인 처리 실적이 있는 (상자, 작업유형)
--   ③ ② 중 위약 시각(+6시간)에 그 크레인이 일하고 있던 것만
--
-- ■ 한계
-- `genuine` 은 TOS 배차 아래에서 관측된 값이다. 우리 정책으로 바꿨을 때의 굶김이 아니다.
-- 여기서 답하는 것은 **"우리가 늦다고 나온 자리에 실제 굶김이 있었나"** 하나다.

\set ON_ERROR_STOP on
\pset null '-'

BEGIN;
SET LOCAL statement_timeout = '180s';

-- ─────────────────────────────────────────────────────────────────────────────
-- 굶김 틱 (2틱 지속 필터) · 창을 넉넉히 잡는다(위약이 +6시간을 보므로)
-- ─────────────────────────────────────────────────────────────────────────────
CREATE TEMP TABLE g AS
WITH s AS (
  SELECT qc, ts,
         genuine                                               AS gen_raw,   -- NULL 가능 (mig0050: 옛 행은 백필 안 함)
         coalesce(starving_real, false)                        AS sr,
         coalesce(genuine, false)                              AS gen,
         -- 정의 재계산(livemap.rs:2606 이 저장할 때 쓰는 식). ⚠양방향으로 대조해야 한다 —
         -- 「저장 참 · 정의 거짓」만 보면 저장식이 정의의 부분집합이라 구조적으로 0이 나온다.
         coalesce(starving_real,false) AND coalesce(near_idle_tt,-1) = 0 AS gen_def,
         coalesce(near_idle_tt, 0)                             AS near_n,
         lag(coalesce(genuine,false))  OVER w AS pg, lag(ts) OVER w AS pts,
         lead(coalesce(genuine,false)) OVER w AS ng, lead(ts) OVER w AS nts,
         -- ③ 민감도도 genuine 과 같은 2틱 지속을 걸어야 한다 (한쪽만 1틱이면 수준을 비교할 수 없다)
         lag(coalesce(starving_real,false) AND coalesce(near_idle_tt,0) > 0)  OVER w AS pk,
         lead(coalesce(starving_real,false) AND coalesce(near_idle_tt,0) > 0) OVER w AS nk
    FROM qc_wait_qc_sample
   WHERE ts BETWEEN now() - interval '9 days' AND now()
   WINDOW w AS (PARTITION BY qc ORDER BY ts)
)
SELECT qc, ts, sr, gen, gen_raw, gen_def,
       gen AND ( (pg AND ts - pts <= interval '90 seconds')
              OR (ng AND nts - ts <= interval '90 seconds') ) AS gen2,
       -- ⑤ 민감도용: genuine 이 정의상 제외하는 갈래 = 「멈췄는데 옆에 빈 트럭이 있었다」
       sr AND near_n > 0 AND ( (pk AND ts - pts <= interval '90 seconds')
                            OR (nk AND nts - ts <= interval '90 seconds') ) AS stuck_with_truck
  FROM s;
CREATE INDEX ON g (qc, ts);
ANALYZE g;

-- 겹침 조회는 굶김 틱만 있으면 된다 — 876k 를 매번 훑지 않도록 따로 뽑는다
CREATE TEMP TABLE g2 AS SELECT qc, ts FROM g WHERE gen2;
CREATE INDEX ON g2 (qc, ts);
ANALYZE g2;

-- ─────────────────────────────────────────────────────────────────────────────
-- 반사실 표본 (counterfactual_wait.sql 의 cf 를 슬림하게 재조립 · 같은 게이트)
-- ─────────────────────────────────────────────────────────────────────────────
CREATE TEMP TABLE cf2 AS
WITH r AS (
  SELECT contno, jobtype, min(ts) AS first_ts,
         (array_agg(arrival_s    ORDER BY ts))[1] AS a_s,
         (array_agg(lead_extra_s ORDER BY ts))[1] AS e_s
    FROM stage2_match_shadow
   WHERE ts BETWEEN now() - interval '8 days' AND now() - interval '1 day'
     AND contno IS NOT NULL AND jobtype IN ('DS','LD')
     AND match_tier IS DISTINCT FROM 2          -- 2계층(미리 배정·mig0161) 제외 — 창 안엔 0건이지만 잣대를 같게 둔다
   GROUP BY 1,2
), d AS (
  SELECT r.*, t.dispatch_ts AS tos_dis_ts
    FROM r LEFT JOIN LATERAL (
      SELECT dispatch_ts FROM tt_move_log t
       WHERE t.contno = r.contno AND t.jobtype = r.jobtype
         AND t.dispatch_ts >= r.first_ts - interval '90 seconds'
         AND t.dispatch_ts <  r.first_ts + interval '12 hours'
       ORDER BY t.dispatch_ts LIMIT 1) t ON true
), c AS (
  SELECT d.*, q.machno, q.comp_ts
    FROM d LEFT JOIN LATERAL (
      SELECT machno, comp_ts FROM qc_move_log q
       WHERE q.contno = d.contno AND q.jobtype = d.jobtype
         AND q.comp_ts >= d.tos_dis_ts AND q.comp_ts < d.tos_dis_ts + interval '12 hours'
       ORDER BY q.comp_ts LIMIT 1) q ON d.tos_dis_ts IS NOT NULL
)
SELECT contno, jobtype, machno, comp_ts,
       first_ts + make_interval(secs => a_s + e_s) AS ready_ts,
       EXTRACT(epoch FROM comp_ts - (first_ts + make_interval(secs => a_s + e_s)))::int AS wait_upper_s
  FROM c WHERE comp_ts IS NOT NULL AND machno IS NOT NULL;
CREATE INDEX ON cf2 (machno, comp_ts);
ANALYZE cf2;

\echo ''
\echo '════ ⓪ 자료 점검 — genuine 컬럼이 정의대로인가 · 크레인 이름이 두 표에서 같은가 ════'
SELECT count(*) AS "창+여유 틱",
       count(*) FILTER (WHERE gen AND NOT gen_def)     AS "저장 참·정의 거짓(0이어야)",
       count(*) FILTER (WHERE gen_def AND NOT gen)     AS "★정의 참·저장 거짓(0이어야·백필 누락 탐지)",
       count(*) FILTER (WHERE gen_raw IS NULL)         AS "★genuine 이 NULL 인 행",
       count(*) FILTER (WHERE sr)   AS "starving_real 틱",
       count(*) FILTER (WHERE gen)  AS "genuine 틱",
       count(*) FILTER (WHERE gen2) AS "genuine 2틱지속"
  FROM g;
SELECT count(*) AS "표본 크레인", count(gq.qc) AS "굶김표에도 있는 크레인"
  FROM (SELECT DISTINCT machno FROM cf2) m
  LEFT JOIN (SELECT DISTINCT qc FROM g) gq ON gq.qc = m.machno;

\echo ''
\echo '════ ① 실제 굶김의 크기 — 크레인이 일하던 30초 틱 중 몇 %가 "멈췄는데 보낼 트럭이 없었다" 인가 ════'
\echo '   분모 = 창(8일~1일 전) 안에서 크레인이 일하고 있던 30초 틱 전체. ⚠starving_real 은 53배 과다 탐지라 쓰지 않는다.'
SELECT count(*)                                                     AS "일하던 틱",
       count(*) FILTER (WHERE sr)                                   AS "starving_real(참고·안 씀)",
       round(100.0*count(*) FILTER (WHERE sr)/count(*),2)           AS "starving_real %",
       count(*) FILTER (WHERE gen2)                                 AS "★genuine 2틱지속",
       round(100.0*count(*) FILTER (WHERE gen2)/count(*),2)         AS "★genuine %",
       count(DISTINCT qc)                                           AS "크레인 수",
       round((EXTRACT(epoch FROM max(ts)-min(ts))/86400.0)::numeric,2) AS "창 일수",
       -- ⚠상수로 나누지 않는다. 창은 (now−8일 ~ now−1일) = 7일인데 8로 나눠 12.5% 과소 보고한 전례가 있다.
       round((count(*) FILTER (WHERE gen2)*30/60.0
              / nullif(EXTRACT(epoch FROM max(ts)-min(ts))/86400.0,0))::numeric,1) AS "★하루 굶김 분(전 크레인 합)",
       round((count(*) FILTER (WHERE gen2)*30/60.0
              / nullif(EXTRACT(epoch FROM max(ts)-min(ts))/86400.0,0)
              / nullif(count(DISTINCT qc),0))::numeric,1)           AS "★크레인 한 대당 하루 분"
  FROM g WHERE ts BETWEEN now() - interval '8 days' AND now() - interval '1 day';

-- ─────────────────────────────────────────────────────────────────────────────
-- 겹침 표 — 실제와 위약을 **한 행에** 담는다.
-- ⚠종전 판은 ②(전체 분모)와 ③(위약이 성립하는 것만)을 따로 내놓고 문서에서 나란히 읽었다.
--   분모가 달라 비교가 성립하지 않는다. 이제 공통 분모(②-b)를 반드시 함께 낸다.
-- ─────────────────────────────────────────────────────────────────────────────
CREATE TEMP TABLE gq AS SELECT DISTINCT qc FROM g;
CREATE INDEX ON gq (qc);
ANALYZE gq;

CREATE TEMP TABLE ov AS
SELECT f.jobtype, (f.wait_upper_s < 0) AS late, f.machno,
       EXISTS (SELECT 1 FROM gq WHERE gq.qc = f.machno) AS crane_logged,
       EXISTS (SELECT 1 FROM g2 WHERE g2.qc = f.machno
                AND g2.ts BETWEEN f.comp_ts - interval '5 minutes'
                              AND f.comp_ts + interval '5 minutes') AS hit,
       EXISTS (SELECT 1 FROM g  WHERE g.qc  = f.machno
                AND g.ts  BETWEEN f.comp_ts + interval '6 hours' - interval '5 minutes'
                              AND f.comp_ts + interval '6 hours' + interval '5 minutes') AS pbo_working,
       EXISTS (SELECT 1 FROM g2 WHERE g2.qc = f.machno
                AND g2.ts BETWEEN f.comp_ts + interval '6 hours' - interval '5 minutes'
                              AND f.comp_ts + interval '6 hours' + interval '5 minutes') AS pbo_hit
  FROM cf2 f;
ANALYZE ov;

\echo ''
\echo '════ ②-0 표본 크레인 중 굶김표에 행이 아예 없는 몫 — 이 상자들은 겹침이 구조적으로 0 이다 ════'
SELECT jobtype AS 작업, count(*) AS 상자,
       count(*) FILTER (WHERE NOT crane_logged) AS "굶김표에 없는 크레인의 상자",
       round(100.0*count(*) FILTER (WHERE NOT crane_logged)/count(*),1) AS "%"
  FROM ov GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ② 겹침(전체 분모) — 우리가 「늦다」고 나온 자리에 실제 굶김이 있었나 ════'
\echo '   늦음 = 우리 트럭이 그 상자 처리보다 늦게 도착(상한<0). 굶김 = 그 크레인의 처리 시각 ±5분 안에 genuine 2틱지속 틱.'
SELECT jobtype AS 작업,
       CASE WHEN late THEN '우리가 늦음' ELSE '우리가 이름(대기)' END AS 갈래,
       count(*) AS 상자, count(*) FILTER (WHERE hit) AS "굶김 겹침",
       round(100.0*count(*) FILTER (WHERE hit)/count(*),1) AS "겹침 %"
  FROM ov GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '════ ②-b ★공통 분모 — 위약이 성립하는 상자만으로 실제와 위약을 나란히 (이 표만 비교에 쓸 것) ════'
\echo '   분모 = 그 크레인이 6시간 뒤에도 일하고 있던 상자. 실제 겹침과 배경 발생률을 같은 모집단에서 잰다.'
SELECT jobtype AS 작업,
       CASE WHEN late THEN '우리가 늦음' ELSE '우리가 이름(대기)' END AS 갈래,
       count(*) FILTER (WHERE pbo_working) AS "공통 분모",
       round(100.0*count(*) FILTER (WHERE pbo_working AND hit)
             / nullif(count(*) FILTER (WHERE pbo_working),0),1) AS "실제 겹침 %",
       round(100.0*count(*) FILTER (WHERE pbo_working AND pbo_hit)
             / nullif(count(*) FILTER (WHERE pbo_working),0),1) AS "위약 겹침 %(배경)",
       count(*) FILTER (WHERE NOT pbo_working) AS "위약 때 안 일함(뺌)"
  FROM ov GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '════ ③ 민감도 — genuine 이 「정의상 제외하는」 갈래로도 재본다 ════'
\echo '   genuine 은 「옆에 빈 트럭이 없었다」를 요구한다. 그런데 우리가 늦어서 생길 굶김은 「빈 트럭은 있는데 안 보냈다」 모양이라'
\echo '   그 갈래에 구조적으로 눈이 먼다. 제외 갈래(멈췄는데 옆에 빈 트럭 있음)로도 겹침을 재서 갈래를 가르는지 본다.'
\echo '   두 갈래가 비슷하면 그건 배경 소음이고, genuine 을 쓴 선택이 옳다. 필터는 genuine 과 같은 2틱 지속으로 맞췄다.'
SELECT f.jobtype AS 작업,
       CASE WHEN f.wait_upper_s < 0 THEN '우리가 늦음' ELSE '우리가 이름(대기)' END AS 갈래,
       count(*) AS 상자,
       round(100.0*count(*) FILTER (WHERE x.hit2)/count(*),1) AS "제외 갈래 겹침 %"
  FROM cf2 f
  CROSS JOIN LATERAL (
    SELECT EXISTS (SELECT 1 FROM g WHERE g.qc = f.machno
                    AND g.ts BETWEEN f.comp_ts - interval '5 minutes' AND f.comp_ts + interval '5 minutes'
                    AND g.stuck_with_truck) AS hit2) x
 GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '════ ④ 늦음의 크기 — 늦다고 나온 건은 얼마나 늦나 (분모 = ② 의 늦음 갈래) ════'
SELECT jobtype AS 작업, count(*) AS 상자,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY -wait_upper_s)/60)::numeric,1) AS "늦는 정도 중앙(분)",
       round((percentile_cont(0.9) WITHIN GROUP (ORDER BY -wait_upper_s)/60)::numeric,1) AS "p90(분)"
  FROM cf2 WHERE wait_upper_s < 0 GROUP BY 1 ORDER BY 1;

ROLLBACK;
