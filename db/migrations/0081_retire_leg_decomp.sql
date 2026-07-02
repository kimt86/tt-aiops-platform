-- learn_leg_decomp 완전 retire.
-- 배차 비용(순수주행 구간시간)은 mig 0080에서 learn_travel_sample 빈트립(leg_ord=0)으로 이관됨.
-- leg_decomp가 남아 떠받치던 것들:
--   · 러닝센터 ① 카드의 drive/stop 분해(주행 22.8 vs 실효 13km/h) → 제거(구간속도만 표시)
--   · road_route_eval(도로망 라우팅 게이트, 이미 부정) → 일시중지(그래프 인프라·테이블은 보존)
-- 30초 GPS 모션분할 적재 잡(spawn_leg_decomp)도 제거됨. 필요 시 git 이력에서 복원 가능.
DROP TABLE IF EXISTS learn_leg_decomp;
