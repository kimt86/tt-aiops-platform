-- ⑥→⑤⑥ 통합: 유휴까지-남은시간 회귀를 soon_idle 한 단계 → 전 사이클 단계로 확장 (2026-07-09).
-- 소스를 tt_soon_idle_pred(soon_idle만) → free_in_sample(mig0072, 60s마다 모든 BUSY 트럭의 state·
-- 피처·실제 남은시간 actual_remaining_s 라벨)로 교체. 상태별 실측 median을 학습해 상수를 대체:
--   delivering DS 실측 644s(상수1030 과대) · approaching 392(480) · wait_rtg 459(480) ·
--   soon_idle DS 300 / LD 425 (상수120 과소). RTG거리 bin으로 세분(soon_idle DS ≤30m 262 < 30–80m 391).
-- free_in_sample은 매 60s 스냅샷이라 "지금 이 상태로 관측된 트럭의 남은시간"(길이편향 포함) = 배차가
-- 스냅샷에서 후보를 고를 때의 조건부와 정확히 일치. GROUPING SETS: (state,jobtype,bin) → (state,jobtype).
DROP MATERIALIZED VIEW IF EXISTS learn_free_in_bias;
CREATE MATERIALIZED VIEW learn_free_in_bias AS
  WITH s AS (
    SELECT state, jobtype,
           (CASE WHEN nearest_rtg_m IS NULL THEN -1 WHEN nearest_rtg_m <= 30 THEN 0
                 WHEN nearest_rtg_m <= 80 THEN 1 WHEN nearest_rtg_m <= 150 THEN 2 ELSE 3 END) AS dist_bin,
           actual_remaining_s AS rem
      FROM free_in_sample
     WHERE ts > now() - interval '7 days'
       AND state IN ('delivering', 'approaching', 'wait_rtg', 'soon_idle')
       AND jobtype IS NOT NULL
       -- NO upper cap: percentile_cont(0.5) is a median (insensitive to tail magnitude); an upper
       -- cutoff would delete upper-tail COUNT and shift every median DOWN (~10-15%), re-introducing
       -- the very optimism this replaces. NULL actual_remaining_s = no drop within the 2h backfill
       -- window (right-censored longest) — excluded as unresolved (median over resolved cases).
       AND actual_remaining_s IS NOT NULL AND actual_remaining_s >= 0
  )
  SELECT state, jobtype, coalesce(dist_bin, -99) AS dist_bin, count(*)::int AS n,
         percentile_cont(0.5) WITHIN GROUP (ORDER BY rem)::int AS med_rem_s
    FROM s
   GROUP BY GROUPING SETS ((state, jobtype, dist_bin), (state, jobtype));
CREATE UNIQUE INDEX learn_free_in_bias_pk ON learn_free_in_bias (state, jobtype, dist_bin);
