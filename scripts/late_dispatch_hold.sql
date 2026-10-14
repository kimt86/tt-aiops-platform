-- 늦게 도착한 TOS 배차로 남은 고정 (2026-10-13) — 보낼 때 빈 트럭이었는데 TOS 가 이미(보내기 전 찍힌 작업목록에서)
-- 배차해 둔 트럭의 고정이, 그 목록이 도착한 뒤에도 얼마나 더 남았나.
-- 사용: psql -v from="'…'" -v to="'…'" -f scripts/late_dispatch_hold.sql
--
-- 분모: 창 안에 적힌 1계층 짝(dispatch_sent kind='assign'·shadow — **아직 안 풀린 것 포함**, 그건 지금 시각까지로 잰다) 중, 그 트럭이 **보낸 시각 이전에 찍혔고
--   (as_of ≤ 보낸 시각+2초 — 고정 풀기 ①이 안 받는 목록) 결정에 쓴 목록보다는 뒤인** 배차 목록 스냅샷(assigned_tt_hist)에
--   마지막 자유 이후로 실린 것. = "보낼 때 우리는 몰랐지만 TOS 는 이미 배차해 둔" 트럭.
-- 지표: 그 스냅샷이 도착한 시각(그 목록을 처음 쓴 매칭 틱의 착지 시각 ts − workpool_age_s) → 고정이 풀린 시각.
--   옛 규칙은 다음 목록(약 1분 뒤)까지 남고, 새 규칙(배차 > 자유면 바쁨)은 그 목록이 도착한 틱에 풀어야 한다.
-- ⚠ 자유 = 원천 드랍 로그(적하 qc_move_log LD · 양하 tos_handover_label DS)의 보낸 시각 이전 마지막 comp_ts — 매칭과 같은 정의.
-- ⚠ assigned_tt_hist 는 3일 보관 — 창은 그 안에서. ⚠ 보낸 시각 이전 3h 안에 자유 사건이 없는 짝(GPS 로만 빈 트럭)은 '자유 이후'를
--   못 따져 분모에서 빠진다(1차 리뷰).
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL statement_timeout = '120s';
WITH d AS (
  SELECT msg_id, ytno, written_at, basis_as_of, coalesce(released_at, now()) AS released_at,
         coalesce(release_why, 'open') AS release_why
    FROM dispatch_sent
   WHERE kind = 'assign' AND mode = 'shadow' AND written_at >= :from AND written_at < :to
     AND basis_as_of IS NOT NULL
), f AS (
  SELECT d.msg_id, greatest(
           (SELECT max(comp_ts) FROM qc_move_log q
             WHERE q.trk_id = d.ytno AND q.jobtype = 'LD' AND q.comp_ts > d.written_at - interval '3 hours' AND q.comp_ts <= d.written_at),
           (SELECT max(comp_ts) FROM tos_handover_label h
             WHERE h.ytno = d.ytno AND h.jobtype = 'DS' AND h.comp_ts > d.written_at - interval '3 hours' AND h.comp_ts <= d.written_at)
         ) AS free_ts
    FROM d
), late AS (
  -- 보내기 전 찍혔지만(①이 못 받는 목록) 결정 목록보다 뒤, 자유 이후에 실린 첫 스냅샷
  SELECT d.*, f.free_ts, s.as_of_ts AS late_as_of
    FROM d JOIN f USING (msg_id)
    JOIN LATERAL (
      SELECT a.as_of_ts FROM assigned_tt_hist a
       WHERE a.ytno = d.ytno AND a.as_of_ts > d.basis_as_of
         AND a.as_of_ts <= d.written_at + interval '2 seconds'
         AND a.as_of_ts >= f.free_ts
       ORDER BY a.as_of_ts LIMIT 1
    ) s ON true
), land AS (
  -- 그 스냅샷을 처음 쓴 틱의 착지 시각
  SELECT late.*, (SELECT min(t.ts - t.workpool_age_s * interval '1 second')
                    FROM stage2_solver_shadow t
                   WHERE t.ts >= late.late_as_of AND t.ts < late.late_as_of + interval '10 minutes'
                     AND t.match_ver = 1 AND t.workpool_age_s IS NOT NULL
                     AND t.ts - t.workpool_age_s * interval '1 second' >= late.late_as_of - interval '1 second') AS landed
    FROM late
)
SELECT (SELECT count(*) FROM d)                                                        AS "1계층 짝",
       count(*)                                                                         AS "늦은 배차 짝",
       count(*) FILTER (WHERE release_why = 'truck_busy')                               AS "그중 truck_busy",
       count(*) FILTER (WHERE release_why IN ('listed','not_listed'))                   AS "그중 다음 목록",
       count(*) FILTER (WHERE release_why = 'open')                                     AS "그중 아직 안 풀림",
       round(avg(extract(epoch FROM released_at - landed))::numeric, 1)                 AS "도착→풀림 평균초",
       percentile_disc(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM released_at - landed)) AS "도착→풀림 중앙초",
       round(max(extract(epoch FROM released_at - landed))::numeric, 1)                 AS "도착→풀림 최대초",
       round(avg(extract(epoch FROM released_at - written_at))::numeric, 1)             AS "보냄→풀림 평균초",
       count(*) FILTER (WHERE landed IS NULL)                                           AS "착지 못 찾음"
  FROM land;
ROLLBACK;
