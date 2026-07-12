-- 유휴시각 재설계 검증: "타이트 작업지점 도착 + 앞대수" (2026-07-13).
-- 사용자 모델: 트럭이 자기 작업지점(QC 슬롯 / RTG 베이)에 도착 = 핸드오버 시작이고, 자유까지는
-- 크레인 사이클 × (1 + 앞 대수)로 바운드된다. QC는 슬롯 2개라 앞 0~1, RTG는 갠트리 고정이라 베이
-- 도착=확정 처리. 현재 classify_tt의 '도착'은 70m 느슨 지오펜스(차선 줄서기 포함)라 도착→자유가
-- 넓게 퍼짐(DS p90 1411s). 이 로거는 그보다 타이트한 작업지점 도착(≤TIGHT_WP_M·정지)을 트립당 처음
-- 한 번 잡고, 그 순간 같은 작업지점 클러스터의 정지한 적재 트럭 수(앞대수)를 GPS로 센다.
-- classify_tt/배차 미변경 — 검증 데이터부터 축적.
-- 검증(축적 후): tt_wp_arrival ⋈ tt_cycle_v2.dropped_at(같은 ytno, arrived_at 이후 20분 내) →
--   residual = dropped_at - arrived_at 를 jobtype × ahead_n 별로. 모델 성립 = residual이 바운드되고
--   ahead_n에 단조 증가(≈ cycle×(1+ahead_n)); QC 사이클 ~91s(엣지 실측), RTG는 데이터가 말해줌.
CREATE TABLE IF NOT EXISTS tt_wp_arrival (
  id            BIGSERIAL PRIMARY KEY,
  ytno          TEXT NOT NULL,
  container     TEXT,
  jobtype       TEXT,                       -- LD / DS
  wp_code       TEXT,                       -- topos1 (LD=크레인, DS=베이)
  arrived_at    TIMESTAMPTZ NOT NULL,       -- 트립당 첫 타이트 작업지점 도착(정지)
  wp_dist_m     DOUBLE PRECISION,           -- 도착 시 작업지점까지 거리(귀속 신뢰도)
  ahead_n       INT,                        -- 같은 작업지점 클러스터의 다른 정지 적재 트럭 수(앞대수)
  business_date DATE NOT NULL,
  shift         TEXT NOT NULL,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS tt_wp_arrival_uniq ON tt_wp_arrival (ytno, container, arrived_at);
CREATE INDEX        IF NOT EXISTS tt_wp_arrival_yt   ON tt_wp_arrival (ytno, arrived_at);
