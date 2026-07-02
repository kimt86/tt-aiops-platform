-- 1단계 개선연구(ETW blend) 인에이블러 (2026-07-02, ETW probe 권고):
-- 예측 시점의 정확 ETW(TOS RPC 게이트웨이, qc_etw_utc→vessel_etw_utc, build_workpool이 이미 조인)를
-- dispatch_pred_sample에 함께 로깅해 두면, 1~2주 뒤 예측 시점의 horizon 밴드별로 "ETW vs 우리 work_eta"를
-- 제대로 비교할 수 있다. tos_etw_cntr는 매 tick upsert + 2h 롤링 삭제라 회고 비교가 원천 불가였음
-- (probe는 2h·단일 시프트 표본으로 채택/기각 확정 불가라 결론냄). 배선 판단은 이 컬럼이 쌓인 뒤에.
--
-- lock_timeout: ADD COLUMN은 메타데이터만 바꿔 즉시지만 ACCESS EXCLUSIVE를 얻어야 하고, 뜨거운
-- 테이블에서 그 대기가 뒤따르는 앱 INSERT/SELECT를 줄세워 봉쇄한다(실제 겪음: 인덱스 없는 장시간
-- 분석 SELECT 뒤에 이 ALTER가 대기하며 예측 로거를 멈춤). 2초 안에 락 못 얻으면 조용히 실패시켜
-- 앱을 절대 안 막게 한다 — 실패하면 방해 쿼리 정리 후 재실행.
SET lock_timeout = '2s';
ALTER TABLE dispatch_pred_sample ADD COLUMN IF NOT EXISTS etw_qc_ts timestamptz;
RESET lock_timeout;
