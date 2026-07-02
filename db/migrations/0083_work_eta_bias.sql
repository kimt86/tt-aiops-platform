-- 1단계 작업시각(work_eta) 자가 보정층 (2026-07-02, 1단계 상세검토 후속):
-- 그림자 검증(dispatch_pred_sample)의 (실제 작업시각 − 예측) 잔차 중앙값을 크레인·작업유형별로 배워
-- build_workpool이 work_eta에 더한다. 검토 실측: 적하는 보정 0이라 근거리 +21분 낙관(트럭 조기호출
-- 낭비 12~18분/대), 양하 정적 +600s는 유령 오버헤드 버그(0083과 같은 커밋에서 수정) 위에서 맞춘 값 —
-- 둘 다 이 학습층이 흡수·자가 재보정한다(예측에 보정이 들어가면 잔차가 0으로 수렴하는 적분 구조,
-- 7일 창 + 20분 리프레시로 감쇠).
-- horizon 5~45분 = 배차 결정대(마감여유 양하 450s/적하 1180s 부근)만 학습 — 원거리 예측은
-- 재편성·취소 오염으로 붕괴 상태라(검토 확인) 섞으면 안 됨.
-- GROUPING SETS: 크레인별 행 + 전체(jobtype만, qc='') 폴백 행.
DROP MATERIALIZED VIEW IF EXISTS learn_work_eta_bias;
CREATE MATERIALIZED VIEW learn_work_eta_bias AS
  SELECT coalesce(qc, '') AS qc, jobtype, count(*)::int AS n,
         percentile_cont(0.5) WITHIN GROUP (
           ORDER BY extract(epoch FROM resolved_at - pred_work_eta_ts))::int AS med_err_s
    FROM dispatch_pred_sample
   WHERE resolved_at IS NOT NULL
     AND jobtype IS NOT NULL
     AND logged_at > now() - interval '7 days'
     -- 유령 오버헤드 버그 수정 배포(2026-07-02 00:45Z) 이후 행만: 그 전 잔차는 버그의 서명이라
     -- 학습에 섞으면 이중 보정이 됨. 7일 롤링창이 이 하한을 지나면 자연히 무의미해짐.
     AND logged_at > TIMESTAMPTZ '2026-07-02 00:45+00'
     AND pred_work_eta_ts - logged_at BETWEEN interval '5 minutes' AND interval '45 minutes'
   GROUP BY GROUPING SETS ((qc, jobtype), (jobtype));
CREATE UNIQUE INDEX learn_work_eta_bias_pk ON learn_work_eta_bias (qc, jobtype);
