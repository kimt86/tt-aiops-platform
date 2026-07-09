-- LD 핸드오버 탐지 검증용 섀도 (2026-07-09):
-- QC 훅로드 empty→laden 라이징엣지(픽업 순간)를, 그 크레인 작업지점에 도착·적재된 LD 트럭에 귀속해
-- 적재한다. LD는 픽업 순간 ≈ 트럭 자유이므로, edge→실제 유휴 잔차가 ~0이어야 한다(현재 도착 기반
-- soon_idle: median 248s / p90 837s = 거의 전부 픽업 전 대기). 이 로거로 우리 데이터에서 잔차가 실제로
-- 촘촘한지 + 엣지→트럭 귀속이 맞는지 검증한 뒤에 classify_tt에 배선한다(지금은 라이브 미변경).
-- 검증 쿼리(데이터 축적 후):
--   SELECT count(*), percentile_cont(0.5)/(0.9) WITHIN GROUP (ORDER BY resid)
--   FROM (SELECT EXTRACT(EPOCH FROM (c.dropped_at - e.edge_ts)) resid
--         FROM qc_handover_edge e JOIN LATERAL (SELECT dropped_at FROM tt_cycle_v2 c
--           WHERE c.ytno=e.ytno AND c.jobtype='LD'
--             AND c.dropped_at BETWEEN e.edge_ts-interval '120s' AND e.edge_ts+interval '600s'
--           ORDER BY abs(EXTRACT(EPOCH FROM (c.dropped_at-e.edge_ts))) LIMIT 1) c ON true
--         WHERE e.ytno IS NOT NULL AND e.n_arrived=1) t WHERE resid BETWEEN -120 AND 600;

CREATE TABLE IF NOT EXISTS qc_handover_edge (
  id            BIGSERIAL PRIMARY KEY,
  crane         TEXT NOT NULL,
  edge_ts       TIMESTAMPTZ NOT NULL,      -- QC hook-load empty→laden rising edge (a pickup)
  ytno          TEXT,                       -- attributed docked LD truck (NULL if none within gate)
  container     TEXT,
  jobtype       TEXT,
  truck_dist_m  DOUBLE PRECISION,           -- docked truck distance to crane workpoint (attribution confidence)
  n_arrived     INT,                        -- loaded LD trucks within the gate (>1 = queue ambiguity)
  land          BOOLEAN,                    -- spreader land-side at log time (rough sea/land hint)
  business_date DATE NOT NULL,
  shift         TEXT NOT NULL,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS qc_handover_edge_uniq ON qc_handover_edge (crane, edge_ts);
CREATE INDEX        IF NOT EXISTS qc_handover_edge_yt   ON qc_handover_edge (ytno, edge_ts);
