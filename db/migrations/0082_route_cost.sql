-- 배차 OD 비용 = 도로망 라우터로 전환 (2026-07-02 사용자 결정):
--   비용 = 추론 도로망(조밀화+작업점 커넥터, road_node/road_edge) 위 경로시간 × 보정계수.
--   보정계수 = 실측 빈트립 순수주행 ÷ 경로시간의 p50/p90 분포(road_route_eval에서 학습) — 잔차 학습층.
--   근거: 이 터미널에선 경로거리≈맨해튼(corr 0.998)이지만, 도로망은 차선속도·혼잡 가중(경로'시간')의
--   헤드룸이 있고 비격자 터미널에도 일반화됨. 맨해튼÷속도는 라우팅 불능 쌍의 폴백으로만 남음.
--
-- (1) 225m 격자 룩업 폐기 — 격자 인터림의 마지막 잔재. 소비처 3곳(매처·비교기)은 RouteCost로 교체됨.
DROP MATERIALIZED VIEW IF EXISTS learn_travel_zone225_drive;

-- (2) road_route_eval 라벨 재정의: drive_s(움직임만 — 폐기된 정의) → actual_s(순수주행=구간시간,
--     learn_travel_sample 빈트립 travel_s). 옛 행은 다른 라벨이라 섞으면 보정이 오염됨 → 비움.
--     leg_start 컬럼은 이제 사이클의 dropped_at을 키로 담음(빈트립 유일키 = ytno+dropped_at).
TRUNCATE road_route_eval;
DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM information_schema.columns
              WHERE table_name='road_route_eval' AND column_name='drive_s') THEN
    ALTER TABLE road_route_eval RENAME COLUMN drive_s TO actual_s;
  END IF;
END $$;
-- 상시 게이트(경로 vs 맨해튼)를 위해 같은 행에 회전격자 맨해튼 거리도 병기.
ALTER TABLE road_route_eval ADD COLUMN IF NOT EXISTS manh_m int;
-- 중복 적재 방지 키(eval 배치의 NOT EXISTS 탐색용).
CREATE INDEX IF NOT EXISTS road_route_eval_key ON road_route_eval (ytno, leg_start);
