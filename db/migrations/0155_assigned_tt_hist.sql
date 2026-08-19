-- 0155 — 배차 목록(live_assigned_tt) 스냅샷 이력 (2026-08-19 · 풀 재현율 측정 도구)
--
-- 왜: `tt_move_log.dispatch_ts` 는 **최종** 배차만 남긴다. 트럭이 비자마자 Q 로 배차됐다가 다른 트럭으로
-- 재배정되면 그 첫 배차는 이력에서 사라지고, 나중의 최종 배차 시각으로 채점하면 "몇 분 빈 채 있었는데 풀에
-- 없었다"는 착시가 난다(2026-08-19 TT1272 실증: 12:58:42 자유 → 12:58:5x 배차 → 회수 → 13:02:30 최종).
-- pull 구조에서 "트럭이 물어본 순간" = **자유 뒤 배차 목록에 처음 실린 틱**이다. live_assigned_tt 는 매 틱
-- 덮어쓰므로 여기 append 해 둔다. 규모 ~350행/틱 × 1,440 ≈ 50만 행/일 · 3일 보관(매처 루프에서 프룬).

CREATE TABLE IF NOT EXISTS assigned_tt_hist (
  as_of_ts  timestamptz NOT NULL,
  ytno      text        NOT NULL,
  jobstatus text,
  PRIMARY KEY (as_of_ts, ytno)
);
CREATE INDEX IF NOT EXISTS assigned_tt_hist_ytno_ts ON assigned_tt_hist (ytno, as_of_ts);

COMMENT ON TABLE assigned_tt_hist IS
  'live_assigned_tt(전 작업유형 A/B/Q 배차 트럭) 매 틱 스냅샷 이력. 자유 뒤 처음 실린 틱 = 트럭이 물어본 순간. 3일 보관.';
