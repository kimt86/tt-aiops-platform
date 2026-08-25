-- 0161: 발행 2계층 — 마감 미도래 발행 지시에 잔여 트럭 배정 (2026-08-25 사용자 확정 설계)
--
-- 종전 발행(=매칭에 태우는 작업)은 마감 도래 슬롯뿐이라, 출항 여유가 큰 선박의 구역에는
-- 발행 자체가 없었다 — TOS 배차 순간 우리 유효 추천 없음(none) DS 55.6/LD 65.9%의 81%(적하)가
-- 이 경우다. 이제 1계층(마감 도래·현행 그대로)을 먼저 매칭하고, **남는 트럭**을 2계층
-- (나머지 전 발행 지시·마감 이른 순·트럭 수까지)에 배정한다. 각 층 비용은 순수 이동시간
-- (튜닝 상수 없음)·재지향(pool_ver 8) 트럭은 2계층에 못 들어간다(긴급하지 않은 일로 TOS
-- 배차를 트는 것은 손해뿐). 1계층 몫은 2계층 유무와 무관 — 종전 동작 보존.
ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS match_tier int2;
COMMENT ON COLUMN stage2_match_shadow.match_tier IS
  '발행 계층: 1 = 마감 도래 슬롯(종전 발행과 동일 모집단) · 2 = 마감 미도래 발행 지시(잔여 '
  '트럭 배정·2026-08-25 mig 0161부터). NULL = 경계 이전(그때는 1계층만 존재했으므로 전부 '
  '1계층이다). 종전 시계열과 비교할 때는 match_tier IS DISTINCT FROM 2 로 거를 것.';
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS t2_works int4;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS t2_slots int4;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS t2_assign_n int4;
COMMENT ON COLUMN stage2_solver_shadow.t2_works IS
  '2계층(마감 미도래 발행 지시)에 배정된 묶음 수. NULL = 경계 이전(mig 0161·2026-08-25). '
  '⚠경계부터 n_works·pool_new_n·optimal_n·greedy_n 은 2계층을 포함한다. 종전 정의(1계층만)로 '
  '복원 가능한 것은 n_works/pool_new_n(−t2_works)·optimal_n(−t2_assign_n)뿐이고, greedy_n 의 '
  '2계층 몫은 따로 기록하지 않아 정확 복원 불가 — 종전 시계열과 비교할 땐 t2_works IS NULL '
  '구간만 쓰거나 optimal 축으로 볼 것. trucks_held_n 정의(슬롯 못 받은 트럭)는 불변이나 '
  '2계층이 잔여 트럭을 채우므로 값의 대역이 크게 내려간다(종전 상시 188~228).';
COMMENT ON COLUMN stage2_solver_shadow.t2_slots IS
  '2계층에 배정된 슬롯 수 합(캡). 1계층 합 + 이 값 ≤ truck_n(재지향 제외) — 작업>트럭 금지는 층을 합쳐 성립.';
COMMENT ON COLUMN stage2_solver_shadow.t2_assign_n IS
  '2계층에서 실제로 성립한 추천 수(잔여 트럭 × 마감 미도래 지시의 최적 매칭). 0이 이어지는데 '
  't2_slots>0 이면 갈래가 죽은 것이다(간선 없음·거리 초과 등) — match_tier=2 행 수와 교차 확인.';
ALTER TABLE dispatch_compare_shadow ADD COLUMN IF NOT EXISTS reco_tier int2;
COMMENT ON COLUMN dispatch_compare_shadow.reco_tier IS
  'T1(TOS 배차 순간)에 유효했던 실제 추천(reco_ytno)의 발행 계층(stage2_match_shadow.match_tier). '
  'NULL = 추천 없음(none)·평가 불능·또는 경계 이전 추천. none 하락이 2계층 덕인지 이 값으로 귀속한다.';
