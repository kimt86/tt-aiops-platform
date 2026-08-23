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

-- ⚠`jobstatus` 는 임의값일 수 있다: `live_assigned_tt` 는 (ytno, jobstatus) 단위라 한 트럭이 A·Q 두 행을 갖는 경우가
-- 있는데(관측 395행/357트럭 = 38대) PK 가 (as_of_ts, ytno) 라 `ON CONFLICT DO NOTHING` 이 하나를 버린다. 존재 여부만
-- 쓰는 현재 측정(자유 뒤 첫 등재)에는 무해하나, **이 컬럼으로 A/Q 를 가르지 말 것**(2026-08-21 2차 리뷰).
COMMENT ON COLUMN assigned_tt_hist.jobstatus IS
  '한 트럭이 A·Q 두 행을 가질 때 하나만 남는다(PK 가 ytno 단위) — 존재 여부 전용, A/Q 판별에 쓰지 말 것.';
