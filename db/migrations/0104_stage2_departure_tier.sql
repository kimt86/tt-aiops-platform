-- 0104_stage2_departure_tier.sql
-- Stage-2 그림자에 '출항 역산' 축을 추가한다. 기존 deadline_slack_s / feasible 는 '크레인 필요
-- 시각(work-ETA)' 기준이며 정의를 절대 바꾸지 않는다 — 19일치 시계열이 있고 21일 보존이라
-- 재정의하면 두 의미가 구분자 없이 섞인다. 새 축은 반드시 새 컬럼으로만.
-- db/apply.sh가 매번 전 파일을 재실행하고 이력 테이블이 없으므로 멱등 필수. down 없음(수동 DROP).
-- nullable·DEFAULT 없음 → PG11+ 메타데이터 전용(1.34M행/299MB에도 즉시). 읽기 측 소비자 12곳이
-- 전부 명시 컬럼 SELECT(`SELECT *` 0건)이라 무영향.
ALTER TABLE stage2_match_shadow  ADD COLUMN IF NOT EXISTS dep_slack_s       int;      -- (베이 완료기한−now)−처리시간, 초. NULL=마감 없는 선박
ALTER TABLE stage2_match_shadow  ADD COLUMN IF NOT EXISTS dep_tier          smallint; -- 0=늦음 1=빠듯 2=여유/미상
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS dep_tier_on       boolean;  -- 이 틱에 티어가 정렬에 적용됐나
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS dep_tier0_n       int;      -- 캡 통과 버킷 중 티어0 수
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS dep_urgent_slots  int;      -- 최종 풀에서 티어0+1 버킷이 실제로 가져간 슬롯 수(예산 판정 카운터가 아니라 결과 집계)
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS dep_null_n        int;      -- 마감 없는 버킷 수(커버리지 경보)
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS dep_demoted_n     int;      -- 슬롯 예산에 밀려 기본(OFF) 순서로 강등된 긴급 버킷 수 = 레버가 실제로 얼마나 세게 물렸나
