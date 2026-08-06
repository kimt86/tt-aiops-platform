-- 0135: 적부계획(live_stow_plan)을 델타 스트림으로 바꾸기 위한 준비.
--
-- 지금 stowplan.rs는 5분마다 VSP_SHIP 스냅샷을 통째로 받아 DELETE+INSERT 전체교체를 한다
-- (전송 6,200행×5분 ≈ 74k행/h, 이 추출기 최대 전송원). VSP_SHIP.UPD_DT에는 인덱스가 있으므로
-- (IDX_VSP_SHIP_UPD_DT, 실측 확인됨) UPD_DT 델타로 바꿀 수 있다. 이 마이그레이션은 그 병합이
-- 필요로 하는 UNIQUE 키만 만든다 — 코드 변경은 별도(stowplan.rs).
--
-- ⚠ UNIQUE 키는 기존 PK(vessel,voyage,queuename,contno)와 다르다. 델타 병합은 "이 상자가
-- 지금 어느 구역에 있는가"가 아니라 "이 상자(적하/양하)가 존재하는가"로 UPSERT/DELETE를
-- 결정해야 한다 — 계획 개정으로 queuename이 바뀌는 경우 옛 구역 행을 못 지우면 유령 행이
-- 남는다. 그래서 (vessel, voyage, contno, disload)를 무결성 키로 쓴다.
--
-- 적용 전 중복 확인(2026-08-06 실측): live_stow_plan 4,923행 중
-- (vessel,voyage,contno,disload) 중복 0건 — UNIQUE 인덱스 생성 안전.
--
-- 멱등.

CREATE UNIQUE INDEX IF NOT EXISTS live_stow_plan_key
  ON live_stow_plan (vessel, voyage, contno, disload);

COMMENT ON INDEX live_stow_plan_key IS
  '델타 병합(UPSERT/DELETE)의 무결성 키. 기존 PK(vessel,voyage,queuename,contno)는 '
  '구역 이동을 반영 못 해 델타에서 유령 행을 남긴다 — 이 키를 쓴다. mig 0135.';

-- etl_watermark 키 'stowplan_delta' 행은 코드가 첫 델타 틱에서 시딩한다(여기서 넣지 않음).
