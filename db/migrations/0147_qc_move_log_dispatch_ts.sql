-- 0147: qc_move_log.st_ts → dispatch_ts 개명 + dur_s 채움 중단 (사용자 승인 2026-08-10).
--
-- 개명 이유: "st(start)"라는 이름이 반복 사고의 뿌리였다 — mig0113 오독, K_QC_Q·가동률
-- 오염(같은 날 절체), scengen service_s 오염까지 전부 이 이름에서 시작했다. 값의 정체는
-- 트럭 배정 시각(TOS ST_DT, 완료 시 소급 기입)이므로 이름을 값에 맞춘다.
--
-- 안전 근거(2026-08-10 전수 확인):
-- - 코드 실사용은 작성기(crates/extractor/src/qc_moves.rs)와 분석 스크립트 2본뿐
--   (scripts/deadline_axis_compare.sql·need_horizon_ab_report.sql) — 같은 커밋에서 갱신.
-- - KPI 3본·scengen 은 같은 날 추정 시작으로 절체돼 이 컬럼을 더 안 읽는다.
-- - 매트뷰(learn_dispatch_lead 등 3개)는 컬럼을 attnum 으로 참조해 개명에 자동 추종 —
--   재생성 불필요(적용 후 REFRESH 1회로 실증).
-- - crates/api 는 주석 언급뿐, 코드 참조 0.
-- - ⚠ rtg_move_log.st_ts 는 개명하지 않는다 — 그쪽은 진짜 물리 시작이라 이름이 맞다.
-- - ⚠ 옛 마이그레이션(0087·0113·0115·0116·0146)은 st_ts 텍스트를 담고 있어 이 파일 적용
--   후에는 재적용 불가(이력 파일로만 유효). 새 배포는 이 파일까지 순서 적용이 전제.
--
-- dur_s 채움 중단: '무브 소요'처럼 읽히는 이름에 배정→완료 리드(적하 중앙 ~24분)가 들어
-- 있었다. 소비자 0 확정(learn_dispatch_lead 는 두 컬럼 직접 차감·scengen 은 이관) →
-- 저장소 전례(NULL 정지 + COMMENT)대로 새 행은 NULL, 과거 행 값은 기록으로 보존.
--
-- 멱등.

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM information_schema.columns
              WHERE table_schema = 'public' AND table_name = 'qc_move_log'
                AND column_name = 'st_ts') THEN
    ALTER TABLE qc_move_log RENAME COLUMN st_ts TO dispatch_ts;
  END IF;
END $$;

COMMENT ON COLUMN qc_move_log.dispatch_ts IS
  '트럭 배정 시각(TOS MCH_OPERATION.ST_DT 원형 사본 — 완료 시 소급 기입). 구명 st_ts 를 '
  'mig0147 에서 개명: "시작"으로 읽혀 크레인 물리 시작으로 오용되는 사고가 반복됐다(mig0113·'
  'K_QC_Q·가동률·scengen — 전부 2026-08-10 절체). 완료보다 양하 중앙 ~7분·적하 ~24분 이르다'
  '(=트럭 준비시간). 크레인 작업 구간이 필요하면 추정 시작(sql/local/l_qc_q.sql)을 쓸 것. '
  'rtg_move_log.st_ts 는 반대로 진짜 물리 시작이라 그대로다 — 혼동 금지.';

COMMENT ON COLUMN qc_move_log.dur_s IS
  '⚠ 2026-08-10(mig0147)부터 채움 중단(신규 행 NULL). 과거 행 값 = comp_ts − 배정시각(구 st_ts) '
  '= 배정→완료 리드(크레인 무브 소요 아님·적하 중앙 ~24분). 배정 리드가 필요하면 '
  'comp_ts − dispatch_ts 를 직접 뺄 것(learn_dispatch_lead 가 그렇게 한다). 무브 시간이 '
  '필요하면 learn_qc_move_time.';
