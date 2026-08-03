-- 0116: 실행가능 판정(feasible)이 **서로 다른 두 지점의 시각을 비교**하던 것을 고친다.
--
-- ■ 무엇이 잘못됐나 (2026-08-03 실측)
-- Stage-2 그림자는 각 추천에 대해 이렇게 채점한다:
--     feasible = (지금 + 트럭의 p90 도착시간) <= 마감
-- 그런데 두 항의 **도착 지점이 다르다**:
--     · `arrival_s` = 트럭이 **픽업 지점**에 닿는 시간  (livemap.rs:4462)
--        - 양하(DS): 픽업 지점 = QC(안벽)      ← 마감과 같은 지점
--        - 적하(LD): 픽업 지점 = **야드 블록** ← 마감과 다른 지점!
--     · `마감`     = work_eta_ts = **QC(안벽)** 가 그 컨테이너를 다루는 시각
-- 즉 적하는 "야드 블록 도착 시간"을 "안벽 크레인 필요 시각"과 비교한다. 그 사이의
-- **RTG 상차 + 적재 주행 + 안벽 큐**가 통째로 빠져 있다.
--
-- ■ 실측 (최근 24시간)
--   TOS 실현 선행시간 = qc_move_log.dur_s (배차 → 크레인 핸드오버 완료) 중앙
--       양하 464초 · 적하 **1,536초**
--   우리 모델 도착시간 = stage2_match_shadow.arrival_s (픽업 지점까지 p50) 중앙
--       양하 358초 · 적하  357초
--   차이(= 우리가 안 세는 시간)
--       양하 **+106초** · 적하 **+1,179초**
--   ⇒ 양하가 106초로 작게 나오는 것이 이 산식이 옳다는 증거다(픽업 지점 = 크레인이라 빠진 구간이
--     없어야 하고, 실제로 없다). 적하만 20분이 빈다 — 정확히 빠진 적재 구간이다.
--   대조 실측: tt_move_log 사이클 분해 중앙 적하 = 공차 554초 + **적재 789초** = 1,536초.
--
-- ■ 이게 만든 증상
--   적하 실행가능률 30.9% (양하 89.4%). 그런데 실제 크레인 기아는 6~16%뿐이다 —
--   지표가 현장과 4~5배 어긋나 있었다. 마감까지 남은 시간 중앙이 적하 330초인데 도착 추정이
--   468초라 **구조적으로 불가능**했다. 없는 구간을 안 세니 남은 시간이 실제보다 짧아 보인 것이다.
--
-- ■ 왜 OD 분해가 아니라 학습 상수인가
--   "적재 구간 = 도로망 경로(블록→QC)"로 분해하고 싶지만, 실측이 그걸 기각한다. 적하 1,536초 중
--   순수 주행으로 설명 가능한 건 절반이 안 된다(터미널 반경 3.2km·구내 속도). 나머지는 **큐**다
--   (RTG 대기 ~200초 + 안벽 대기 ~440초). 그리고 ADR 0002(kc/dispatch/leadtime-adr.html)가
--   이미 확정했다: **개별 큐 시간은 관측 신호로 예측 불가**, 중앙값 + 보수적 p90 으로만 쓴다.
--   ⇒ 그래서 경로로 쪼개지 않고, **실현 선행시간과 우리 모델의 차이를 그대로 학습**한다.
--      우리 주행 모델이 바뀌면 차이도 따라 움직이므로 자가보정된다(road_route_eval 과 같은 패턴).
--
-- ■ ⚠ 이 산식의 한계 (알고 채택한다)
--   두 항의 모집단이 다르다. `dur_s`는 TOS 가 고른 배차의 실현값이고, `arrival_s`는 **우리가
--   비용 최소로 고른** 짝의 추정값이다. 우리가 더 가까운 트럭을 고르는 만큼 차이가 부풀 수 있다.
--   적하는 빠진 구간(20분)이 압도적이라 이 효과가 묻히지만, **양하의 106초는 상당 부분 이 선택
--   효과일 수 있다**. 그럼에도 작업유형별로 다른 산식을 쓰지 않는다 — 0113 사고가 정확히
--   "같은 예측을 유형별로 다르게 다루다" 생긴 것이라, 비대칭 특례를 다시 만들지 않는다.
--   106초는 보수적(더 급하게) 방향이라 안전한 쪽이기도 하다.
--
-- ■ ⚠ 비용 행렬은 건드리지 않는다
--   이 값은 **실행가능/여유 판정에만** 들어간다. Stage-2 간선 비용은 설계상 순수 공차주행이며
--   (긴급도·기아·부하분산 배제), 적재 구간은 낭비가 아니라 생산적 작업이라 비용에 넣으면
--   야드 트럭을 안벽으로 내모는 역효과가 난다. livemap.rs:4686 주석 참조.
--
-- ■ ⚠ 기존 feasible / deadline_slack_s 는 **건드리지 않는다**
--   livemap.rs:4768 이 못 박아 둔 규율이다: 그 두 컬럼은 19일치 시계열이 쌓여 있고 보존이 21일이라,
--   같은 이름으로 정의를 바꾸면 구분자 없이 두 의미가 섞인다. 그래서 **새 축을 컬럼으로 추가**한다
--   (출항 마감 축을 dep_slack_s/dep_tier 로 추가했던 것과 같은 방식).
--     lead_extra_s   = 이 추천에 적용된 보정(초)
--     crane_slack_s  = 마감 − (지금 + p90 도착 + 보정)   ← 크레인 기준 진짜 여유
--     feasible_crane = crane_slack_s >= 0
--   두 축을 나란히 두면 전환 시점을 지나서도 옛 지표와 새 지표를 같은 행에서 비교할 수 있다.
--
-- 멱등.

ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS lead_extra_s   integer;
ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS crane_slack_s  integer;
ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS feasible_crane boolean;

COMMENT ON COLUMN stage2_match_shadow.lead_extra_s IS
  'learn_dispatch_lead.extra_s — 픽업 지점 도착 이후 크레인 핸드오버까지 우리 주행 모델이 세지 않는 시간.';
COMMENT ON COLUMN stage2_match_shadow.crane_slack_s IS
  '마감 − (지금 + p90 도착 + lead_extra_s). 크레인 기준 여유. '
  '기존 deadline_slack_s 는 적하에서 야드 블록 도착만 세던 옛 정의라 그대로 보존한다(정의 불변).';
COMMENT ON COLUMN stage2_match_shadow.feasible_crane IS
  'crane_slack_s >= 0. 기존 feasible 의 대체가 아니라 병행 축이다(mig 0116).';

DROP MATERIALIZED VIEW IF EXISTS learn_dispatch_lead;
CREATE MATERIALIZED VIEW learn_dispatch_lead AS
  WITH realized AS (
    -- TOS 가 실제로 쓴 선행시간: 배차(st_ts) → 크레인↔트럭 핸드오버 완료(comp_ts).
    -- ⚠ st_ts 를 '크레인 시작'으로 읽지 말 것 — 배차 시각이다(mig 0115 참조). 여기서는
    --    바로 그 성질 때문에 쓴다: comp_ts − st_ts = 배차부터 핸드오버까지 = 재려는 그 구간.
    --
    -- ⚠ 저장된 `dur_s` 를 쓰지 않는다. 추출기(crates/extractor/src/qc_moves.rs)가 dur_s 를
    --    0..3600 초로 잘라 NULL 로 만드는데, st_ts 가 배차시각이라 적하는 그 상한을 정상적으로
    --    넘는다 — 실측 최근 24h 적하 **9.32%가 잘려 나갔고** 그 절단이 중앙값을 1,654 → 1,530 초로
    --    **124초 과소**하게 만든다. 잘린 쪽이 전부 '오래 걸린 무브'라 편향이 한 방향이다.
    --    원본 두 컬럼은 그대로 남아 있으므로 여기서 직접 뺀다(추출기 상한도 함께 올렸다).
    SELECT jobtype,
           percentile_cont(0.5) WITHIN GROUP (
             ORDER BY EXTRACT(epoch FROM (comp_ts - st_ts))
           )::float8 AS lead_s,
           count(*)::integer AS n_realized
      FROM qc_move_log
     WHERE comp_ts > now() - interval '7 days'
       AND st_ts IS NOT NULL
       AND comp_ts - st_ts BETWEEN interval '0' AND interval '2 hours'  -- 초과는 교대 걸침/이상치
     GROUP BY 1
  ), modeled AS (
    -- 우리 모델이 세는 시간: 픽업 지점까지의 p50 도착 추정.
    SELECT jobtype,
           percentile_cont(0.5) WITHIN GROUP (ORDER BY arrival_s)::float8 AS arr_s,
           count(*)::integer AS n_modeled
      FROM stage2_match_shadow
     WHERE ts > now() - interval '7 days'
       AND arrival_s IS NOT NULL
       AND jobtype IS NOT NULL
     GROUP BY 1
  )
  SELECT r.jobtype,
         r.n_realized,
         m.n_modeled,
         round(r.lead_s)::integer AS realized_lead_s,
         round(m.arr_s)::integer  AS modeled_arrival_s,
         -- 상한 2400초: 이 항은 마감을 앞당기는 방향이라 폭주하면 전부 실행불가가 된다.
         -- 실측 최대(적하 1,179초)의 2배로 잡아 정상 변동은 통과시키고 폭주만 막는다.
         LEAST(2400, GREATEST(0, round(r.lead_s - m.arr_s)))::integer AS extra_s
    FROM realized r
    JOIN modeled  m USING (jobtype)
   WHERE r.n_realized >= 200 AND m.n_modeled >= 200;

CREATE UNIQUE INDEX IF NOT EXISTS learn_dispatch_lead_pk ON learn_dispatch_lead (jobtype);

COMMENT ON MATERIALIZED VIEW learn_dispatch_lead IS
  '픽업 지점 도착 이후 크레인 핸드오버까지 우리 모델이 세지 않는 시간(초). '
  '실행가능/여유 판정에만 쓴다 — Stage-2 비용 행렬에는 넣지 않는다(순수 공차주행 규율). '
  '적하는 적재 구간이 통째로 빠져 있어 ~1,180초, 양하는 ~106초.';
