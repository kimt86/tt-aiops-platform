-- 0132: stage2_solver_shadow 에 '어느 풀이 매칭을 구동했나' 판별자를 더한다.
-- 지금까지 n_works·greedy_*·optimal_*·매치 행은 전부 종전 풀(TOS 미배차·크레인당 캡)이
-- 구동한 매칭의 값이었다. 2026-08-06 부터 설계③ 풀(마감순·굶주림 항 없음)로 전환한다
-- (킬스위치 STAGE2_POOL=legacy). 두 모집단을 섞어 읽지 않도록 판별자를 둔다(판별자 규율).
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS pool_mode smallint;
COMMENT ON COLUMN stage2_solver_shadow.pool_mode IS
  '매칭을 구동한 풀. NULL=전환 전(종전 풀), 0=종전 풀(킬스위치), 1=설계③ 마감 풀. '
  'n_works·greedy_*·optimal_* 및 같은 ts 의 stage2_match_shadow 행의 모집단이 이 값에 따라 '
  '다르다. 집계는 반드시 이 값으로 가를 것 (mig 0132).';
COMMENT ON COLUMN stage2_solver_shadow.n_works IS
  '매칭에 실제 공급된 버킷 수(구동 풀 기준 — pool_mode 로 가를 것, mig 0132).';
