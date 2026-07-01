-- 순수주행(pure drive) 정의 정정 (사용자 정의 확정 2026-07-01):
--   순수주행 = "주행 구간 시간" = 출발 → 작업지점 도달
--            = empty_travel_start → empty_arrived  (= learn_leg_decomp.total_s)
--   · 경로상 신호·정체 정지는 아직 '주행 중'이므로 포함 (혼잡·날씨·시간대 피처가 예측할 몫).
--   · 도착 후 핸드오버 대기만 제외 — 그건 주행이 아니고 다음 구간(받기/주기)에 이미 있음.
--
-- 이전 두 정의는 둘 다 틀렸다:
--   · drive_s(30초 움직임만) = 경로 정체를 벗겨 근거리-원거리 spread를 압축 (0073에서 realized로 되돌린 바로 그 사유). 과소.
--   · realized travel_s = 도착지 핸드오버까지 포함. 과대.
--   → total_s가 정확히 가운데. 실측(중앙): dist 781m·구간 237s(=13.3km/h) vs 움직임만 120s(=22km/h).
--
-- 비용 매트뷰 이름(learn_travel_zone225_drive)은 3개 소비처 안정성 위해 유지하되 내용만 total_s로 교정.
-- ("_drive" = 사용자 정의의 '순수주행 = 그 주행(구간)'을 뜻함 — drive_s(움직임만)와 혼동 주의.)
DROP MATERIALIZED VIEW IF EXISTS learn_travel_zone225_drive;
CREATE MATERIALIZED VIEW learn_travel_zone225_drive AS
  SELECT oz, dz, count(*) AS n,
         percentile_cont(0.5) WITHIN GROUP (ORDER BY total_s)::int AS p50_s,
         percentile_cont(0.9) WITHIN GROUP (ORDER BY total_s)::int AS p90_s
    FROM learn_leg_decomp
   WHERE oz IS NOT NULL AND dz IS NOT NULL AND total_s > 0
   GROUP BY oz, dz;
CREATE UNIQUE INDEX learn_travel_zone225_drive_pk ON learn_travel_zone225_drive (oz, dz);

-- 크레인 진입정지(진입/핸드오버 대기 매트뷰) 폐기: 크레인 도착 좌표가 학습된 WHARF 중심점이라, 50m
-- 물리도착 측정이 트럭의 절반은 TOS도착 전·절반은 후로 갈려 중앙값 ≈ 0 (일관 측정: 양하 −1s·적하 −3s).
-- 기존 "중앙 72초"는 GPS 탐색을 empty_arrived까지로 잘라 'TOS도착 전 진입한 트럭'만 집계한 선택편향값.
-- 못 믿는 신호라 1단계 마감/리드에 쓰지 않기로 함 → 산출물 제거. (진짜 크레인 큐는 work_eta·스케줄 영역.)
DROP MATERIALIZED VIEW IF EXISTS learn_crane_approach;
