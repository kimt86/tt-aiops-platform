-- CHUNK7 7-2(a) 정정: e4의 K_EMPTY는 공차거리(UN_LNDN_TRV_RNG)가 본체인데 CHUNK4가
-- LNDN_TRV_RNG(적재거리)만 받아왔다. 타입 실측(사용자 확인): UN_LNDN_TRV_RNG = NUMBER(8,1)
-- -> f64, LNDN_TRV_RNG와 동일. CHECK 금지(RULES 2).
ALTER TABLE tos_handover_label ADD COLUMN IF NOT EXISTS un_trv_rng DOUBLE PRECISION;
