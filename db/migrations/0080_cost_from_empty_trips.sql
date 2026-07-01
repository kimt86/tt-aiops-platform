-- 순수주행 OD 비용의 출처를 leg_decomp(GPS 모션분해·~1일·신뢰셀 15개)에서
-- learn_travel_sample의 빈-트립 행(leg_ord=0)으로 교체. 정의는 동일(순수주행=구간시간):
--   빈트립 travel_s = empty_arrived − empty_travel_start (사이클에서 직접, GPS 불필요),
--   origin = 직전 사이클 드롭(트럭별 lag 체이닝), dest = 이 사이클 픽업 — 이미 수집 중(livemap.rs ~2138).
-- 이점: (1) GPS 2일 창에 안 묶여 3주치 축적 → 신뢰셀 15→431개(~29배), (2) leg_decomp를 비용에서 분리(저장통합).
-- 좌표→225m 격자는 realized zone225(mig 0051)와 동일한 travel_grid225. 소비처(oz/dz/p50_s/p90_s/n)·이름 불변.
DROP MATERIALIZED VIEW IF EXISTS learn_travel_zone225_drive;
CREATE MATERIALIZED VIEW learn_travel_zone225_drive AS
  SELECT travel_grid225(origin_lat, origin_lon) AS oz,
         travel_grid225(dest_lat,   dest_lon)   AS dz,
         count(*)::int AS n,
         percentile_cont(0.5) WITHIN GROUP (ORDER BY travel_s)::int AS p50_s,
         percentile_cont(0.9) WITHIN GROUP (ORDER BY travel_s)::int AS p90_s
    FROM learn_travel_sample
   WHERE leg_ord = 0                          -- 빈 트럭 트립(순수주행) — 실차 leg(ord≥1) 제외
     AND travel_s BETWEEN 1 AND 3600
     AND origin_lat IS NOT NULL AND dest_lat IS NOT NULL
   GROUP BY 1, 2;
CREATE UNIQUE INDEX learn_travel_zone225_drive_pk ON learn_travel_zone225_drive (oz, dz);
