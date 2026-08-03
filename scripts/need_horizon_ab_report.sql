-- 지평 레버(NEED_HORIZON) A/B 판정 리포트. mig0119.
--   psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/need_horizon_ab_report.sql
--
-- 승격 기준: ①(마감 정합)이 개선되고, 그 개선이 ③(크레인 커버리지) 손실보다 클 때만.
-- 워밍업 구간(블록 앞 3분)은 제외한다 — anti-thrash 의 '직전 추천'이 팔을 넘어 이어져 서로 오염된다.

\set win '12 hours'

\echo ''
\echo '=== 표본 ==='
SELECT need_horizon_on AS "지평 ON", count(*) AS 틱,
       count(DISTINCT (extract(epoch FROM ts)::bigint/60/30)) AS 블록,
       round(avg(n_trucks),1) AS 트럭, round(avg(n_works),1) AS 작업,
       round(avg(works_raw),1) AS "원본작업"
  FROM stage2_solver_shadow
 WHERE ts > now() - :'win'::interval AND need_horizon_on IS NOT NULL
   AND (extract(epoch FROM ts)::bigint/60) % 30 >= 3      -- 워밍업 제외
 GROUP BY 1 ORDER BY 1;

\echo ''
\echo '=== ① 마감 정합 (주 지표) — TOS 실제 배송 − 우리 마감. 0 에 가까울수록 좋다 ==='
WITH m AS (
  SELECT s.need_horizon_on, x.jobtype, x.ts, x.qc, x.queuename,
         x.deadline_slack_s + x.od_p90_s AS our_h, x.feasible_crane
    FROM stage2_match_shadow x
    JOIN stage2_solver_shadow s USING (ts)
   WHERE x.ts > now() - :'win'::interval AND s.need_horizon_on IS NOT NULL
     AND x.jobtype IN ('DS','LD') AND x.queuename IS NOT NULL
     AND (extract(epoch FROM x.ts)::bigint/60) % 30 >= 3
), t AS (
  SELECT m.*, d.comp
    FROM m JOIN LATERAL (
      SELECT q.comp_ts AS comp FROM qc_move_log q
       WHERE q.machno = m.qc AND q.queuename = m.queuename AND q.st_ts > m.ts
       ORDER BY q.st_ts LIMIT 1) d ON true
)
SELECT jobtype AS 작업, need_horizon_on AS "지평 ON", count(*) AS n,
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY our_h))                            AS "우리 마감",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM comp - ts)))    AS "TOS 실제",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM comp - ts) - our_h)) AS "어긋남",
       round(avg(abs(extract(epoch FROM comp - ts) - our_h)))                               AS "절대 어긋남"
  FROM t GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '=== ② 실행가능률 — 현장 크레인 기아율과 견줄 것 ==='
WITH a AS (
  SELECT s.need_horizon_on, x.jobtype, x.feasible, x.feasible_crane
    FROM stage2_match_shadow x JOIN stage2_solver_shadow s USING (ts)
   WHERE x.ts > now() - :'win'::interval AND s.need_horizon_on IS NOT NULL
     AND x.jobtype IN ('DS','LD') AND (extract(epoch FROM x.ts)::bigint/60) % 30 >= 3)
SELECT jobtype AS 작업, need_horizon_on AS "지평 ON", count(*) AS n,
       round(100.0*count(*) FILTER (WHERE feasible)/count(*),1)       AS "옛 축 %",
       round(100.0*count(*) FILTER (WHERE feasible_crane)/count(*),1) AS "새 축 %"
  FROM a GROUP BY 1,2 ORDER BY 1,2;
SELECT '  참고 · 실제 크레인 기아율 ' ||
       round(100.0*count(*) FILTER (WHERE starving_real)/count(*),1) || '%'
  FROM qc_wait_qc_sample WHERE ts > now() - :'win'::interval;

\echo ''
\echo '=== ③ 크레인 커버리지 (비용) — 트럭 한 대당 몇 대의 크레인을 덮는가 ==='
WITH t AS (
  SELECT s.ts, s.need_horizon_on, s.n_trucks,
         (SELECT count(DISTINCT qc) FROM stage2_match_shadow m WHERE m.ts = s.ts) AS qcs
    FROM stage2_solver_shadow s
   WHERE s.ts > now() - :'win'::interval AND s.need_horizon_on IS NOT NULL
     AND (extract(epoch FROM s.ts)::bigint/60) % 30 >= 3)
SELECT need_horizon_on AS "지평 ON", count(*) AS 틱,
       round(avg(n_trucks),1) AS 트럭, round(avg(qcs),2) AS 크레인,
       round(avg(qcs)/NULLIF(avg(n_trucks),0)*100,2) AS "트럭당 크레인 %"
  FROM t GROUP BY 1 ORDER BY 1;

\echo ''
\echo '=== ④ 부작용 점검 — 재지정률·매칭 규모 ==='
WITH a AS (
  SELECT s.need_horizon_on, x.switched
    FROM stage2_match_shadow x JOIN stage2_solver_shadow s USING (ts)
   WHERE x.ts > now() - :'win'::interval AND s.need_horizon_on IS NOT NULL
     AND (extract(epoch FROM x.ts)::bigint/60) % 30 >= 3)
SELECT need_horizon_on AS "지평 ON", count(*) AS n,
       round(100.0*count(*) FILTER (WHERE switched)/count(*),1) AS "재지정 %"
  FROM a GROUP BY 1 ORDER BY 1;
