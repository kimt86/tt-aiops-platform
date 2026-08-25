-- 0159: 순간 비교(T1)에 '실제 추천 트럭' 성적 추가 (2026-08-25 사용자 지시).
--
-- 배경: dispatch_compare_shadow 의 our_ytno 는 우리 제품(추천 스트림)이 아니라 채점용으로
-- 사후에 새로 계산한 '그 순간 최근접 가용 트럭'이다(모든 배차를 빠짐없이 비교하기 위한
-- 상한 계기 — 코드 주석 "NOT from our advisory" 참조). 현장 투입 판단에 필요한 것은
-- "운영자가 그 순간 보드를 그대로 따라 했다면"의 성적이라, 실제 추천(stage2_match_shadow)을
-- T1 기준으로 되감아 같은 자(도로망 OD)로 채점한 컬럼을 병기한다. 기존 컬럼 의미 불변.
ALTER TABLE dispatch_compare_shadow ADD COLUMN IF NOT EXISTS reco_ytno text;
ALTER TABLE dispatch_compare_shadow ADD COLUMN IF NOT EXISTS reco_ts timestamptz;
ALTER TABLE dispatch_compare_shadow ADD COLUMN IF NOT EXISTS reco_src text;
ALTER TABLE dispatch_compare_shadow ADD COLUMN IF NOT EXISTS reco_arrival_s integer;
ALTER TABLE dispatch_compare_shadow ADD COLUMN IF NOT EXISTS reco_delta_s integer;
ALTER TABLE dispatch_compare_shadow ADD COLUMN IF NOT EXISTS reco_free boolean;
COMMENT ON COLUMN dispatch_compare_shadow.reco_ytno IS
  'T1(배차 시각)에 보드에 떠 있던 우리 실제 추천 트럭(150초 이내 최신 — BoardPage STALE_S 와 같은 기준). '
  'NULL 의미는 reco_src 로 가른다. 2026-08-25(mig 0159)부터 기록, 이전 행은 NULL.';
COMMENT ON COLUMN dispatch_compare_shadow.reco_src IS
  '추천을 찾은 방법: cont=같은 상자 · queue=같은 (qc,queuename) · none=평가 가능했으나 그 순간 '
  '이 작업에 유효 추천 없음(발행 전/낡음 — 이것도 성적의 일부) · NULL=평가 불능(T1 이 조회 창 밖·경계 이전).';
COMMENT ON COLUMN dispatch_compare_shadow.reco_arrival_s IS
  '추천 트럭의 T1 위치 기준 도착초(비교기와 같은 OD 자). NULL=트럭 위치 없음.';
COMMENT ON COLUMN dispatch_compare_shadow.reco_delta_s IS
  'tos_arrival_s − reco_arrival_s (+ = 우리 추천이 더 빠름). 둘 다 있을 때만.';
COMMENT ON COLUMN dispatch_compare_shadow.reco_free IS
  'T1 에 추천 트럭이 가용 상태(idle/soon_idle/wait_rtg)였나 — 아니면 그 트럭은 아직 일하는 중이라 '
  '단순 주행시간(reco_arrival_s)이 실제보다 후하게 나온다. 층화용.';
