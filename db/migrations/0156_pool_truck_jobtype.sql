-- 0156 — 후보 풀 기록에 작업유형 + 0154 문서 정정 (2026-08-21 리뷰 반영)
--
-- 왜 jobtype: 이 저장소에서 반복해서 틀린 축이 정확히 DS/LD 다(준비시간 455 vs 1,448초, 무브로그 앵커 정확도가
-- 유형마다 반대). 풀 사유별 분해(`free_tos` 13% · `inflight` 85%)를 유형으로 못 가르면 같은 함정에 다시 빠진다.
-- 값은 그 트럭의 **직전(또는 진행 중) 작업유형**(TOS 배차의 jobtype). 신규 투입처럼 유형을 모르면 NULL.
ALTER TABLE stage2_pool_truck_shadow ADD COLUMN IF NOT EXISTS jobtype text;

COMMENT ON COLUMN stage2_pool_truck_shadow.jobtype IS
  '그 트럭의 직전/진행 중 작업유형(DS/LD). 사유별 분해를 유형으로 가르기 위한 것 — 모르면 NULL(신규 투입 등).';

-- 0154 문서 정정: 코드와 어긋난 서술을 바로잡는다.
--   · pool_ver 는 3까지 왔는데 COMMENT 가 1 로 남아 있었다.
--   · pos_src 의 'drop_est' 는 코드에 존재한 적이 없다(문자열 0건).
--   · gps_age_s 의 "NULL = 픽스 없음" 은 발생 불가 — 위치를 못 찾으면 행 자체가 들어가지 않는다.
COMMENT ON TABLE stage2_pool_truck_shadow IS
  'Stage-2 후보 풀에 든 트럭(배정 여부 무관), 매 매칭 틱. 풀 재현율 측정용. 3일 보관(db.rs RETENTION 등록).
   pool_ver: 1=첫 배포(2026-08-19 12:57 MYT) · 2=픽업 가드+앵커 status 필터 제거(15:09 KST) ·
   3=리뷰 반영(적하 GPS 우선 복구·위치 나이 상한 3600s·asg 창 분리·tos_sig 실패 시 GPS 갈래 차단, 08-21 09:01) ·
   4=적하 앵커를 값에서만 미루고 풀 소속은 유지(08-21 10:30 — 3판이 커버리지까지 버려 재현율 98.7→87.7% 회귀).';
COMMENT ON COLUMN stage2_pool_truck_shadow.pool_ver IS
  '풀 규칙 판. 집계는 반드시 이 값으로 가른다 — 판이 다르면 모집단이 다르다(livemap.rs POOL_VER).';
COMMENT ON COLUMN stage2_pool_truck_shadow.pos_src IS
  'gps_live(≤120s) | gps_stale(장치 목록의 낡은 픽스) | pos_hist(truck_pos_hist 마지막 행·나이 ≤ POS_MAX_AGE_S).';
COMMENT ON COLUMN stage2_pool_truck_shadow.gps_age_s IS
  '그 틱에서 쓴 위치의 나이(초). 위치를 못 찾은 트럭은 행 자체가 없으므로 NULL 은 사실상 나오지 않는다.';

-- stage2_match_shadow.veh_state — 값 어휘가 2026-08-19 경계에서 바뀌었다(21일 보존이라 두 어휘가 섞여 있다).
-- 이 표에는 판(ver) 컬럼이 없으므로 경계를 COMMENT 로 못박는다. 집계할 때 기간으로 갈라 읽을 것.
COMMENT ON COLUMN stage2_match_shadow.veh_state IS
  '추천 시점 트럭 상태. ⚠어휘가 2026-08-19 12:57 MYT 에 바뀌었다 — 그 전: idle | soon_idle | soon_idle_anchored |
   soon_idle_held | wait_rtg. 그 뒤: 위 값들 + free_tos(원천 자유 신호로 확인된 빈 트럭) | free_gps(TOS 기록 없이
   GPS 로만 빈 차). 경계 이전의 idle 중 상당수가 이후 free_tos 로 이름이 바뀐 같은 상황이다. 기간으로 가를 것.';
