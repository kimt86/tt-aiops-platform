-- 0160: 재지향 가능 공차 갈래 (pool_ver 8 · 2026-08-25 사용자 결정)
--
-- 배차받고 픽업 전인 빈 트럭(TOS 정본 = live_workpool Q+트럭 행 ∧ 같은 트럭의 A행 없음
-- ∧ 픽업 로그 가드)을 후보 풀에 넣는다(reason='redirectable'). 긴급 작업이 전환 벌점
-- (REDIRECT_PENALTY_S=180초)을 물고 그 트럭을 집으면 "재지향(스왑)" 추천이다.
-- ⚠Q행만으로 판별하면 안 된다 — TOS 는 적재 운행 중에도 다음 작업을 선배정한다
-- (실측 2026-08-25: Q틱의 7.7%가 실제 적재 중·tt_move_log 구간 대조·docs/cycles 참조).
ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS redirected_from text;
COMMENT ON COLUMN stage2_match_shadow.redirected_from IS
  '재지향(스왑) 추천 표식: 이 트럭이 추천 시점에 TOS 배차로 붙들고 있던 작업(qc queuename '
  '[contno]). NULL = 보통 추천(빈/곧 빌 트럭). 2026-08-25(mig 0160·pool_ver 8)부터. '
  'veh_state=''redirectable'' 과 항상 짝이다.';
COMMENT ON COLUMN stage2_pool_truck_shadow.pool_ver IS
  '풀 규칙 판. 집계는 반드시 이 값으로 가른다 — 판이 다르면 모집단이 다르다(livemap.rs POOL_VER). '
  '8 = 재지향 가능 공차 갈래 추가(2026-08-25) — 배차됨·픽업 전 빈 트럭(reason=redirectable)이 풀에 '
  '들어온다. truck_n(Stage-1 슬롯 수)에는 세지 않아 발행량 불변. '
  '7 = 추출기가 Q+트럭(배차됨·픽업 전) 행을 live_workpool 에 착지(2026-08-24) — 배차 신호가 ≤60초에 '
  '도착해 배차된 트럭이 풀에서 더 일찍 빠진다. 6 이하 경계는 mig0156·livemap.rs 참조.';
