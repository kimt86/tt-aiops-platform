-- 0140: 배차 마감의 원천을 전방 예측에서 출항 역산으로 (pool_mode=2 판별자).
--
-- 방향 전환(2026-08-10 사용자 결정): 목표는 예측 정확도가 아니라 "처음 정한 출항시간을
-- 지키는 상식적인 배차"다. 마감 = 요구(requirement)로 다시 정의한다:
--   상자 마감 = (출항 목표 − 이 상자 뒤에 남은 크레인 작업 소요) − 트럭 준비시간
-- 남은 작업량은 카운터·계획(잔여 거울)에서 오므로 QC 가 작업을 완료할 때마다(반영 ≤ ~2분)
-- 마감이 그 시점 기준으로 다시 매겨진다 — 전방 예측처럼 오차가 누적될 자리가 없다.
-- 출항 목표가 없는 배(스케줄 미상)만 종전 전방 예측 마감으로 폴백한다.
-- 매칭 실행가능 축(deadline_slack_s/feasible, deadline_ver=2 = work_eta 기준)은 바뀌지 않는다.
COMMENT ON COLUMN stage2_solver_shadow.pool_mode IS
  '매칭을 구동한 풀. NULL=전환 전(레거시), 1=설계③ 마감 풀(마감=전방 예측 − 준비시간, '
  '2026-08-06~10), 2=설계③ 풀 + 출항 역산 마감(2026-08-10~, mig 0140). n_works·greedy_*·'
  'optimal_* 및 같은 ts 의 stage2_match_shadow/stage2_pool_shadow 행의 모집단이 이 값에 따라 '
  '다르다. 집계는 반드시 이 값으로 가를 것 (mig 0132·0133·0140).';
COMMENT ON COLUMN stage2_match_shadow.dispatch_deadline_ts IS
  '이 상자의 배차 마감. 같은 ts 의 solver.pool_mode 로 가를 것: <2 = 설계②(크레인 도달 예측 '
  '− 준비시간, mig 0120) / 2 = 출항 역산(출항 목표 − 뒤에 남은 작업 소요 − 준비시간, mig 0140).';
COMMENT ON COLUMN stage2_match_shadow.dd_slack_s IS
  'dispatch_deadline_ts − 추천 시각. 음수 = 이미 배차했어야 할 작업. 마감의 정의는 '
  'pool_mode 에 따라 다르다(0140 주석 참조) — 두 구간을 섞어 읽지 말 것.';
COMMENT ON COLUMN stage2_pool_shadow.dispatch_deadline_ts IS
  '풀 후보의 배차 마감. 같은 ts 의 solver.pool_mode 로 가를 것(<2 전방 예측 / 2 출항 역산, '
  'mig 0140).';
COMMENT ON COLUMN dispatch_pred_sample.dispatch_deadline_ts IS
  '로깅 시점의 배차 마감. 2026-08-09 23:09:47Z(배포 경계) 이전 = 전방 예측 기준(설계②), '
  '이후 = 출항 역산 기준(mig 0140). 채점(pred_work_eta_ts·resolved_at)은 이 컬럼과 무관하다.';
