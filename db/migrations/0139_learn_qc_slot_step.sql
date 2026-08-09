-- 0139: 상자별 예측의 '순번당 걸음'을 실측으로 학습하는 매뷰 + pred_ver=3 판별자.
--
-- 문제(실측 2026-08-07~10): 상자별 작업도달 예측 = 구역시작 + 잔여순번 × 걸음인데, 걸음에
-- 벽시계 무브 리듬(learn_qc_wall_cadence, DS 126/LD 183초)을 쓰고 있었다. 크레인은 계획
-- 순서를 그대로 따르지 않으므로(구역별 comp~순번 회귀 R² DS 0.27/LD 0.53) "잔여 순번
-- 하나당 실제 경과"는 무브당 리듬보다 작다 — 활성 구역 실측 중앙값 DS 117/LD 133초.
-- 그 차이 × 순번이 뒤 순번의 늦은 예측(17+ 구간 DS −15/LD −23분)으로 쌓였다.
-- ⚠ 2026-08-07 보고의 "완료분 이중계상"은 오진으로 철회한다 — live_stow_plan 은 애초에
-- 남은 상자만 담는 거울이라(활성 구역 실측: 완료 72~127 vs 계획 잔행 0~28·겹침 ~0)
-- 순번은 이미 잔여 기준이었다. 살아남은 결함은 걸음 값 하나다.
--
-- 학습원은 채점 표 그 자체다: 로깅 시점의 잔여 순번(slot_idx)과 실제 경과(resolved_at −
-- logged_at)의 비의 중앙값. **활성 구역 행만** 쓴다(로깅 전 45분 안에 같은 구역 comp가
-- 있는 행) — 시작 전 구역은 경과에 '구역 시작 대기'가 섞여 걸음이 부풀기 때문.
-- 예측은 이 두 값(순번·실측 경과)에 영향을 주지 않으므로(그림자) 순환 학습이 아니다.
-- 갱신: spawn_dispatch_pred_logger 가 20분 주기 REFRESH (wall_cadence 와 같은 자리).
CREATE MATERIALIZED VIEW IF NOT EXISTS learn_qc_slot_step AS
WITH s AS (
  SELECT qc, queuename, jobtype, slot_idx, logged_at,
         EXTRACT(epoch FROM resolved_at - logged_at) AS elapsed_s
    FROM dispatch_pred_sample
   WHERE pred_ver >= 2 AND resolved_src = 'qc_comp' AND resolved_at IS NOT NULL
     AND logged_at > now() - interval '3 days' AND slot_idx >= 2
     AND resolved_at > logged_at
), act AS (
  SELECT * FROM s
   WHERE EXISTS (SELECT 1 FROM qc_move_log m
                  WHERE m.machno = s.qc AND m.queuename = s.queuename
                    AND m.comp_ts BETWEEN s.logged_at - interval '45 minutes' AND s.logged_at)
), per_qc AS (
  SELECT qc, jobtype, count(*)::int AS n,
         round(percentile_cont(0.5) WITHIN GROUP (ORDER BY elapsed_s / slot_idx))::int AS step_s
    FROM act GROUP BY 1, 2 HAVING count(*) >= 30
), gl AS (
  -- 크레인별 표본이 모자랄 때의 전역 폴백 행. qc='*' 는 실제 크레인 id 와 충돌하지 않는다.
  SELECT '*'::text AS qc, jobtype, count(*)::int AS n,
         round(percentile_cont(0.5) WITHIN GROUP (ORDER BY elapsed_s / slot_idx))::int AS step_s
    FROM act GROUP BY 2 HAVING count(*) >= 200
)
SELECT qc, jobtype, step_s, n, now() AS as_of_ts FROM per_qc
UNION ALL
SELECT qc, jobtype, step_s, n, now() AS as_of_ts FROM gl;

CREATE UNIQUE INDEX IF NOT EXISTS learn_qc_slot_step_key ON learn_qc_slot_step (qc, jobtype);

COMMENT ON MATERIALIZED VIEW learn_qc_slot_step IS
  '상자별 작업도달 예측의 순번당 걸음(초). 활성 구역 채점 행의 (경과시간/잔여순번) 중앙값. '
  'qc=''*'' 행은 전역 폴백. 벽시계 무브 리듬(learn_qc_wall_cadence)과 다른 양이다 — 그쪽은 '
  '무브당, 이쪽은 계획 순서 이탈이 반영된 순번당 (mig 0139).';

COMMENT ON COLUMN dispatch_pred_sample.pred_ver IS
  'NULL=레거시 front-6 공식. 2=배선된 상자별 예측(계획 slot × 벽시계 무브시간, mig 0130). '
  '3=걸음을 learn_qc_slot_step(순번당 실측 경과)로 교체(2026-08-10, mig 0139) — slot 의 '
  '의미는 2와 같다(잔여 계획 순번·트윈 환산). 집계는 반드시 이 값으로 가를 것.';
