-- 0144: 채택률 시계열 — 배차 보드의 24h 즉석 계산을 시간별 점으로 축적한다.
-- 목적: TOS 공유 제안서에 넣을 "추천 vs 실배차 일치 추이"와 보드의 스파크라인.
-- 적재: spawn_dispatch_pred_logger 가 10분 주기로 시도, 최근 55분 내 행이 있으면 건너뜀
-- (시간당 ~1점). 창은 산출 시점 기준 직전 24시간(보드 즉석 계산과 같은 잣대·같은 쿼리).
CREATE TABLE IF NOT EXISTS dispatch_adoption_metric (
  captured_at     timestamptz PRIMARY KEY DEFAULT now(),
  window_h        int NOT NULL,
  boxes_reco      bigint NOT NULL,
  boxes_dispatched bigint NOT NULL,
  box_pct         float8,
  ytno_match_pct  float8
);
COMMENT ON TABLE dispatch_adoption_metric IS
  '추천 채택률 시계열(시간당 ~1점). boxes_reco = 직전 24h 추천 상자 수(contno·최초 추천시각), '
  'boxes_dispatched = 그중 최초 추천 후 20분 내 TOS 실배차(tt_move_log). ytno_match = 트럭까지 '
  '일치. contno 는 mig 0142(2026-08-10)부터라 그 이전 구간은 분모가 얕다 (mig 0144).';
