-- ⑤ 곧빔 게이트 양방향 튜닝 (2026-07-09, 0084 후속):
-- 0084 게이트는 로그가 게이트(≤현재값) 안쪽 fired 예측뿐이라 tighten만 가능했다. 여기서 near-miss
-- (도착·적재됐지만 nearest RTG가 게이트 밖 = wait_rtg 첫 진입)를 로깅해, "게이트 밖 거리에서
-- 곧-유휴가 실제로 자주 일어났나"를 관측 → precision 곡선을 전 거리대로 확장해 게이트가 넓힐(loosen)
-- 수도 있게 한다. 로거: spawn_soon_idle_logger가 wait_rtg 첫 진입을 tt_soon_idle_nearmiss에 기록.

CREATE TABLE IF NOT EXISTS tt_soon_idle_nearmiss (
  id             BIGSERIAL PRIMARY KEY,
  ytno           TEXT NOT NULL,
  container      TEXT,
  jobtype        TEXT,
  observed_at    TIMESTAMPTZ NOT NULL,   -- first wait_rtg entry = counterfactual reference time
  nearest_rtg_m  DOUBLE PRECISION,       -- RTG distance then (beyond the gate)
  business_date  DATE NOT NULL,
  shift          TEXT NOT NULL,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS tt_soon_idle_nearmiss_uniq     ON tt_soon_idle_nearmiss (ytno, container, observed_at);
CREATE INDEX        IF NOT EXISTS tt_soon_idle_nearmiss_match_idx ON tt_soon_idle_nearmiss (ytno, container);
CREATE INDEX        IF NOT EXISTS tt_soon_idle_nearmiss_bd_idx    ON tt_soon_idle_nearmiss (business_date, shift, observed_at);

-- ── 게이트: fired 예측 ∪ near-miss 를 (거리 d, 시각 t)로 관측하고 [t, t+900s] 내 실제 유휴 여부(hit).
--    precision(≤c) 유지 최대 컷오프 c — 이제 c가 현재 게이트 위/아래 어디든 갈 수 있다. clamp[30,90].
DROP MATERIALIZED VIEW IF EXISTS learn_soon_idle_gate;
CREATE MATERIALIZED VIEW learn_soon_idle_gate AS
  -- DISTINCT ON (ytno, container): one observation per trip per source (earliest). A transient GPS
  -- stale gap can drop the logger's dedup key and re-insert a trip's "first entry" a second time
  -- (livemap.rs soon_idle_open/nearmiss_open); collapsing here keeps that from double-counting the
  -- precision/count curve. Fired (≤gate) and near-miss (>gate) of the same trip are different
  -- distances → both legitimately kept (dedup is within each source, not across).
  WITH obs AS (
    (SELECT DISTINCT ON (p.ytno, p.container)
            p.ytno, p.container, p.jobtype, p.predicted_at AS t, p.nearest_rtg_m AS d
       FROM tt_soon_idle_pred p
      WHERE p.jobtype = 'DS' AND p.nearest_rtg_m IS NOT NULL AND p.predicted_at > now() - interval '7 days'
      ORDER BY p.ytno, p.container, p.predicted_at)
    UNION ALL
    (SELECT DISTINCT ON (n.ytno, n.container)
            n.ytno, n.container, n.jobtype, n.observed_at AS t, n.nearest_rtg_m AS d
       FROM tt_soon_idle_nearmiss n
      WHERE n.jobtype = 'DS' AND n.nearest_rtg_m IS NOT NULL AND n.observed_at > now() - interval '7 days'
      ORDER BY n.ytno, n.container, n.observed_at)
  ), hh AS (
    SELECT o.d,
           (coalesce(g.dropped_at, t2.comp_ts) IS NOT NULL
            AND EXTRACT(EPOCH FROM (coalesce(g.dropped_at, t2.comp_ts) - o.t)) BETWEEN 0 AND 900)::int AS hit
      FROM obs o
      LEFT JOIN LATERAL (
        SELECT dropped_at FROM tt_cycle_v2 c
         WHERE c.ytno = o.ytno AND c.jobtype = o.jobtype
           AND c.dropped_at >= o.t - interval '90 seconds' AND c.dropped_at < o.t + interval '20 minutes'
         ORDER BY c.dropped_at LIMIT 1) g ON true
      LEFT JOIN LATERAL (
        SELECT comp_ts FROM tos_handover_label h
         WHERE h.ytno = o.ytno AND h.contno = o.container
           AND h.comp_ts >= o.t - interval '60 seconds' AND h.comp_ts < o.t + interval '20 minutes'
         ORDER BY abs(EXTRACT(EPOCH FROM (h.comp_ts - o.t))) LIMIT 1) t2 ON true
  ), c AS (
    -- RANGE (not ROWS): with tied distances (nearest_rtg_m is 0.1m-discretized) the cumulative
    -- precision/count must cover ALL rows with d' <= d, not an arbitrary cut inside a tie group —
    -- else max(d) FILTER(cum_prec>=0.82) can admit a distance whose full cum_prec is below target.
    SELECT d, avg(hit) OVER w AS cum_prec, count(*) OVER w AS cum_n
      FROM hh WINDOW w AS (ORDER BY d RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)
  )
  SELECT 'DS'::text AS jobtype,
         (SELECT count(*) FROM hh)::int AS n,
         (SELECT count(*) FROM tt_soon_idle_nearmiss WHERE jobtype = 'DS' AND observed_at > now() - interval '7 days')::int AS nearmiss_n,
         round((SELECT avg(hit) * 100 FROM hh))::int AS prec_pct,
         greatest(30.0, least(90.0,
           coalesce(max(d) FILTER (WHERE cum_prec >= 0.82 AND cum_n >= 100), 50.0)))::real AS gate_m
    FROM c;
CREATE UNIQUE INDEX learn_soon_idle_gate_pk ON learn_soon_idle_gate (jobtype);
