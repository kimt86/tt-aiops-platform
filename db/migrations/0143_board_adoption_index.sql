-- 0143: 배차 보드 채택률 계산용 프로브 인덱스.
-- 채택률 = 추천 상자(contno·최초 추천시각) vs TOS 실배차(tt_move_log.dispatch_ts 20분 내).
-- "트럭까지 일치" 판정이 stage2_match_shadow 를 (contno, ytno, ts) 로 프로브한다 —
-- PK 가 (ts, ytno) 라 contno 조회가 불가능했다(부분 인덱스: contno 는 mig 0142 이후만 값).
CREATE INDEX CONCURRENTLY IF NOT EXISTS stage2_match_shadow_contno_ts
  ON stage2_match_shadow (contno, ts) WHERE contno IS NOT NULL;
