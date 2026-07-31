-- 0111: 배차 레버를 판정할 수 있게 만드는 A/B 하네스 기록 컬럼.
--
-- 왜 필요한가:
--   출항 마감 티어는 2026-07-27 부터 **모든 틱에서 ON** 이고, 그 이전 틱에는 관련 값이 아예
--   비어 있다. 즉 같은 조건의 반사실(counterfactual)이 존재하지 않는다 — 지금 이 레버가
--   도움이 됐는지 해가 됐는지 **말할 방법이 없다**. 측정 없는 개선은 개선이 아니므로,
--   이후 어떤 배차 개선도 이 하네스가 먼저다.
--
-- 무엇을 기록하나:
--
--  ab_block  — 30분 블록 번호. `STAGE2_DEP_TIER=ab` 일 때 레버 on/off 를 **블록 단위로 무작위**
--              배정하고 그 블록 번호를 남긴다. 틱 단위 교대가 아니라 블록인 이유:
--              anti-thrash 의 '직전 추천'(prev)이 틱을 넘어 이어지므로, 매 틱 팔을 바꾸면 두 팔이
--              서로의 잔상에 오염돼 "항상 ON vs 항상 OFF" 를 근사하지 못한다. 블록이면 각 팔이
--              연속 구간을 갖는다. 항상 ON/OFF 모드에서는 NULL 이라, 분석이 실수로 단일 팔
--              구간을 대조군으로 쓰는 일이 없다.
--
--  ab_warmup — 블록 앞부분(기본 3분) 표시. 팔이 막 바뀐 직후 틱은 이전 팔이 남긴 prev 때문에
--              오염돼 있다. 버리지 않고 표시만 해서, 분석이 제외할지 포함할지 고를 수 있게 한다
--              (조용히 버리면 표본이 왜 줄었는지 나중에 아무도 모른다).
--
--  works_raw — 트럭 수에 맞춰 자르기 **전**의 작업 버킷 수. 지금은 자른 뒤 값(n_works)만 남아
--              "수요가 적어서 적게 배정한 것"과 "트럭이 모자라 잘린 것"을 구분할 수 없다.
--              두 팔을 비교하려면 이 구분이 필수다 — 트럭이 모자란 구간에서는 레버가 무엇을
--              먼저 담느냐가 전부이고, 수요가 적은 구간에서는 레버가 아무 일도 하지 않는다.
--
-- 전부 nullable · 기본값 없음(PG11+ 에서 메타데이터만 바뀌는 즉시 연산). 기존 21일치 시계열의
-- 의미를 바꾸지 않는다 — 새 축은 새 컬럼으로만 추가한다(0104 와 같은 규칙).
--
-- 멱등: db/apply.sh 가 전체 파일을 매번 실행한다.

ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS ab_block  bigint;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS ab_warmup boolean;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS works_raw integer;

COMMENT ON COLUMN stage2_solver_shadow.ab_block IS
  'A/B 30분 블록 번호(STAGE2_DEP_TIER=ab 일 때만; 그 외 NULL). 같은 블록의 틱은 같은 팔이다.';
COMMENT ON COLUMN stage2_solver_shadow.ab_warmup IS
  '블록 앞 3분 = 직전 팔의 anti-thrash 잔상이 남은 구간. 분석에서 제외 여부를 고를 수 있게 표시만 한다.';
COMMENT ON COLUMN stage2_solver_shadow.works_raw IS
  '트럭 수 캡 적용 전 작업 버킷 수. n_works 와의 차이가 "트럭이 모자라 잘린 양".';

CREATE INDEX IF NOT EXISTS stage2_solver_shadow_ab_idx
  ON stage2_solver_shadow (ab_block, dep_tier_on) WHERE ab_block IS NOT NULL;
