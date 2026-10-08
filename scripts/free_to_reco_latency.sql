-- 빈 트럭 → 우리 첫 추천까지 시간을 구간으로 나눈다 (2026-10-08).
--
-- 사용: psql -v from="'2026-10-06 03:51:50+00'" -v to="'2026-10-07 23:14:10+00'" -f scripts/free_to_reco_latency.sql
--
-- 분모: 창 안의 **자유 사건** = 트럭이 상자를 내려 빈 순간(TOS 원천 드랍 로그).
--   적하 = qc_move_log LD comp_ts(QC 가 내림) · 양하 = tos_handover_label DS comp_ts(야드 인계).
--   매처의 후보 풀(livemap.rs `freed`)이 쓰는 것과 같은 두 원천이다. 트윈(같은 트럭 180초 안 연속 자유)은
--   마지막 하나만 센다 — 트럭이 실제로 빈 것은 둘째 상자를 내린 뒤다.
--
-- 구간 (모두 이 사건 기준):
--   ① 수집   = 자유(F) → 우리 표에 착지(captured_at, 추출 유닛이 INSERT 한 시각)
--   ② 틱대기 = 착지 → 그 뒤 첫 매칭 틱(stage2_pool_truck_shadow 의 틱 시각)
--   ③ 풀     = F → 이 트럭이 처음 '지금 빈 트럭'(reason free_tos)으로 풀에 오른 틱
--   ④ 추천   = F → 이 트럭에 처음 '내보내는 짝'(match_tier 1) 추천이 나온 틱 (match_ver 1)
--   TOS     = F → TOS 가 이 트럭을 배차 가능 목록에서 꺼낸 시각(tos_avail_interval.exit_ts, 다음 배차)
-- ③④ 는 그림자 효과가 섞인다 — TOS 가 먼저 배차하면 그 트럭은 풀에서 빠진다. 우리 쪽 기계 지연은 ①+②.
-- 끝까지 볼 창: F + 15분.

\set ON_ERROR_STOP on
BEGIN;
SET LOCAL statement_timeout = '120s';

CREATE TEMP TABLE ev ON COMMIT DROP AS
WITH raw AS (
  SELECT trk_id AS ytno, comp_ts AS f, captured_at AS c, 'LD'::text AS jt
    FROM qc_move_log
   WHERE jobtype = 'LD' AND trk_id IS NOT NULL AND trk_id <> ''
     AND comp_ts >= :from AND comp_ts < :to
  UNION ALL
  SELECT ytno, comp_ts, captured_at, 'DS'
    FROM tos_handover_label
   WHERE jobtype = 'DS' AND ytno IS NOT NULL AND ytno <> ''
     AND comp_ts >= :from AND comp_ts < :to
), seq AS (
  SELECT *, lead(f) OVER (PARTITION BY ytno ORDER BY f) AS next_f FROM raw
)
SELECT ytno, f, c, jt, next_f FROM seq
 WHERE next_f IS NULL OR next_f > f + interval '180 seconds';

CREATE TEMP TABLE lat ON COMMIT DROP AS
SELECT e.ytno, e.jt, e.f, e.c,
       t.ts  AS tick_ts,
       p.ts  AS pool_ts,
       r.ts  AS reco_ts,
       d.exit_ts AS tos_ts
  FROM ev e
  LEFT JOIN LATERAL (SELECT min(ts) AS ts FROM stage2_pool_truck_shadow s
                      WHERE s.ts >= e.c AND s.ts < e.c + interval '15 minutes') t ON true
  LEFT JOIN LATERAL (SELECT min(ts) AS ts FROM stage2_pool_truck_shadow s
                      WHERE s.ytno = e.ytno AND s.reason = 'free_tos'
                        AND s.ts >= e.f AND s.ts < e.f + interval '15 minutes'
                        AND (e.next_f IS NULL OR s.ts < e.next_f)) p ON true
  LEFT JOIN LATERAL (SELECT min(ts) AS ts FROM stage2_match_shadow m
                      WHERE m.ts >= e.f AND m.ts < e.f + interval '15 minutes'
                        AND m.ytno = e.ytno AND m.match_tier = 1 AND m.match_ver = 1
                        AND (e.next_f IS NULL OR m.ts < e.next_f)) r ON true
  LEFT JOIN LATERAL (SELECT min(exit_ts) AS exit_ts FROM tos_avail_interval a
                      WHERE a.ytno = e.ytno AND a.enter_ts >= e.f - interval '60 seconds'
                        AND a.exit_ts >= e.f AND a.exit_ts < e.f + interval '15 minutes') d ON true;

\echo '── 분모와 구간(초): 중앙 / p90'
SELECT jt AS "유형", count(*) AS "자유 사건",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM c - f))::numeric, 1)       AS "①수집 중앙",
       round(percentile_cont(0.9) WITHIN GROUP (ORDER BY extract(epoch FROM c - f))::numeric, 1)       AS "①수집 p90",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM tick_ts - c))::numeric, 1) AS "②틱대기 중앙",
       round(percentile_cont(0.9) WITHIN GROUP (ORDER BY extract(epoch FROM tick_ts - c))::numeric, 1) AS "②틱대기 p90",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM tick_ts - f))::numeric, 1) AS "①+② 중앙",
       round(percentile_cont(0.9) WITHIN GROUP (ORDER BY extract(epoch FROM tick_ts - f))::numeric, 1) AS "①+② p90"
  FROM lat GROUP BY ROLLUP (jt) ORDER BY jt NULLS LAST;

\echo '── 풀·추천·TOS (F 기준 초). 비율의 분모 = 위 자유 사건'
SELECT jt AS "유형",
       round(100.0 * avg((pool_ts IS NOT NULL)::int), 1) AS "풀에 오름 %",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM pool_ts - f))::numeric, 1) AS "③풀 중앙",
       round(100.0 * avg((reco_ts IS NOT NULL)::int), 1) AS "추천 받음 %",
       round(100.0 * avg((reco_ts < f + interval '5 minutes')::int), 1) AS "5분 안 추천 %",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM reco_ts - f))::numeric, 1) AS "④추천 중앙",
       round(percentile_cont(0.9) WITHIN GROUP (ORDER BY extract(epoch FROM reco_ts - f))::numeric, 1) AS "④추천 p90",
       round(100.0 * avg((tos_ts IS NOT NULL)::int), 1) AS "TOS 배차 잡힘 %",
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM tos_ts - f))::numeric, 1) AS "TOS 중앙",
       round(percentile_cont(0.9) WITHIN GROUP (ORDER BY extract(epoch FROM tos_ts - f))::numeric, 1) AS "TOS p90"
  FROM lat GROUP BY ROLLUP (jt) ORDER BY jt NULLS LAST;

\echo '── 추천 받은 사건만: ④ = ①수집 + ②틱대기 + ⑤나머지(첫 틱 뒤 추천까지)'
SELECT jt AS "유형", count(*) AS n,
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM reco_ts - f))::numeric, 1)       AS "④ 중앙",
       round(avg(extract(epoch FROM c - f))::numeric, 1)            AS "① 평균",
       round(avg(extract(epoch FROM tick_ts - c))::numeric, 1)      AS "② 평균",
       round(avg(extract(epoch FROM reco_ts - tick_ts))::numeric, 1) AS "⑤ 평균",
       round(avg(extract(epoch FROM reco_ts - f))::numeric, 1)      AS "④ 평균(=①+②+⑤)",
       round(100.0 * avg((reco_ts = tick_ts)::int), 1)              AS "첫 틱에 바로 추천 %"
  FROM lat WHERE reco_ts IS NOT NULL
 GROUP BY ROLLUP (jt) ORDER BY jt NULLS LAST;

\echo '── TOS 보다 먼저 우리 표에 들어왔나 (①수집 < TOS) — 연계 후 우리가 줄 수 있었던 몫'
SELECT jt AS "유형", count(*) FILTER (WHERE tos_ts IS NOT NULL) AS "TOS 배차 잡힌 사건",
       round(100.0 * avg((tick_ts < tos_ts)::int) FILTER (WHERE tos_ts IS NOT NULL), 1) AS "첫 틱 < TOS 배차 %",
       round(100.0 * avg((c < tos_ts)::int) FILTER (WHERE tos_ts IS NOT NULL), 1)       AS "착지 < TOS 배차 %"
  FROM lat GROUP BY ROLLUP (jt) ORDER BY jt NULLS LAST;

ROLLBACK;
