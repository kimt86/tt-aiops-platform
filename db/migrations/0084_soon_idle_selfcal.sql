-- ⑤ 곧빔 게이트 + ⑥ 유휴분 잔차 자가보정 (2026-07-09):
-- 둘 다 tt_soon_idle_pred(예측) ⋈ 실측 유휴시각(tt_cycle_v2.dropped_at, fallback TOS)에서 배워,
-- ⑦ learn_work_eta_bias와 같은 방식(주기 REFRESH + 라이브 경로에서 읽어 자가 재보정)으로 돈다.
--
-- ⑥ learn_free_in_bias — soon_idle 진입→실제 유휴까지 median lead(초)를 (jobtype, RTG거리bin)별로.
--   실측: 상수 free_in(soon_idle)=120s는 크게 낙관(DS 실측 median ~289s, LD ~246s) → 배차 후보를
--   너무 일찍 부름. bin으로 RTG거리 신호까지 반영(≤30m 265s < 30–80m 345s). dist_bin: -1 RTG없음,
--   0 ≤30m, 1 ≤80m, 2 ≤150m, 3 >150m, -99 전체 폴백(GROUPING SETS 롤업).
-- ⑤ learn_soon_idle_gate — DS 블록 soon_idle 판정의 RTG거리 컷오프(RTG_BAY_M=50m 대체).
--   precision(≤900s 내 실제 유휴) 목표 0.82를 유지하는 최대 컷오프. 실측: precision 상한 ~84%라
--   0.85는 도달불가, 0.82면 ~42m(노이즈 큰 42–50m 밴드 트림). [30,55] 클램프, 데이터 부족 시 50 폴백.
--   주의: 로그가 현재 게이트(≤50m) 안에서만 있어 지금은 tighten만 가능 — 완화(>50m)는 wait_rtg
--   근접 미스 로깅이 붙어야 한다(후속).

-- ── ⑥ 유휴분 잔차 ────────────────────────────────────────────────────────
DROP MATERIALIZED VIEW IF EXISTS learn_free_in_bias;
CREATE MATERIALIZED VIEW learn_free_in_bias AS
  WITH j AS (
    SELECT p.jobtype,
           (CASE WHEN p.nearest_rtg_m IS NULL THEN -1 WHEN p.nearest_rtg_m <= 30 THEN 0
                 WHEN p.nearest_rtg_m <= 80 THEN 1 WHEN p.nearest_rtg_m <= 150 THEN 2 ELSE 3 END) AS dist_bin,
           EXTRACT(EPOCH FROM (
             coalesce(g.dropped_at,
                      CASE WHEN p.jobtype = 'LD' THEN coalesce(t.dis_ts, t.comp_ts) ELSE t.comp_ts END)
             - p.predicted_at)) AS lead_s
      FROM tt_soon_idle_pred p
      LEFT JOIN LATERAL (
        SELECT dropped_at FROM tt_cycle_v2 c
         WHERE c.ytno = p.ytno AND c.jobtype = p.jobtype
           AND c.dropped_at >= p.predicted_at - interval '90 seconds'
           AND c.dropped_at <  p.predicted_at + interval '20 minutes'
         ORDER BY c.dropped_at LIMIT 1) g ON true
      LEFT JOIN LATERAL (
        SELECT dis_ts, comp_ts FROM tos_handover_label h
         WHERE h.ytno = p.ytno AND (p.jobtype <> 'DS' OR h.contno = p.container)
           AND h.comp_ts >= p.predicted_at - interval '60 seconds'
           AND h.comp_ts <  p.predicted_at + interval '20 minutes'
         ORDER BY abs(EXTRACT(EPOCH FROM (h.comp_ts - p.predicted_at))) LIMIT 1) t ON true
     WHERE p.predicted_at > now() - interval '7 days'
  )
  SELECT jobtype, coalesce(dist_bin, -99) AS dist_bin, count(*)::int AS n,
         percentile_cont(0.5) WITHIN GROUP (ORDER BY lead_s)::int AS med_lead_s
    FROM j
   WHERE lead_s >= 0 AND lead_s < 3600
   GROUP BY GROUPING SETS ((jobtype, dist_bin), (jobtype));
CREATE UNIQUE INDEX learn_free_in_bias_pk ON learn_free_in_bias (jobtype, dist_bin);

-- ── ⑤ 곧빔 게이트 ────────────────────────────────────────────────────────
DROP MATERIALIZED VIEW IF EXISTS learn_soon_idle_gate;
CREATE MATERIALIZED VIEW learn_soon_idle_gate AS
  WITH pp AS (
    SELECT p.nearest_rtg_m AS d,
           (coalesce(g.dropped_at, t.comp_ts) IS NOT NULL
            AND EXTRACT(EPOCH FROM (coalesce(g.dropped_at, t.comp_ts) - p.predicted_at))
                BETWEEN 0 AND 900)::int AS hit
      FROM tt_soon_idle_pred p
      LEFT JOIN LATERAL (
        SELECT dropped_at FROM tt_cycle_v2 c
         WHERE c.ytno = p.ytno AND c.jobtype = p.jobtype
           AND c.dropped_at >= p.predicted_at - interval '90 seconds'
           AND c.dropped_at <  p.predicted_at + interval '20 minutes'
         ORDER BY c.dropped_at LIMIT 1) g ON true
      LEFT JOIN LATERAL (
        SELECT comp_ts FROM tos_handover_label h
         WHERE h.ytno = p.ytno AND h.contno = p.container
           AND h.comp_ts >= p.predicted_at - interval '60 seconds'
           AND h.comp_ts <  p.predicted_at + interval '20 minutes'
         ORDER BY abs(EXTRACT(EPOCH FROM (h.comp_ts - p.predicted_at))) LIMIT 1) t ON true
     WHERE p.jobtype = 'DS' AND p.nearest_rtg_m IS NOT NULL
       AND p.predicted_at > now() - interval '7 days'
  ), c AS (
    SELECT d,
           avg(hit) OVER w AS cum_prec,
           count(*) OVER w AS cum_n
      FROM pp WINDOW w AS (ORDER BY d ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)
  )
  SELECT 'DS'::text AS jobtype,
         (SELECT count(*) FROM pp)::int AS n,
         round((SELECT avg(hit) * 100 FROM pp))::int AS prec_pct,
         greatest(30.0, least(55.0,
           coalesce(max(d) FILTER (WHERE cum_prec >= 0.82 AND cum_n >= 100), 50.0)))::real AS gate_m
    FROM c;
CREATE UNIQUE INDEX learn_soon_idle_gate_pk ON learn_soon_idle_gate (jobtype);
