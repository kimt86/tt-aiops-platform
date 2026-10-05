-- 마감 품질 비교 — 현행(출항 페이스, pool_mode 3) vs 대안(크레인 도착 예측 − 준비시간).
--   psql -v from="'2026-09-30 00:30+00'" -v to="'2026-10-06 00:00+00'" -f scripts/deadline_quality.sql
--   (창 끝은 자동으로 now()−12h 로 잘린다 — 크레인 처리 실적이 다 들어올 시간을 준다.)
--
-- ■ 질문 (2026-10-06 사용자 기준): "크레인이 트럭을 기다리는 일(늦음) ≤ 5% 를 지키면서, 트럭이 크레인 앞에서
--   기다리는 시간을 최소로" 만드는 마감은 어느 쪽인가. 그리고 **크레인 사이 순서**(우리의 차별점)는 어느 쪽이 맞나.
--
-- ■ 재료 — 매칭 기록(stage2_match_shadow)은 틱마다 추천된 상자의 두 값을 다 담는다:
--     현행 마감 dd   = dispatch_deadline_ts (출항 목표 ÷ 남은 무브 균등 페이스 − 준비시간)
--     크레인 예측 eta = ts + deadline_slack_s + od_p90_s + lead_extra_s   (deadline_slack_s 의 정의를 거꾸로 풂,
--                      livemap.rs: slack = eta − now − (p90도착 + extra). 기록 시각과 now 의 몇 초 차이는 무시)
--     대안 마감      = eta − dd_lead_s  (같은 준비시간을 뺀다 — 두 마감의 차이는 '크레인 시각'뿐이다)
--   2계층(미리 배정·08-25~)이 마감 전 상자도 매 틱 기록해 상자마다 시계열이 있다.
--
-- ■ 결정 시점 = 마감이 '도래'한 첫 틱 (마감 ≤ 그 틱 + 300초 = 매처의 1계층 판정과 같다).
--   그 시점에 보내면 트럭은 결정 + 준비시간(dd_lead_s)에 크레인에 닿는다(같은 잣대 — 트럭별 편차는 두 방법에 공통).
--     대기 w = comp_ts − (결정 + 준비시간)   (+ = 트럭이 크레인 앞에서 기다림 · − = 늦음)
--   정답 comp_ts = qc_move_log (크레인이 그 상자를 다룬 시각 — CLAUDE.md 정답지).
--   ⚠검열: 상자가 우리 기록에 처음 나타난 순간에 이미 마감이 와 있었다면 결정 시점은 '처음 본 순간'으로 잡힌다
--     (실제로는 더 일렀을 수 있다 → 그 방법의 대기를 과소평가). 비율을 같이 내고, 둘 다 검열 안 된 상자로도 낸다.
--
-- ■ 늦음 5% 에 맞춘 비교: 모든 결정을 M초 앞당기면(미루면) w 는 M 만큼 커진다(작아진다). 늦음이 5% 가
--   되는 M = −(w 의 5% 분위수)이고, 그때 대기 중앙 = 중앙(w) − 5%분위(w). **치우침은 M 이 지우고 남는 것은
--   예측 오차의 퍼짐**이다 — 이 값이 작은 쪽이 '늦음 5% 에서 덜 기다리는' 마감이다.
--   (1차 근사: 결정을 M 만큼 옮겨도 그 시점의 예측이 같다고 본다.)
--
-- ■ 위약 — TOS 자기 배차(tt_move_log.dispatch_ts)에 같은 식을 적용하면 중앙이 0 부근이어야 한다(준비시간이
--   바로 그 구간의 학습값이므로). 안 오면 잣대(준비시간)가 틀린 것이다.
--
-- ■ 순서 정확도 — 같은 틱 스냅샷에서, 서로 다른 크레인의 상자 쌍 중 마감 순서와 실제 필요 순서
--   (comp_ts − 준비시간)가 같은 비율. 우연 = 50%. TOS 는 크레인 사이에 순서가 없다(같은 순번 = 동순위).
--   스냅샷 = 15틱마다 1틱(틱 간 같은 상자 중복을 줄인다). 실제 필요 시각 차이가 2분 안인 쌍은 동률로 빼고 센다.
\if :{?from}
\else
  \set from '''2026-09-30 00:30+00'''
\endif
\if :{?to}
\else
  \set to '''2100-01-01 00:00+00'''
\endif
\set ON_ERROR_STOP on
\pset null '-'
BEGIN;
SET LOCAL statement_timeout = '300s';

CREATE TEMP TABLE m AS
SELECT ts, tick, contno, jobtype, qc, match_tier, dd_lead_s AS lead,
       dispatch_deadline_ts AS dd_cur,
       ts + make_interval(secs => deadline_slack_s + od_p90_s + lead_extra_s) AS eta,
       ts + make_interval(secs => deadline_slack_s + od_p90_s + lead_extra_s - dd_lead_s) AS dd_alt
  FROM stage2_match_shadow
 WHERE ts >= :from::timestamptz AND ts < LEAST(:to::timestamptz, now() - interval '12 hours')
   AND contno IS NOT NULL AND jobtype IN ('DS','LD');
CREATE INDEX ON m (contno, jobtype, ts);
ANALYZE m;

-- 상자별: 처음 본 순간, 두 방법의 결정 시점, 그때의 준비시간·마감값
CREATE TEMP TABLE b AS
SELECT contno, jobtype,
       min(ts) AS first_seen,
       min(ts) FILTER (WHERE dd_cur <= ts + interval '300 seconds') AS t_cur,
       min(ts) FILTER (WHERE dd_alt <= ts + interval '300 seconds') AS t_alt,
       (array_agg(lead ORDER BY ts))[1] AS lead,
       (array_agg(qc ORDER BY ts))[1] AS qc
  FROM m GROUP BY 1, 2;
-- 정답: 크레인이 그 상자를 다룬 시각(처음 본 순간 −10분 이후 첫 처리)
CREATE TEMP TABLE bc AS
SELECT b.*, c.comp_ts,
       (b.t_cur = b.first_seen) AS cens_cur, (b.t_alt = b.first_seen) AS cens_alt
  FROM b
  JOIN LATERAL (SELECT min(q.comp_ts) AS comp_ts FROM qc_move_log q
                 WHERE q.contno = b.contno AND q.jobtype = b.jobtype
                   AND q.comp_ts > b.first_seen - interval '10 minutes'
                   AND q.comp_ts < b.first_seen + interval '24 hours') c ON true
 WHERE c.comp_ts IS NOT NULL;

\echo '⓪ 분모 — 기록에 나온 상자 / 크레인 처리 실적이 붙은 상자 / 각 방법의 마감이 도래한 상자 / 처음 본 순간 이미 도래(검열)'
SELECT jobtype, (SELECT count(*) FROM b WHERE b.jobtype = bc.jobtype) AS "기록 상자", count(*) AS "실적 붙음",
       count(t_cur) AS "현행 도래", count(t_alt) AS "대안 도래",
       round(100.0 * count(*) FILTER (WHERE cens_cur) / NULLIF(count(t_cur), 0), 1) AS "현행 검열 %",
       round(100.0 * count(*) FILTER (WHERE cens_alt) / NULLIF(count(t_alt), 0), 1) AS "대안 검열 %"
  FROM bc GROUP BY 1 ORDER BY 1;

CREATE TEMP TABLE w AS
SELECT jobtype, 'cur' AS how, cens_cur AS cens, extract(epoch FROM comp_ts - (t_cur + make_interval(secs => lead))) AS w_s,
       (cens_cur OR cens_alt OR t_alt IS NULL) AS any_cens
  FROM bc WHERE t_cur IS NOT NULL AND comp_ts >= t_cur
UNION ALL
SELECT jobtype, 'alt', cens_alt, extract(epoch FROM comp_ts - (t_alt + make_interval(secs => lead))),
       (cens_cur OR cens_alt OR t_cur IS NULL)
  FROM bc WHERE t_alt IS NOT NULL AND comp_ts >= t_alt;

\echo '① 마감대로 보냈다면 — 분모: 그 방법의 마감이 도래했고 크레인 처리가 결정 뒤인 상자. 대기 + = 트럭이 기다림 · − = 늦음 (분)'
SELECT jobtype, how AS "방법", count(*) AS n,
       round(100.0 * avg((w_s < 0)::int), 1) AS "늦음 %",
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY w_s) / 60)::numeric, 1) AS "대기 중앙",
       round((percentile_cont(0.05) WITHIN GROUP (ORDER BY w_s) / 60)::numeric, 1) AS "5% 분위",
       round(((percentile_cont(0.5) WITHIN GROUP (ORDER BY w_s) - percentile_cont(0.05) WITHIN GROUP (ORDER BY w_s)) / 60)::numeric, 1)
         AS "늦음 5% 맞출 때 대기 중앙",
       round((-percentile_cont(0.05) WITHIN GROUP (ORDER BY w_s) / 60)::numeric, 1) AS "그때 당길 분(−=미룸)"
  FROM w GROUP BY 1, 2 ORDER BY 1, 2;

\echo '② 같은 표, 두 방법 모두 검열 없는 상자만 (공정 비교)'
SELECT jobtype, how AS "방법", count(*) AS n,
       round(100.0 * avg((w_s < 0)::int), 1) AS "늦음 %",
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY w_s) / 60)::numeric, 1) AS "대기 중앙",
       round(((percentile_cont(0.5) WITHIN GROUP (ORDER BY w_s) - percentile_cont(0.05) WITHIN GROUP (ORDER BY w_s)) / 60)::numeric, 1)
         AS "늦음 5% 맞출 때 대기 중앙"
  FROM w WHERE NOT any_cens GROUP BY 1, 2 ORDER BY 1, 2;

\echo '③ 위약 — TOS 자기 배차 + 같은 준비시간 → 크레인 처리. 분모: 실적 붙은 상자 중 TOS 배차가 있는 것. 중앙이 0 근처여야 잣대가 산다 (분)'
SELECT bc.jobtype, count(*) AS n,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM bc.comp_ts - (t.dispatch_ts + make_interval(secs => bc.lead)))) / 60)::numeric, 1) AS "위약 대기 중앙",
       round(100.0 * avg((bc.comp_ts < t.dispatch_ts + make_interval(secs => bc.lead))::int), 1) AS "위약 늦음 %"
  FROM bc
  JOIN LATERAL (SELECT dispatch_ts FROM tt_move_log t WHERE t.contno = bc.contno AND t.jobtype = bc.jobtype
                  AND t.dispatch_ts < bc.comp_ts AND t.dispatch_ts > bc.comp_ts - interval '3 hours'
                ORDER BY t.dispatch_ts DESC LIMIT 1) t ON true
 GROUP BY 1 ORDER BY 1;

\echo '④ 크레인 사이 순서 정확도 — 분모: 스냅샷 틱(15틱마다)에서 서로 다른 크레인의 상자 쌍(실제 필요 차이 2분 넘는 것). 우연 50%'
CREATE TEMP TABLE snap AS
SELECT m.tick, m.contno, m.jobtype, m.qc, m.dd_cur, m.dd_alt,
       bc.comp_ts - make_interval(secs => m.lead) AS need
  FROM m JOIN bc USING (contno, jobtype)
 WHERE m.tick % 15 = 0 AND bc.comp_ts > m.ts;
SELECT count(*) AS "쌍",
       round(100.0 * avg(((a.dd_cur < c.dd_cur) = (a.need < c.need))::int) FILTER (WHERE a.dd_cur <> c.dd_cur), 1) AS "현행 순서 일치 %",
       round(100.0 * avg(((a.dd_alt < c.dd_alt) = (a.need < c.need))::int) FILTER (WHERE a.dd_alt <> c.dd_alt), 1) AS "대안 순서 일치 %",
       round(100.0 * avg((a.dd_cur = c.dd_cur)::int), 1) AS "현행 동률 %"
  FROM snap a JOIN snap c ON a.tick = c.tick AND a.qc <> c.qc AND a.contno < c.contno
 WHERE abs(extract(epoch FROM a.need - c.need)) > 120;
\echo '   같은 크레인 안 쌍(참고 — 둘 다 계획 순번을 따르므로 비슷해야 한다)'
SELECT count(*) AS "쌍",
       round(100.0 * avg(((a.dd_cur < c.dd_cur) = (a.need < c.need))::int) FILTER (WHERE a.dd_cur <> c.dd_cur), 1) AS "현행 %",
       round(100.0 * avg(((a.dd_alt < c.dd_alt) = (a.need < c.need))::int) FILTER (WHERE a.dd_alt <> c.dd_alt), 1) AS "대안 %"
  FROM snap a JOIN snap c ON a.tick = c.tick AND a.qc = c.qc AND a.contno < c.contno
 WHERE abs(extract(epoch FROM a.need - c.need)) > 120;
ROLLBACK;
