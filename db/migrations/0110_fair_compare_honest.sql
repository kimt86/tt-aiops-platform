-- 0110: 공정 비교 지표를 정직하게 만든다 — 무작위 대조군 + 중복 집계 제거.
--
-- 무엇이 잘못됐나 (2026-07-31 실측):
--   `fair_compare_shadow.savings_pct` 는 "TOS 대비 절감"으로 화면에 나가는데, **음수가 나올 수
--   없는 지표**다. 항등 순열(=TOS 가 실제로 한 배정)이 항상 실행 가능한 해이므로 최소비용
--   매칭의 비용은 정의상 그보다 작거나 같다. 실측이 이를 확인한다: 7일 1,924틱 중
--   음수 0건 · 0% 0건 · 최솟값 +0.0274%. 개별 짝에서는 우리가 지는 경우가 20%(6,868/34,075)나
--   되는데도 합계는 매번 이긴다 — 성능이 아니라 순열 자유도의 산물이다.
--   ⇒ 이 값의 정직한 이름은 "절감"이 아니라 **"TOS 배정이 최적에서 얼마나 떨어져 있나"** 다.
--
-- 이 마이그레이션이 더하는 것:
--
-- 1) rand_total_s — 같은 트럭·같은 작업·같은 순간을 **무작위로 짝지었을 때**의 비용.
--    이게 있어야 세 값을 비교할 수 있다:  무작위 ≥ TOS ≥ 최적
--    그러면 처음으로 의미 있는 질문에 답할 수 있다 — **TOS 는 이미 개선 여지의 몇 %를
--    잡고 있는가** = (무작위−TOS)/(무작위−최적). 이 값이 90% 면 우리가 더 짜낼 게 거의 없고,
--    40% 면 진짜 여지가 있다. 지금까지는 이 구분을 할 수 없었다.
--    기존 19일치 행은 NULL 로 남는다(소급 계산 불가) — 정의 변경이 아니라 컬럼 추가다.
--
-- 2) fair_compare_detail 의 중복 제거 키.
--    비교기는 5분마다 도는데 **15분 창**을 읽는다. 그래서 같은 배차가 최대 3틱에 걸쳐
--    반복 집계된다(실측: 24시간 34,075행 / 288틱 = 틱당 118.3행, 상한 120 에 91% 도달).
--    분해 분석이 같은 사건을 세 번 세면 표본 수도 가중치도 틀린다.
--    (ytno, queuename, dispatch_ts) 로 한 배차를 한 번만 남긴다 — 먼저 본 틱이 이긴다.
--    ⚠ 부분 유니크 인덱스다: 기존 행은 세 컬럼이 NULL 이라 대상에서 빠진다(소급 정리 안 함).
--
-- 멱등: db/apply.sh 가 전체 파일을 매번 실행한다.

ALTER TABLE fair_compare_shadow ADD COLUMN IF NOT EXISTS rand_total_s bigint;

COMMENT ON COLUMN fair_compare_shadow.rand_total_s IS
  '같은 풀을 무작위로 짝지었을 때의 총 공차초(8회 평균). 무작위 >= TOS >= 최적 이므로 '
  '(무작위-TOS)/(무작위-최적) = TOS 가 이미 잡고 있는 개선 여지의 비율.';

COMMENT ON COLUMN fair_compare_shadow.savings_pct IS
  '⚠"절감"이 아니다 — 정의상 음수가 될 수 없다(항등 순열이 항상 실행 가능해 최적해가 '
  '그보다 나쁠 수 없음). TOS 배정이 같은 풀의 최적에서 떨어진 거리 = 개선 여지의 상한.';

ALTER TABLE fair_compare_detail ADD COLUMN IF NOT EXISTS ytno        text;
ALTER TABLE fair_compare_detail ADD COLUMN IF NOT EXISTS queuename   text;
ALTER TABLE fair_compare_detail ADD COLUMN IF NOT EXISTS dispatch_ts timestamptz;

CREATE UNIQUE INDEX IF NOT EXISTS fair_compare_detail_dedupe_idx
  ON fair_compare_detail (ytno, queuename, dispatch_ts)
  WHERE ytno IS NOT NULL;
