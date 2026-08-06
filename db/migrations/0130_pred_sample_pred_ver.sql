-- 0130: dispatch_pred_sample 에 예측 '공식' 판별자와 계획 순번을 더한다.
-- 기록기가 지금까지 적어온 것은 배선된 상자별 예측이 아니라 옛 공식(구역ETA+i/rem×p·ETW순
-- front-6)이었다. 이제 상자별 예측(Stage2Work.work_eta_ts)을 적는다. 두 모집단을 같은 표에서
-- 섞어 읽으면 mig0117 이 고친 것과 같은 사고가 나므로 판별자를 둔다(레거시 행은 NULL).
ALTER TABLE dispatch_pred_sample ADD COLUMN IF NOT EXISTS pred_ver smallint;
ALTER TABLE dispatch_pred_sample ADD COLUMN IF NOT EXISTS slot_idx integer;
COMMENT ON COLUMN dispatch_pred_sample.pred_ver IS
  '예측 공식 판. NULL=레거시 front-6(구역ETA+균등분배·ETW순). 2=상자별(적부계획 slot×move_s). '
  '집계는 반드시 이 값으로 가를 것. 2026-08-06 이후 새 행은 전부 2 (mig 0130).';
COMMENT ON COLUMN dispatch_pred_sample.slot_idx IS
  '기록 시점의 구역 안 순번(Stage2Work.slot_idx). 오차를 순번 수로 가르는 분석용. 레거시 NULL.';
COMMENT ON MATERIALIZED VIEW learn_work_eta_bias IS
  '⚠2026-08-06: 원천 행의 예측 공식이 바뀌었다(pred_ver 참조). 이 매뷰는 pred_ver 를 거르지
  않으므로 전환 후 7일간 두 모집단이 섞인다. 게이지 전용(되먹임 없음)이라 허용. 분석은
  pred_ver=2 로 직접 거를 것.';
