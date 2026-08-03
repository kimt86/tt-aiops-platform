-- 0120: 설계상의 **배차 마감**(= 크레인 시작시각 − 트럭 준비시간)을 Stage-2 에 실어 기록한다.
--       판정은 아직 바꾸지 않는다 — 같은 행에서 옛 마감과 나란히 두고 비교하기 위한 축이다.
--
-- ■ 원래 설계 (사용자 명시, 2026-08-03)
--   ① QC 작업 큐의 작업마다 **예상 작업 시작 시각**(크레인 기준)을 붙인다.
--   ② 작업 종류별 **트럭 준비시간**(작업 할당 후 QC 작업지점 도착까지)을 빼면 그것이
--      **배차를 해야 할 시각**이다.
--   ③ 그 시각으로 배차가 필요한 작업을 선정하고 후보 작업 풀을 구성한다.
--
-- ■ 실제 코드 상태 — ②까지만 있고 ③이 없다
--   ① `QueueOut.work_eta_ts`                       ✅
--   ② `workpool.rs:894  let deadline = work_eta − lead`  ✅ (LEAD_DS_S=450 / LEAD_LD_S=1180)
--      → 그런데 이 값은 **검증 로그(dispatch_pred_sample.dispatch_deadline_ts)에만** 쓰이고 버려진다.
--   ③ `Stage2Work` 구조체에 그 필드가 **아예 없다**. 매처는 그 값을 받지 못한다.
--      대신 매처는 전혀 다른 식을 쓴다(livemap.rs):
--          deadline = max(work_eta, now) + (크레인당 트럭 상한 ÷ 2) × 무브시간
--      즉 "트럭 준비시간을 뺀다"가 아니라 "크레인 시작에 상한의 절반을 **더한다**"이다.
--      그 상한은 원래 **트럭을 여러 크레인에 흩뿌리기 위한** 장치(NEED_HORIZON_S)라, 마감과는
--      무관한 설정이 마감을 정하고 있었다. 적하 실행가능률이 0% 로 고정되던 배경이 이것이다.
--
-- ■ 이 마이그레이션이 하는 일
--   `stage2_match_shadow` 에 설계 ② 기준 축을 **컬럼으로 추가**한다. 매처 동작은 그대로 두므로
--   현재 돌고 있는 판정에 오염이 없고, **같은 행에서 옛 마감과 새 마감을 직접 비교**할 수 있다.
--     dispatch_deadline_ts  = 이 버킷의 크레인 시작시각 − 학습된 트럭 준비시간
--     dd_slack_s            = dispatch_deadline_ts − 지금  (음수 = 이미 배차했어야 함)
--     dd_lead_s             = 실제로 뺀 준비시간(초)
--   ⚠ 기존 feasible·deadline_slack_s·feasible_crane 은 전부 그대로 둔다(정의 불변 규율).
--
-- ■ 준비시간은 학습값을 쓴다 (사용자 선택)
--   `learn_dispatch_lead`(mig 0116) = TOS 실현 선행시간 − 우리 모델 도착시간, 7일 창으로 재측정.
--   현재 값 양하 455초 / 적하 1,448초. 하드코딩 상수(450 / 1,180)와 **양하는 사실상 일치**하고
--   적하만 23% 크다. 상수는 2026-07-01 p75 측정이라 5주 전 값이고, 학습값은 선박·계절·장비 변화를
--   따라간다. 상수는 학습값이 없을 때의 폴백으로 남긴다.
--
-- ■ 판정 기준 (전환 여부를 정할 때 볼 것)
--   새 축이 옛 축보다 **실제 크레인 핸드오버 시각을 잘 맞히는가**. 구체적으로 같은 행에서
--     |dispatch_deadline_ts − (그 큐의 실제 다음 핸드오버)|  vs  |옛 마감 − 같은 값|
--   ⚠ 이 비교의 한계: 버킷 단위 추천을 컨테이너 단위 사건으로 채점하는 것이라 프록시다.
--      오늘 이 혼동으로 한 번 오독했다(철회 기록: project_work_eta_target_choice).
--
-- 멱등.

ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS dispatch_deadline_ts timestamptz;
ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS dd_slack_s           integer;
ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS dd_lead_s            integer;

COMMENT ON COLUMN stage2_match_shadow.dispatch_deadline_ts IS
  '설계 ②: 이 버킷의 크레인 시작시각 − 트럭 준비시간(learn_dispatch_lead, 없으면 LEAD_*_S 상수). '
  '"이 시각까지는 배차를 해야 한다". mig 0120 에서는 기록만 하고 판정에는 아직 안 쓴다.';
COMMENT ON COLUMN stage2_match_shadow.dd_slack_s IS
  'dispatch_deadline_ts − 추천 시각. 음수 = 이미 배차했어야 할 작업. '
  '기존 deadline_slack_s(크레인 시작 + 상한의 절반 기준)와는 정의가 다르다 — 섞어 읽지 말 것.';
COMMENT ON COLUMN stage2_match_shadow.dd_lead_s IS
  '실제로 뺀 트럭 준비시간(초). 학습값이면 learn_dispatch_lead, 폴백이면 LEAD_DS_S=450 / LEAD_LD_S=1180.';
