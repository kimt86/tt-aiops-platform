-- 0134: KPI 병산(parity) 로그. Oracle 경로는 그대로 권위값을 쓴다 — 이 표는 로컬 계산을
-- 나란히 기록만 한다(절체 아님). PLAN-extractor.md CHUNK2.
-- ⚠ 번호 조정: 계획은 "0133"으로 지정했으나 다른 에이전트가 동시에 0133을 이미 사용함
-- (0133_retire_legacy_pool_columns.sql, workpool 담당) → 다음 빈 번호 0134로 옮김.
CREATE TABLE IF NOT EXISTS kpi_parity_log (
  kpi_key       TEXT NOT NULL,
  business_date DATE NOT NULL,
  shift         TEXT NOT NULL,           -- 'N'|'D'|'E' (shift.rs 라벨)
  src           TEXT NOT NULL,           -- 'oracle' | 'local'
  value         DOUBLE PRECISION,
  sample_n      BIGINT,
  computed_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS kpi_parity_lookup ON kpi_parity_log (kpi_key, business_date, shift, src, computed_at);
