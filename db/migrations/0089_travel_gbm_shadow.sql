-- 이동시간 GBM 그림자 (2026-07-13): 배차 미변경. Python 배치(scripts/travel_gbm_shadow.py)가 새로
-- 완료된 trip을 GBM(LightGBM)과 현재 OD-중앙값 모델 둘 다로 예측해 실제와 함께 로깅한다. 오프라인
-- 벤치마크(시간분할)에서 GBM이 MAPE 57→49% 이겼으나, 라이브 out-of-sample에서도 이기는지 검증용.
-- 학습은 dropped_at < 학습시점 데이터만 → 채점 trip은 항상 미학습(무누수). 하루 1회 재학습.
CREATE TABLE IF NOT EXISTS travel_gbm_shadow (
  ytno             TEXT NOT NULL,
  dropped_at       TIMESTAMPTZ NOT NULL,
  leg_ord          INT NOT NULL,
  origin           TEXT,
  dest             TEXT,
  dist_m           DOUBLE PRECISION,
  actual_s         INT,               -- 실제 이동시간
  gbm_s            INT,               -- GBM 예측
  base_s           INT,               -- 현재 OD-중앙값 예측
  model_trained_at TIMESTAMPTZ,       -- 이 예측을 낸 모델의 학습 시각(무누수 감사)
  scored_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (ytno, dropped_at, leg_ord)
);
CREATE INDEX IF NOT EXISTS travel_gbm_shadow_scored ON travel_gbm_shadow (scored_at);
