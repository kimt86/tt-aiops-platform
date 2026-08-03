-- 0117: `applied_bias_s` 가 보정의 **일부만** 담고 있던 것을 고치고, 전환기 혼입을 막을 판별자를 둔다.
--
-- ■ 무엇이 잘못됐나
-- 0113 이 `applied_bias_s` 를 도입한 목적은 "예측에서 보정을 빼서 **원본예측**을 복원"하는 것이었다.
-- 그런데 양하 예측에는 보정이 **두 겹**이다:
--     raw = eta_anchor + before + DS_WORK_ETA_BIAS_S(정적 상수 600초) + learned(학습항)
-- 그리고 `applied_bias_s` 에는 `learned` 만 기록됐다(workpool.rs:581). 따라서 매뷰가 복원하는
-- "원본예측"은 양하에서 여전히 +600 을 품고 있다 — 이름과 다른 값이다.
--
-- 실해(實害)는 예측 자체가 아니라 **얽힘**이다. 총 보정(600 + learned)은 옳고 상수라 진동도 없다.
-- 다만 같은 물리량(작업도달 치우침)을 손으로 튜닝한 상수와 학습항이 나눠 갖고 있어서,
--   · 학습항만 보면 실제 치우침(양하 +957초)이 아니라 그 잔여(+357초)로 보인다
--   · 상수를 건드리면 학습항이 반대로 움직여 상쇄하므로 어느 쪽도 단독으로 해석할 수 없다
--   · 그 상수는 주석 스스로 밝히듯 **유령전환 버그가 ETA 를 부풀리던 시절에 튜닝된 값**이다
--
-- ■ 조치
-- 정적 상수를 **학습항의 초기값(부트스트랩 폴백)으로 강등**한다. 학습값이 있으면 그것만 쓰고,
-- 없을 때만 상수를 쓴다. 그러면 `applied_bias_s = 실제로 적용된 보정 전부`가 되어 이름과 값이 맞고,
-- 보정의 출처가 하나가 된다.
--     learned = 매뷰값 (없으면: 양하 600 / 적하 0)
--     raw     = eta_anchor + before + learned
--     applied_bias_s = learned          ← 이제 전부
--
-- ■ ⚠ 왜 판별자(bias_ver)가 필요한가
-- `applied_bias_s` 의 **의미가 바뀐다**. 옛 행은 "학습항만", 새 행은 "보정 전부"다. 매뷰는 7일 창을
-- 쓰므로 둘이 섞이면 중앙값이 두 모집단 사이를 떠돈다 — 0113 이 정확히 이 종류의 혼입으로 틀렸다
-- (창 기준이 바뀌면 이전 잔차 측정이 이월되지 않는다).
-- 그래서 컬럼 이름을 재사용하되 **판(version)을 명시**한다. 매뷰는 bias_ver = 2 만 본다.
-- 옛 행은 지우지 않는다(운영 DB 대량 UPDATE 회피 + 나중에 두 판을 나란히 비교 가능).
--
-- ■ 전환기
-- 매뷰가 다시 빈다 → learned 가 폴백(양하 600 / 적하 0)으로 떨어진다. 적하는 현재 학습값 +1,474 를
-- 잃어 예측이 ~25분 이르게 잡히므로 **마감이 보수적(더 급하게)** 인 안전한 방향이다. 표본은 시간당
-- 수백 건이라 한두 시간이면 다시 찬다.
--
-- 멱등.

ALTER TABLE dispatch_pred_sample ADD COLUMN IF NOT EXISTS bias_ver smallint;

COMMENT ON COLUMN dispatch_pred_sample.bias_ver IS
  'applied_bias_s 의 의미 판. 2 = 적용된 보정 전부(정적 상수 포함·mig 0117). '
  'NULL/1 = 학습항만 담겨 있어 원본예측 복원이 양하에서 +600 만큼 어긋난 옛 판. 매뷰는 2 만 쓴다.';

DROP MATERIALIZED VIEW IF EXISTS learn_work_eta_bias;
CREATE MATERIALIZED VIEW learn_work_eta_bias AS
  SELECT COALESCE(qc, '')::text AS qc,
         jobtype,
         count(*)::integer      AS n,
         percentile_cont(0.5) WITHIN GROUP (
           ORDER BY EXTRACT(epoch FROM (resolved_at - (pred_work_eta_ts - make_interval(secs => applied_bias_s))))::float8
         )::integer             AS med_err_s
    FROM dispatch_pred_sample
   WHERE resolved_at IS NOT NULL
     AND jobtype IS NOT NULL
     AND resolved_src = 'qc_comp'       -- 크레인 물리 핸드오버만 (mig 0115)
     AND applied_bias_s IS NOT NULL
     AND bias_ver = 2                   -- 보정 전부가 담긴 판만 (mig 0117)
     AND logged_at > now() - interval '7 days'
     AND (pred_work_eta_ts - make_interval(secs => applied_bias_s) - logged_at)
           BETWEEN interval '5 min' AND interval '45 min'
   GROUP BY GROUPING SETS ((qc, jobtype), (jobtype))
  HAVING count(*) >= 50;

CREATE UNIQUE INDEX IF NOT EXISTS learn_work_eta_bias_pk ON learn_work_eta_bias (qc, jobtype);
