-- 0153: 매칭 틱이 **무엇 때문에 깨어났는지**를 남긴다 + workpool_age_s 대역 경계.
--
-- ■ 왜
-- 이 사이클에 매칭 틱의 깨어나는 방식을 바꿨다: 고정 초(:15)에서 **작업목록 착지 신호**로.
--   착지로 깨어난 틱  → 목록 나이 0~3초 (원하는 상태)
--   폴백으로 깨어난 틱 → 목록 나이 60초 안팎 (60초를 기다려도 새 목록이 안 온 것)
--
-- 이 둘을 `workpool_age_s` 값으로 갈라 읽으면 안 된다. "나이가 크면 폴백"으로 가르는 것은
-- **결과에서 파생된 변수로 층화**하는 것이라, 그 뒤에 "착지 틱은 나이가 작다"고 보고하면
-- 동어반복이다. 이 저장소가 2026-08-03 에 같은 함정으로 한 번 크게 틀렸다
-- (`project_work_eta_target_choice`: 잔차를 잔차의 부호로 갈랐다).
-- ⇒ 깨어난 이유를 **원천에서** 적는다.
--
-- ■ 값
--   'landing'  — data_freshness(WORKPOOL).last_success_at 이 앞으로 가서 깨어남
--   'fallback' — 최대 대기(60초)를 채워 깨어남. 목록은 직전 틱과 같다.
--   'startup'  — 프로세스 기동 직후 첫 회. 기준선이 없어 지금 있는 목록으로 한 번 돈다.
--   NULL       — 경계 이전(고정 위상 :15) 행
--
-- ⚠ 'startup' 을 따로 둔 이유: 이 틱의 목록 나이는 **재시작한 순간에 달린 임의값**(0~60초)
--    이다. 'landing' 에 섞으면 재배포 한 번마다 p99 가 오염된다. 재배포마다 1행씩만 생긴다.
--
-- ⚠ 'fallback' 비율에는 **경보를 걸지 않는다.** 이 값은 "추출이 분당 한 번을 못 따라온 비율"
--    이고, 2026-08-12 실측으로 tt-workpool 실행 소요가 p90 65초·최대 84초라 0 이 아닐 것으로
--    예상된다. 임계는 정상 대역이 며칠 쌓인 뒤에 정한다.
--
-- ■ 비용
-- 컬럼 하나 + 매 틱 바인딩 하나. 대기 루프의 마지막 신선도 조회 결과를 그대로 재사용하므로
-- 틱당 추가 질의는 없다.

ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS wake_src text;

COMMENT ON COLUMN stage2_solver_shadow.wake_src IS
  '매칭 틱이 깨어난 이유. landing=작업목록 착지 신호(data_freshness(WORKPOOL).last_success_at 전진) / '
  'fallback=최대 대기 60초 소진(목록 그대로) / startup=프로세스 기동 직후 첫 회(나이가 임의라 landing 에 섞지 말 것) / '
  'NULL=2026-08-12 경계 이전(고정 위상 :15). '
  'workpool_age_s 를 집계할 때는 반드시 이 컬럼으로 먼저 가른다 — 나이로 이유를 추정하면 동어반복이다.';

-- workpool_age_s 의 정상 대역은 이 사이클에 바뀌었다. 판(ver) 컬럼을 새로 두지 않는 이유:
-- 컬럼의 **의미**("매칭이 실제로 쓴 목록의 나이")는 그대로고 분포만 바뀌었다. 가르는 축은
-- 위의 wake_src 다.
COMMENT ON COLUMN stage2_solver_shadow.workpool_age_s IS
  '매칭이 실제로 쓴 작업목록의 나이(초) = now() - data_freshness(WORKPOOL).last_success_at. '
  '대역 경계: NULL=2026-08-11 mig0150 이전(미기록) / 6~15초=고정 위상 :15 구간(2026-08-11~08-12) / '
  '0~3초=착지 신호로 깨우는 구간(2026-08-12~, wake_src=landing · fallback 은 60초 안팎이 정상). '
  '⚠ mig0150 머리말의 "MATCH_TICK_SEC 만 바꾸고 타이머를 안 맞춘다" 는 낡은 서술이다 — '
  'MATCH_TICK_SEC 도 tt-workpool.timer 와의 짝 관계도 이 사이클에 사라졌다.';
