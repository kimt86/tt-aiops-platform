-- 곧 빌 트럭 신뢰도 재측정 (mig 0163 의 learn_soon_free_reliability 와 같은 잣대).
--
-- 질문: 후보 풀에 "곧 빔"으로 든 트럭이 그 뒤 실제로 언제 짐을 내렸나?
-- 분모: 후보 풀(stage2_pool_truck_shadow)의 (트럭, 틱) 쌍 중 곧-빔 갈래(reason LIKE 'inflight%').
-- 정답: TOS 내림 기록 — 적하 qc_move_log(trk_id·comp_ts), 양하 tos_handover_label(ytno·comp_ts).
--       매처의 자유 판정과 같은 원천이다(livemap.rs freed CTE).
-- 갈래: (풀 사유, 트럭의 지금 작업유형, 예측 남은 시간 구간 0/60/300/900/3600).
--
-- 쓰는 법:  psql -v from="'2026-10-06 00:00+08'" -v to="'2026-10-07 00:00+08'" -f scripts/soon_free_reliability.sql
--   창 끝은 now() − 30분보다 앞이어야 추적 30분이 다 찬다(아니면 '안 내림'이 부풀려진다).
--   ⚠GPS 공백 09-18 17:00 ~ 09-30 08:29 MYT 는 풀 기록이 0행이라 창에 걸리면 비어 보인다.
-- 안정성 확인(CLAUDE.md): 같은 질의를 창을 반으로 잘라 한 번 더 돌려 비율이 비슷한지 본다.
-- 실측 2026-10-01 07:45~15:15 MYT: 1분 안 0~16% · 5분 안 0~57% (두 반창 안정).
\if :{?from}
\else
  \set from '''2026-10-06 00:00+08'''
\endif
\if :{?to}
\else
  \set to '''2026-10-07 00:00+08'''
\endif
BEGIN;
SET LOCAL statement_timeout = '180s';
WITH p AS (
  SELECT p.ts, p.ytno, p.reason, p.jobtype, p.pool_ver,
         CASE WHEN p.free_in_s <= 0 THEN 0 WHEN p.free_in_s <= 60 THEN 60 WHEN p.free_in_s <= 300 THEN 300
              WHEN p.free_in_s <= 900 THEN 900 ELSE 3600 END AS pred_b
    FROM stage2_pool_truck_shadow p
   WHERE p.ts >= :from::timestamptz AND p.ts < :to::timestamptz
     AND p.ts <= now() - interval '30 minutes'
     AND p.reason LIKE 'inflight%' AND p.jobtype IN ('DS','LD')
), d AS MATERIALIZED (
  SELECT p.*, CASE p.jobtype
     WHEN 'LD' THEN (SELECT min(q.comp_ts) FROM qc_move_log q WHERE q.trk_id = p.ytno AND q.jobtype = 'LD'
                        AND q.comp_ts > p.ts AND q.comp_ts <= p.ts + interval '30 minutes')
     WHEN 'DS' THEN (SELECT min(h.comp_ts) FROM tos_handover_label h WHERE h.ytno = p.ytno AND h.jobtype = 'DS'
                        AND h.comp_ts > p.ts AND h.comp_ts <= p.ts + interval '30 minutes')
   END AS drop_ts FROM p
)
SELECT pool_ver, reason, jobtype, pred_b AS "예측 구간(초)", count(*) AS "쌍 수", count(DISTINCT ytno) AS "트럭",
       round(100.0 * count(*) FILTER (WHERE drop_ts <= ts + interval '60 seconds')  / count(*), 1) AS "1분 안 %",
       round(100.0 * count(*) FILTER (WHERE drop_ts <= ts + interval '300 seconds') / count(*), 1) AS "5분 안 %",
       round(100.0 * count(*) FILTER (WHERE drop_ts <= ts + interval '900 seconds') / count(*), 1) AS "15분 안 %",
       round(100.0 * count(*) FILTER (WHERE drop_ts <= ts + interval '1800 seconds') / count(*), 1) AS "30분 안 %",
       round(100.0 * count(*) FILTER (WHERE drop_ts IS NULL) / count(*), 1) AS "30분 안 안 내림 %"
  FROM d
 GROUP BY 1, 2, 3, 4
HAVING count(*) >= 30
 ORDER BY 1, 2, 3, 4;
ROLLBACK;
