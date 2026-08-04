-- 0124: live_workpool 에 twinkey 를 저장한다.
--
-- ■ 왜
-- 트윈은 **상자 2개·트럭 1대**다. 지금까지 트윈 합치기는 live_candidate(집계 경로)에서만 했고,
-- 상자 단위 경로가 생기면서 그 합치기가 빠졌다. 그대로 두면 트윈 한 쌍에 트럭 2대를 요구해
-- 수요가 약 8~9% 부풀어 오른다(실측 트윈 비율 양하 17.8% / 적하 14.3%).
-- twinkey 는 이미 workpool.sql 의 SELECT 에 있었는데 저장만 안 하고 있었다 — 추출 부하 0.
--
-- ⚠ 트윈 판별은 twinkey 가 권위다(twintandem 은 26.8%만 채워지고 빈값의 의미가 불명).
--
-- 멱등.

ALTER TABLE live_workpool ADD COLUMN IF NOT EXISTS twinkey text;

COMMENT ON COLUMN live_workpool.twinkey IS
  'JOB_ODR_TWINKEY — 같은 값 = 상자 2개가 트럭 1대에 실리는 트윈 쌍. '
  '트럭 수요를 셀 때 twinkey 로 합친다(상자 수가 아니라 트럭 대수가 수요다·mig 0124).';
