-- 이미 TOS 가 배차한 트럭에 보낸 1계층 짝 (2026-10-13 · pool_ver 10 점검).
-- 사용: psql -v from="'…'" -v to="'…'" -f scripts/sent_to_dispatched.sql
--
-- 분모: 창 안에 적힌 1계층 짝(dispatch_sent kind='assign'·shadow) 중 보낸 시각 이전 3시간 안에 자유 사건이 있는 것.
-- ★본 지표 = 'TOS 선배차 %' — 그 트럭에 마지막 자유 뒤·보낸 시각 전의 TOS 배차(tt_move_log.dispatch_ts)가 있다. 규칙이 쓰는 신호
--   (배차 목록 등재)와 **다른 원천**이라 독립 증거다(1차 리뷰). 아래 넷으로 가른 칸은 설명용이다 — 특히 첫 칸은 규칙이 쓰는 신호로
--   가른 것이라 pool_ver 10+ 에서 0 인 것은 '구현이 규칙대로 돈다'는 확인일 뿐 효과의 증거가 아니다.
--   결정 전 목록에 이미 실림 : 자유 뒤 첫 등재(assigned_tt_hist) ≤ 결정에 쓴 목록 시각 — 우리가 알 수 있었던 것(pool_ver 10 이 막는 대상).
--                              pool_ver 9 에선 목록에서 빠진 뒤 픽업 기록 전 틈으로 샜다(10-12 19:00Z~10-13 04:00Z 25,369 중 4,791).
--   결정 뒤·보내기 전 등재   : 늦게 도착한 목록 — 고정 풀기가 그 목록 도착 틱에 풀어야 한다(scripts/late_dispatch_hold.sql).
--   보낸 뒤 처음 등재        : 그림자에서 TOS 가 우리보다 먼저 가져간 것 — 우리가 알 방법이 없었다(정상).
--   등재 이력 없음           : 그 트럭의 자유 뒤 등재가 이력에 없다(이력 공백·재시작·3일 보관 밖) — 판정 불가.
-- ⚠ tt_move_log 는 최종 배차만 남긴다(재배정된 첫 배차는 빠짐) — 선배차는 약간 과소. ⚠ assigned_tt_hist 는 3일 보관.
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL statement_timeout = '120s';
WITH d AS (
  SELECT msg_id, ytno, written_at, basis_as_of, released_at FROM dispatch_sent
   WHERE kind = 'assign' AND mode = 'shadow' AND written_at >= :from AND written_at < :to
), f AS (
  SELECT d.*, greatest(
    (SELECT max(comp_ts) FROM qc_move_log q WHERE q.trk_id = d.ytno AND q.jobtype = 'LD'
        AND q.comp_ts > d.written_at - interval '3 hours' AND q.comp_ts <= d.written_at),
    (SELECT max(comp_ts) FROM tos_handover_label h WHERE h.ytno = d.ytno AND h.jobtype = 'DS'
        AND h.comp_ts > d.written_at - interval '3 hours' AND h.comp_ts <= d.written_at)) AS free_ts
  FROM d
), x AS (
  SELECT f.*,
    (SELECT min(dispatch_ts) FROM tt_move_log t WHERE t.ytno = f.ytno AND t.dispatch_ts > f.free_ts AND t.dispatch_ts <= f.written_at) AS dis_before,
    (SELECT min(a.as_of_ts) FROM assigned_tt_hist a WHERE a.ytno = f.ytno AND a.as_of_ts >= f.free_ts
        AND a.as_of_ts > f.written_at - interval '3 hours') AS first_list
  FROM f WHERE f.free_ts IS NOT NULL
)
SELECT count(*)                                                                                    AS "1계층 짝(자유 있음)",
       count(*) FILTER (WHERE dis_before IS NOT NULL)                                              AS "TOS 선배차",
       round(100.0 * count(*) FILTER (WHERE dis_before IS NOT NULL) / nullif(count(*), 0), 1)     AS "TOS 선배차%",
       count(*) FILTER (WHERE dis_before IS NOT NULL AND first_list <= basis_as_of)                AS "결정 전 목록에 이미 실림",
       round(100.0 * count(*) FILTER (WHERE dis_before IS NOT NULL AND first_list <= basis_as_of) / nullif(count(*), 0), 1) AS "그 비율%",
       count(*) FILTER (WHERE dis_before IS NOT NULL AND first_list > basis_as_of
                          AND first_list <= written_at + interval '2 seconds')                     AS "결정 뒤·보내기 전 등재",
       count(*) FILTER (WHERE dis_before IS NOT NULL AND first_list > written_at + interval '2 seconds') AS "보낸 뒤 처음 등재",
       count(*) FILTER (WHERE dis_before IS NOT NULL AND first_list IS NULL)                       AS "등재 이력 없음"
  FROM x;
ROLLBACK;
