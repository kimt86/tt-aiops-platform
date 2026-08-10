-- 0146: qc_move_log.st_ts / dur_s 의 정체를 컬럼 주석으로 박제한다 (재발 방지).
--
-- 배경(2026-08-10 확정): TOS MCH_OPERATION 의 ST_DT 는 QC 쪽에서 크레인 작업 시작이
-- 아니라 **트럭 배정 시각**(완료 시 소급 기입)이다. 이 이름 때문에 K_QC_Q·K_UTIL_CRANE
-- 산식이 이를 크레인 시작으로 오용해 굶김 1/10 과소·가동률 ~2배 과대가 실측됐고,
-- 같은 날 산식을 추정 물리 시작으로 절체했다. 값 자체는 거울 원형이므로 그대로 두고
-- (거울은 거울), 의미를 주석으로 고정한다. 컬럼명 교체(dispatch_ts 류)는 소비자 이관
-- 완료 후의 별도 단계.
--
-- 멱등(COMMENT 는 덮어쓰기).

COMMENT ON COLUMN qc_move_log.st_ts IS
  '⚠ 크레인 작업 시작이 아니다. TOS ST_DT 의 원형 사본 = 트럭 배정 시각(완료 시 소급 기입). '
  '완료보다 양하 중앙 ~7분·적하 중앙 ~24분 이르다(= 트럭 준비시간, 2026-08-10 실측 4.6만건). '
  '크레인 작업 구간이 필요하면 추정 시작(greatest(직전 들어올림 완료, comp_ts − '
  'learn_qc_move_time.med_sec))을 쓸 것 — sql/local/l_qc_q.sql 참조. '
  'rtg_move_log 의 동명 컬럼은 반대로 진짜 물리 시작이니 혼동 금지. mig0146.';

COMMENT ON COLUMN qc_move_log.dur_s IS
  '⚠ 크레인 무브 소요시간이 아니다. comp_ts − st_ts = 트럭 배정→완료(적하 중앙 ~24분). '
  '무브 시간이 필요하면 learn_qc_move_time 을 쓸 것. rtg_move_log.dur_s 는 진짜 작업 '
  '소요이니 혼동 금지. mig0146.';

COMMENT ON COLUMN rtg_move_log.st_ts IS
  'RTG/ES 의 진짜 물리 시작(구간 겹침 1.3% 실측 — 직렬). qc_move_log 의 동명 컬럼(트럭 '
  '배정 시각)과 의미가 다르다. mig0146.';
