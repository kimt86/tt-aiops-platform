-- 비용곡선을 잡타입별로 분리 (2026-07-02, 사이클타임 감사 후속):
-- 같은 경로시간에서 양하(픽업=크레인)의 실측 공차시간이 적하(픽업=블록)보다 p50 ~1.5×·p90 ~2× 크다(측정
-- 확증). 단일 곡선은 둘을 섞어 적하 배차비용을 부풀리고 양하를 낮춘다(p90은 데드라인 필터라 특히 문제).
-- road_route_eval에 jobtype을 실어 RouteCost가 DS/LD 곡선을 따로 적합하게 한다.
-- lock_timeout: ALTER는 ACCESS EXCLUSIVE라 뜨거운 테이블에서 대기 시 앱을 줄세워 봉쇄할 수 있음 → 못 얻으면
-- 즉시 실패. statement_timeout: 백필 UPDATE 런타임 상한(ROW EXCLUSIVE라 앱 읽기/쓰기와는 공존).
SET lock_timeout = '3s';
SET statement_timeout = '60s';
ALTER TABLE road_route_eval ADD COLUMN IF NOT EXISTS jobtype text;
-- 기존 행 백필(사이클에서). 이래야 DS/LD 곡선이 즉시 채워짐(안 하면 신규 행이 쌓일 때까지 ALL 폴백).
UPDATE road_route_eval e SET jobtype = c.jobtype
  FROM tt_cycle_v2 c
 WHERE c.ytno = e.ytno AND c.dropped_at = e.leg_start AND e.jobtype IS NULL;
RESET statement_timeout;
RESET lock_timeout;
