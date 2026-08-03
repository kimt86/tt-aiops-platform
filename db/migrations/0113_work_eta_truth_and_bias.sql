-- 0113: 작업 도달시각(work-ETA) 예측의 **채점 기준**과 **보정 갱신 규칙**을 고친다.
--
-- ■ 무엇이 잘못됐나 (2026-08-03 실측)
-- work_eta_ts 는 "크레인이 이 컨테이너에 언제 도달하나"를 예측한다. 그런데 채점을 두 갈래로
-- 하고 있었다:
--   양하(DS) = TOS 작업 시각          → 크레인 실제 시작과 +55초 차이(정확)
--   적하(LD) = 컨테이너가 풀에서 사라진 시각 → 크레인 실제 시작보다 **+567초 늦음**
-- 코드 주석은 "LD has no such per-container signal" 이라 적혀 있었지만 **사실이 아니다** —
-- qc_move_log 에 적하 75,201건이 contno·st_ts·comp_ts 까지 전부 채워져 라이브로 쌓이고 있다
-- (양하 69,399건과 대등, 매칭률 98.2%). 한 소스(라이브 작업풀 스냅샷)에 없다고 전체에 없다고
-- 단정한 것이다.
--
-- 그 결과: 크레인 **실제 시작** 기준으로 재면 적하 예측 오차는 **+105초(2분 미만)** 로 이미
-- 정확한데, 늦은 기준으로 채점해서 "10분 늦음"으로 보였고 보정이 +693초까지 부풀었다.
--
-- ■ 두 번째 결함: 보정이 수렴할 수 없는 구조
-- 보정을 **누적이 아니라 교체**로 넣는다. 새 보정 = median(정답 − 예측)인데 그 예측에는 이미
-- 이전 보정이 들어 있다 ⇒ L_new = R − L_old (R = 정답 − 원본예측). 이건 수렴이 아니라 **진동**
-- 이다. 실측이 그 쌍을 보여준다: 보정 693 ↔ 잔차 589. 양하는 R 이 작아(55초) 진폭이 안 보였다.
--
-- ■ 이 마이그레이션이 하는 일
-- 1) applied_bias_s — 그 예측에 실제로 적용된 보정을 함께 기록한다. 그러면 매뷰가
--    원본예측(= pred − applied_bias)을 복원해 **원본 대비 잔차**를 잴 수 있다. 되먹임 소멸:
--    L = median(정답 − 원본) 을 한 번에 추정하므로 반복도 진동도 없다.
-- 2) resolved_src — 정답을 어디서 얻었는지('qc' = 크레인 기록 / 'pool' = 풀 이탈 대체값).
--    매뷰는 'qc' 만 쓴다. 대체값이 다시 보정을 오염시키지 못하게 하는 것이 핵심이다.
-- 3) 표본 창도 원본예측 기준으로 자른다 — 보정된 값으로 자르면 보정이 커질수록 표본 모집단이
--    이동해(창이 [5,45]분에 고정인데 기준점이 움직인다) 서로 다른 집단을 비교하게 된다.
-- 4) HAVING count(*) >= 50 — 크레인 3건짜리 보정은 잡음이다. 기존 매뷰에는 최소표본이 없었다.
-- 5) qc_move_log (contno, jobtype, st_ts) 인덱스 — 정답 조회가 contno 로 들어가는데 PK 가
--    (machno, contno, seqno) 라 contno 단독 조회가 안 된다(실측: 인덱스 없이 2분 타임아웃).
--
-- ⚠ 전환기: 기존 행은 applied_bias_s 가 NULL 이라 매뷰에서 빠진다. 새 규칙 표본이 50건 쌓일
--    때까지 보정이 비고, 그동안 적하 예측은 원본값(현재보다 ~13분 이르다)을 쓴다. 마감이
--    보수적으로(더 급하게) 잡히는 방향이라 안전한 쪽이고, 적하 6~9천건/일이라 한두 시간이면
--    채워진다.
--
-- ■ 적용 후 실측 (2026-08-03, 창 5~45분, 정답 = qc_move_log.st_ts)
--   DS n=565  중앙  −35초   p10 −1085 / p90 +1351   절대오차평균 788초
--   LD n=188  중앙 +391초   p10  −806 / p90 +1506   절대오차평균 804초
--
--   ⚠ 여기 위에서 내가 "수렴 후 약 +798초"로 예상했던 것은 **빗나갔다**(실측 +391). 예상은 옛
--   표본(보정 693이 박힌 예측, 창을 보정된 값으로 자른 집단)에서 잰 잔차를 그대로 외삽한 것인데,
--   이 마이그레이션이 창 기준을 원본예측으로 바꿔 **모집단 자체가 달라졌다**. 예상치를 지우지 않고
--   남겨 둔다 — 창 기준이 바뀌면 이전 잔차 측정은 이월되지 않는다는 기록이다.
--
--   더 중요한 실측: 양하·적하의 **퍼짐이 사실상 같다**(절대오차평균 788 vs 804초, p10~p90 폭도
--   동일). 즉 이 보정이 걷어내는 중앙값 치우침은 ±20분 퍼짐에 비하면 작다. 옛 "+23 vs +693" 이라는
--   극단적 비대칭은 거의 전부 **채점 기준을 두 갈래로 쓴 탓**이었지, 적하 예측이 실제로 그만큼
--   나쁜 게 아니었다. 남은 DS↔LD 중앙값 차(약 426초)는 실재하나 원인 미규명 — qc_move_log 에
--   트럭 도착 시각 컬럼이 없어 "적하는 크레인이 트럭을 기다린다" 가설은 이 표만으로 검정 불가.
--
--   ⇒ 크레인별 보정 최소표본은 코드에서 30 → 150 으로 올렸다(workpool.rs). σ≈1040초에서 n=30 은
--   중앙값 표준오차가 ~240초로 재려는 효과보다 커서 잡음을 학습한다.
--
-- 멱등.

ALTER TABLE dispatch_pred_sample ADD COLUMN IF NOT EXISTS applied_bias_s integer;
ALTER TABLE dispatch_pred_sample ADD COLUMN IF NOT EXISTS resolved_src   text;

COMMENT ON COLUMN dispatch_pred_sample.applied_bias_s IS
  '이 예측에 적용된 학습 보정 초. 원본예측 = pred_work_eta_ts - applied_bias_s. '
  '이게 있어야 보정을 되먹임 없이 한 번에 추정할 수 있다.';
COMMENT ON COLUMN dispatch_pred_sample.resolved_src IS
  'qc = qc_move_log.st_ts(크레인 실제 시작·권위) · pool = 풀 이탈 대체값(늦음). 보정 학습은 qc 만 쓴다.';

CREATE INDEX IF NOT EXISTS qc_move_log_cont_idx ON qc_move_log (contno, jobtype, st_ts);

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
     AND resolved_src = 'qc'            -- 권위 있는 정답만
     AND applied_bias_s IS NOT NULL     -- 원본예측을 복원할 수 있는 행만
     AND logged_at > now() - interval '7 days'
     -- 표본 창은 **원본예측** 기준. 보정된 값으로 자르면 보정이 커질수록 모집단이 이동한다.
     AND (pred_work_eta_ts - make_interval(secs => applied_bias_s) - logged_at)
           BETWEEN interval '5 min' AND interval '45 min'
   GROUP BY GROUPING SETS ((qc, jobtype), (jobtype))
  HAVING count(*) >= 50;

CREATE UNIQUE INDEX IF NOT EXISTS learn_work_eta_bias_pk ON learn_work_eta_bias (qc, jobtype);
