-- 0145: live_stow_plan 행 정체성을 무결성 키 하나로 통일한다.
--
-- 이 표에는 유니크 제약이 둘 있었다: 옛 PK(vessel,voyage,queuename,contno)와 mig0135 의
-- 무결성 키(vessel,voyage,contno,disload). 쓰기 경로가 어느 한쪽을 ON CONFLICT 대상으로
-- 잡아도 **다른 쪽** 제약이 같은 문장에서 터질 수 있다 — 실제로 스냅샷·recon 경로는 옛 PK 를,
-- 델타 경로는 무결성 키를 겨냥하고 있어서, Oracle 이 같은 상자·같은 방향을 두 구역으로
-- 동시에 반환하는 순간 그 트랜잭션 전체가 유니크 위반으로 롤백된다(피해 = 치유 1시간 지연,
-- 2026-08-10 최종 점검에서 잠재 엣지로 보고·승인된 수리).
--
-- 행 정체성은 mig0135 가 정한 대로 (vessel, voyage, contno, disload) 하나면 충분하다 —
-- 구역(queuename)은 계획 개정으로 움직이는 **속성**이지 정체성이 아니다. 조회는
-- live_stow_plan_q_idx(vessel,voyage,queuename,planseq)가 그대로 받는다(api 소비자는
-- 이 축으로만 읽는다 — crates/api/src/workpool.rs 의 pp CTE). FK 참조 없음.
--
-- ⚠ 적용 순서: 새 바이너리(쓰기 3경로 전부 무결성 키 겨냥) 배포·검증 **후에** 이 파일을
--   적용한다. 옛 바이너리의 스냅샷·recon 은 PK 를 ON CONFLICT 대상으로 요구하므로
--   PK 를 먼저 떨어뜨리면 그 경로가 "no unique constraint matching" 으로 죽는다.
--
-- 멱등.

ALTER TABLE live_stow_plan DROP CONSTRAINT IF EXISTS live_stow_plan_pkey;

COMMENT ON INDEX live_stow_plan_key IS
  '행 정체성(유일한 유니크 키). 델타·스냅샷·recon 세 쓰기 경로가 전부 이 키를 ON CONFLICT '
  '대상으로 쓴다. 옛 PK(vessel,voyage,queuename,contno)는 mig0145 에서 제거 — 구역은 개정으로 '
  '움직이는 속성이라 정체성이 될 수 없고, 유니크 제약 둘이 공존하면 어느 쪽을 겨냥한 UPSERT 도 '
  '다른 쪽 위반으로 구를 수 있다.';
