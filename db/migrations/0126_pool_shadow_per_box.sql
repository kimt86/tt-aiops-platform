-- 0126: stage2_pool_shadow 가 **같은 구역의 상자들을 한 줄로 뭉개던 것**을 고친다.
--
-- ■ 무엇이 문제였나
-- PK 가 (ts, qc, vessel, queuename) 인데, 설계 ① 이후 **한 구역에 상자가 여러 줄**이다.
-- `ON CONFLICT DO NOTHING` 이라 그 구역의 첫 상자만 남고 나머지는 **말없이 버려졌다**.
-- 실측: 풀에 담긴 묶음이 23.2개인데 이 표에는 10.0개만 기록됐다(57% 유실).
-- 그 상태로 "요구 트럭 대수"를 집계해 48대라는 과소값을 얻었고, 그걸 근거로 판단할 뻔했다.
--
-- ⚠ 오늘 반복된 실수와 같은 유형이다 — 조용히 버려지는 행을 세지 않으면 집계가 거짓말을 한다.
--
-- ■ 조치
-- PK 에 slot_idx(구역 안 순번)를 넣어 상자마다 한 줄이 되게 한다. 기존 행은 slot_idx 가 없으므로
-- 표를 비우고 새로 쌓는다(진단용 표라 이력 보존 가치가 낮고, 뭉개진 이력은 어차피 못 쓴다).
--
-- 멱등.

ALTER TABLE stage2_pool_shadow ADD COLUMN IF NOT EXISTS slot_idx integer;

TRUNCATE stage2_pool_shadow;   -- 뭉개진 이력은 해석 불가 — 새 기준으로 다시 쌓는다

ALTER TABLE stage2_pool_shadow DROP CONSTRAINT IF EXISTS stage2_pool_shadow_pkey;
ALTER TABLE stage2_pool_shadow
  ADD CONSTRAINT stage2_pool_shadow_pkey PRIMARY KEY (ts, qc, vessel, queuename, slot_idx);

COMMENT ON COLUMN stage2_pool_shadow.slot_idx IS
  '구역 안 상자 순번(0-based). PK 에 포함 — 없으면 같은 구역 상자들이 한 줄로 뭉개진다(mig 0126).';
