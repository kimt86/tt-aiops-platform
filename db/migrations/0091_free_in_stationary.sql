-- 정차 앵커 재캘리브레이션 (2026-07-15): 곧-유휴 시각을 GPS 정차 순간 기준으로.
-- 배경: 기존 곧-유휴 시각은 laden_arrived_at(느슨 70m/TOS도착) 기준이라 트럭이 아직 이동 중인데
-- "도착 대기"로 계산돼 ~2배 부풀려짐(대기의 50%가 delivering 상태). GPS 정차(마지막 delivering→dropped)
-- 로 재면 진짜 대기 = 절반(LD p50 140·p90 520 / DS p50 256·p90 763). dropped_at은 훅로드엣지 −29초로 정확.
-- 이 MV는 정차 후 진짜 잔여시간(jobtype별 median·p90)을 학습해, 배차가 '정차한' 후보 트럭의 base로 쓴다
-- (이동 중 트럭은 기존 free_in 유지). 개별 정밀도는 여전히 없으나(정차 후에도 0~520s 흩어짐) 점추정
-- 캘리브레이션이 정확해진다. spawn_selfcal_refresh가 갱신·적재.
DROP MATERIALIZED VIEW IF EXISTS learn_free_in_stationary;
CREATE MATERIALIZED VIEW learn_free_in_stationary AS
  WITH cyc AS (
    SELECT ytno, jobtype, dropped_at FROM tt_cycle_v2
     WHERE jobtype IN ('LD','DS') AND dropped_at > now() - interval '7 days'
  ), lm AS (
    SELECT c.jobtype, c.dropped_at,
           max(h.ts) FILTER (WHERE h.state = 'delivering') AS last_move
      FROM cyc c
      JOIN truck_pos_hist h ON h.ytno = c.ytno
        AND h.ts BETWEEN c.dropped_at - interval '30 min' AND c.dropped_at
     GROUP BY c.ytno, c.jobtype, c.dropped_at
  )
  SELECT jobtype, count(*)::int n,
         percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (dropped_at - last_move)))::int med_s,
         percentile_cont(0.9) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (dropped_at - last_move)))::int p90_s
    FROM lm
   WHERE last_move IS NOT NULL AND dropped_at > last_move
     AND EXTRACT(EPOCH FROM (dropped_at - last_move)) BETWEEN 0 AND 1800
   GROUP BY jobtype;
CREATE UNIQUE INDEX learn_free_in_stationary_pk ON learn_free_in_stationary (jobtype);
