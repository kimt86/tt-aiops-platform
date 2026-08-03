-- 0118: 작업도달 보정을 **예측 거리(horizon)별**로 학습한다. 지금은 상수 하나가 2,768초 범위의
--       구조를 덮고 있다.
--
-- ■ 진단 (2026-08-03 실측, 정답지 = qc_move_log.comp_ts·mig 0115, bias_ver=2, 최근 10시간)
-- 오차를 "원본예측이 얼마나 먼 미래를 가리켰나"로 층화하면 두 가지가 동시에 보인다.
--
--   예측 거리   적하 치우침   적하 IQR    양하 치우침   양하 IQR
--    0~10분      +1,548초     1,783초      +547초      1,510초
--   10~20분      +2,145초     1,674초      +736초      1,672초
--   20~30분      +1,727초     1,400초      +412초      1,470초
--   30~40분      +1,078초     1,384초      +115초      1,367초
--   40~50분        +616초     1,119초      +484초      1,178초
--   50~60분        −623초     1,307초         —           —
--
-- ① **퍼짐은 평평하다.** IQR 이 5분 앞을 볼 때나 50분 앞을 볼 때나 ~1,400초로 같다. 오차가
--    "무브마다 조금씩 틀리는 것이 쌓여서" 생긴다면 거리에 따라 퍼짐이 커져야 하는데 안 커진다.
--    ⇒ 무브 시간 상수(DS_MOVE_S=90 / LD_MOVE_S=110)는 범인이 아니다. 실측 크레인 사이클도
--      양하 99초 / 적하 126초로 상수와 큰 차이가 없다. 여기를 손봐야 얻는 게 없다.
--
-- ② **치우침은 거리에 따라 크게 움직인다.** 적하는 가까울수록 크게 낙관적이고(+2,145초),
--    멀어지면 오히려 비관으로 넘어간다(−623초). 즉 큐 앞머리에 모델이 안 세는 지연이 있고,
--    그 지연은 멀리 갈수록 상대적으로 희석된다. 물리적으로 그럴듯하다 —
--      · 크레인의 **진행 중인 작업**이 베이 앵커에 안 들어간다(코드가 이미 아는 사실)
--      · 적하는 크레인이 **트럭을 기다려야** 진행한다. 모델은 크레인이 쉬지 않고 자기 카덴스로
--        돈다고 가정하므로, 트럭에 막히는 앞머리에서 가장 크게 낙관한다(실현 선행 1,448초와 자릿수가 같다).
--
-- ■ 지금 구조가 왜 이걸 못 잡나
-- learn_work_eta_bias 는 (크레인, 작업유형)당 **숫자 하나**를 낸다. 위 표의 +2,145 ~ −623 을
-- 하나의 중앙값으로 뭉개면, 가까운 예측은 여전히 낙관이고 먼 예측은 과보정된다. 그리고 배차가
-- 실제로 보는 구간이 5~45분이라 이 왜곡이 그대로 마감에 들어간다.
--
-- ■ 조치 — 보정에 거리 축을 추가한다
-- 10분 단위 버킷(0: 0~10분 … 4: 40분+)을 키에 넣는다. 조회 우선순위:
--     ① (크레인, 작업유형, 버킷)  n>=150
--     ② (      , 작업유형, 버킷)  n>=100   ← 실질적으로 이게 일한다
--     ③ (      , 작업유형, 전체)  n>=100   ← 종전 동작(전환기·표본부족 시 폴백)
-- 버킷은 **원본예측 기준 거리**로 자른다. 보정된 값으로 자르면 보정이 커질수록 같은 예측이 다른
-- 버킷으로 이동해 되먹임이 생긴다(0113 이 표본 창에서 저지른 것과 같은 종류의 오류).
--
-- ⚠ 버킷 경계의 미세 불일치(알고 넘어간다): 코드는 `before`(크레인 앞 작업 초)로 버킷을 고르고
--   매뷰는 `(pred − applied_bias) − logged_at`으로 자른다. 둘은 **교대정지 보정(brk)** 만큼 다르다
--   (경계당 500초·하루 3회). 10분 버킷에서 경계 근처 일부 행이 한 칸 밀릴 수 있다. 맞추려면 brk 를
--   따로 저장해야 하는데, brk 는 raw 로부터 계산되고 raw 는 학습항에 의존하므로 버킷 선택에 넣으면
--   순환이 된다. 영향이 작아 근사를 택했다.
--
-- ⚠ 이건 치우침만 걷어낸다. 남는 ±1,400초(IQR) 는 이 마이그레이션이 건드리지 못하고, ADR 0002
--   (kc/dispatch/leadtime-adr.html)가 이미 확정했듯 관측 가능한 신호로는 못 줄인다 — 대기를 만드는
--   크레인 큐가 우리 신호에 안 보인다. 그래서 마감 판정은 계속 보수적 p90 으로 방어한다.
--
-- 멱등.

DROP MATERIALIZED VIEW IF EXISTS learn_work_eta_bias;
CREATE MATERIALIZED VIEW learn_work_eta_bias AS
  WITH s AS (
    SELECT COALESCE(qc, '')::text AS qc,
           jobtype,
           -- 원본예측 기준 거리 → 10분 버킷(0..4). 보정된 값으로 자르면 되먹임이 생긴다.
           LEAST(4, GREATEST(0, floor(
             EXTRACT(epoch FROM (pred_work_eta_ts - make_interval(secs => applied_bias_s) - logged_at)) / 600
           )))::integer AS horizon_bucket,
           EXTRACT(epoch FROM (resolved_at - (pred_work_eta_ts - make_interval(secs => applied_bias_s))))::float8 AS err_s
      FROM dispatch_pred_sample
     WHERE resolved_at IS NOT NULL
       AND jobtype IS NOT NULL
       AND resolved_src = 'qc_comp'       -- 크레인 물리 핸드오버만 (mig 0115)
       AND applied_bias_s IS NOT NULL
       AND bias_ver = 2                   -- 보정 전부가 담긴 판만 (mig 0117)
       AND logged_at > now() - interval '7 days'
       AND (pred_work_eta_ts - make_interval(secs => applied_bias_s) - logged_at)
             BETWEEN interval '5 min' AND interval '45 min'
  )
  -- ⚠ GROUPING SETS 에서 그룹화되지 않은 컬럼은 출력에서 NULL 이 된다. CTE 안에서 이미
  --   COALESCE 했더라도 바깥에서 다시 해야 한다(안 하면 유니크 인덱스가 NULL 로 깨진다).
  SELECT COALESCE(qc, '')::text AS qc, jobtype,
         -- -1 = 거리 무관(종전 동작). 코드가 버킷 → 전체 순으로 떨어진다.
         COALESCE(horizon_bucket, -1) AS horizon_bucket,
         count(*)::integer AS n,
         percentile_cont(0.5) WITHIN GROUP (ORDER BY err_s)::integer AS med_err_s
    FROM s
   GROUP BY GROUPING SETS ((qc, jobtype, horizon_bucket), (jobtype, horizon_bucket), (jobtype))
  HAVING count(*) >= 50;

CREATE UNIQUE INDEX IF NOT EXISTS learn_work_eta_bias_pk
  ON learn_work_eta_bias (qc, jobtype, horizon_bucket);

COMMENT ON MATERIALIZED VIEW learn_work_eta_bias IS
  '작업도달 예측의 치우침 보정. 키 = (크레인, 작업유형, 예측거리 10분버킷). horizon_bucket=-1 은 거리 무관 폴백. '
  'qc='''' 는 크레인 무관. 정답지는 qc_move_log.comp_ts(mig 0115), 원본예측 복원은 bias_ver=2(mig 0117).';
