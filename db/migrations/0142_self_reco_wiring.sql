-- 0142: 실배차 전환 준비 — ① 자기 추천 이력 배선 ② 매치의 상자 단위 식별자 ③ 생산 0 경보.
--
-- 배경: 풀은 "TOS 미배차 상자"만 담는다(2026-08-04 결정). 우리가 실제 배차자가 되면
-- '이미 배차됨'의 출처가 TOS 가 아니라 **우리 자신의 직전 추천**이어야 한다 — 추천을 내고
-- TOS 추출에 반영되기까지(~1-2분) 같은 작업을 재추천하면 이중 배차가 난다. 그 장치를
-- 지금 배선하고(shadow 에서는 게이지로 검증만), 전환은 유닛 환경변수 DISPATCH_MODE 로 한다.
ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS contno text;
COMMENT ON COLUMN stage2_match_shadow.contno IS
  '추천 대상 상자(트윈이면 대표 contno). 집계 버킷 추천은 NULL. 2026-08-10(mig 0142)부터 '
  '기록 — 자기 추천 이력의 키이자 상자 단위 집행(TOS 공유)의 필수 식별자. 이전 행은 NULL.';
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS self_cover_n integer;
COMMENT ON COLUMN stage2_solver_shadow.self_cover_n IS
  '최근 180초 안에 추천했던 작업이 이 틱 풀 후보에 다시 나타난 수(자기 추천 이력 적중). '
  'shadow 모드 = 계상만(배선 검증 게이지 — 직전 틱 추천 수와 비슷해야 정상, 0이면 키 불일치). '
  'active 모드(DISPATCH_MODE=active) = 이 수만큼 풀에서 제외(재추천 방지, TTL 지나면 재진입). '
  '(mig 0142)';
