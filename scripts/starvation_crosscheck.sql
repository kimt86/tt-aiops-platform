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
         coalesce(starving_real, false)                        AS sr,
         coalesce(genuine, false)                              AS gen,
         coalesce(genuine, false) AND coalesce(starving_real,false) AND coalesce(near_idle_tt,-1) = 0 AS gen_chk,
         lag(coalesce(genuine,false))  OVER w AS pg, lag(ts) OVER w AS pts,
         lead(coalesce(genuine,false)) OVER w AS ng, lead(ts) OVER w AS nts
    FROM qc_wait_qc_sample
   WHERE ts BETWEEN now() - interval '9 days' AND now()
   WINDOW w AS (PARTITION BY qc ORDER BY ts)
)
SELECT qc, ts, sr, gen, gen_chk,
       gen AND ( (pg AND ts - pts <= interval '90 seconds')
              OR (ng AND nts - ts <= interval '90 seconds') ) AS gen2
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
       count(*) FILTER (WHERE gen <> gen_chk) AS "저장 genuine ≠ 정의 재계산(0이어야 함)",
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
       round((count(*) FILTER (WHERE gen2)*30/60.0/8)::numeric,1)   AS "하루 평균 굶김 분(전 크레인 합)"
  FROM g WHERE ts BETWEEN now() - interval '8 days' AND now() - interval '1 day';

\echo ''
\echo '════ ② 겹침 — 우리가 "늦다"고 나온 상자 자리에 실제 굶김이 있었나 (분모 = 반사실 표본) ════'
\echo '   늦음 = 우리 트럭이 크레인의 그 상자 처리보다 늦게 도착(상한<0). 굶김 = 그 크레인의 comp_ts ±5분 안에 genuine 2틱지속 틱 존재.'
SELECT f.jobtype AS 작업,
       CASE WHEN f.wait_upper_s < 0 THEN '우리가 늦음' ELSE '우리가 이름(대기)' END AS 갈래,
       count(*) AS 상자,
       count(*) FILTER (WHERE x.hit) AS "굶김 겹침",
       round(100.0*count(*) FILTER (WHERE x.hit)/count(*),1) AS "겹침 %"
  FROM cf2 f
  CROSS JOIN LATERAL (
    SELECT EXISTS (SELECT 1 FROM g2 WHERE g2.qc = f.machno
                    AND g2.ts BETWEEN f.comp_ts - interval '5 minutes' AND f.comp_ts + interval '5 minutes') AS hit) x
 GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '════ ③ 위약 — 같은 크레인·같은 ±5분 폭을 6시간 뒤에 대면 겹침이 얼마인가 (배경 발생률) ════'
\echo '   분모 = ② 중 위약 시각에도 그 크레인이 일하고 있던 것만(틱이 없으면 뺀다·뺀 수를 적는다).'
SELECT f.jobtype AS 작업,
       CASE WHEN f.wait_upper_s < 0 THEN '우리가 늦음' ELSE '우리가 이름(대기)' END AS 갈래,
       count(*) FILTER (WHERE x.working) AS "위약 분모(그때도 일하던)",
       count(*) FILTER (WHERE NOT x.working) AS "위약 때 안 일함(뺌)",
       round(100.0*count(*) FILTER (WHERE x.working AND x.hit)
             / nullif(count(*) FILTER (WHERE x.working),0),1) AS "위약 겹침 %"
  FROM cf2 f
  CROSS JOIN LATERAL (
    SELECT EXISTS (SELECT 1 FROM g WHERE g.qc = f.machno
                    AND g.ts BETWEEN f.comp_ts + interval '6 hours' - interval '5 minutes'
                                 AND f.comp_ts + interval '6 hours' + interval '5 minutes') AS working,
           EXISTS (SELECT 1 FROM g2 WHERE g2.qc = f.machno
                    AND g2.ts BETWEEN f.comp_ts + interval '6 hours' - interval '5 minutes'
                                 AND f.comp_ts + interval '6 hours' + interval '5 minutes') AS hit) x
 GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '════ ④ 늦음의 크기 — 늦다고 나온 건은 얼마나 늦나 (분모 = ② 의 늦음 갈래) ════'
SELECT jobtype AS 작업, count(*) AS 상자,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY -wait_upper_s)/60)::numeric,1) AS "늦는 정도 중앙(분)",
       round((percentile_cont(0.9) WITHIN GROUP (ORDER BY -wait_upper_s)/60)::numeric,1) AS "p90(분)"
  FROM cf2 WHERE wait_upper_s < 0 GROUP BY 1 ORDER BY 1;

ROLLBACK;
