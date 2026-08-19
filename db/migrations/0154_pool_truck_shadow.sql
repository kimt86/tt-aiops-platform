-- 0154 — Stage-2 후보 풀의 트럭 단위 기록 (pull 구조 재정의 1/2 · 2026-08-19)
--
-- 왜: 지금까지 미배정 후보 트럭은 어디에도 남지 않아 "트럭이 요청한 순간 우리 풀에 있었나"(풀 재현율)를
-- 잴 수 없었다. 이 표는 매 매칭 틱마다 풀에 든 트럭 전부를 남긴다(배정 여부와 무관).
-- 규모: 틱당 ~200~300행 × 1,440틱/일 ≈ 40만 행/일. 3일 보관(livemap.rs 매처 루프에서 프룬).
--
-- reason(포함 사유):
--   free_tos      = 원천 드랍 로그(적하 qc_move_log LD · 양하 tos_handover_label DS)에 자유가 찍혔고 그 뒤 새 배차
--                   (live_workpool.yt_dis_ts) 없음. GPS 라벨과 무관. 자유까지 0.
--   gps_free      = TOS 에 최근 3h 기록이 없는데 GPS 가 신선하고 빈 차(idle/staging/empty_travel)로 보임(신규 투입 등).
--   inflight_*    = 배차 중인 트럭 중 예측 자유까지 시간 ≤ H(POOL_FREE_HORIZON_S). anchor=무브로그 픽업 앵커,
--                   gps=GPS 상태 학습값(soon_idle/wait_rtg), held=침묵+짐 실음+드랍 근접(옛 가지·앵커 없을 때만).
-- pos_src(위치 출처): gps_live(≤120s) · gps_stale(장치 목록에 남은 낡은 픽스 ≤600s) · pos_hist(truck_pos_hist 마지막 행)
--                   · drop_est(드랍 지점 추정).
-- pool_ver: 1 = 이 재정의(2026-08-19~). 이전 판은 이 표에 없다(NULL 구간 없음).

CREATE TABLE IF NOT EXISTS stage2_pool_truck_shadow (
  ts         timestamptz NOT NULL,
  ytno       text        NOT NULL,
  reason     text        NOT NULL,
  free_in_s  integer,
  pos_src    text,
  gps_age_s  integer,
  pool_ver   smallint    NOT NULL DEFAULT 1,
  PRIMARY KEY (ts, ytno)
);
CREATE INDEX IF NOT EXISTS stage2_pool_truck_ytno_ts ON stage2_pool_truck_shadow (ytno, ts);

COMMENT ON TABLE stage2_pool_truck_shadow IS
  'Stage-2 후보 풀에 든 트럭(배정 여부 무관), 매 매칭 틱. 풀 재현율 측정용. 3일 보관. pool_ver=1 (2026-08-19 pull 재정의).';
COMMENT ON COLUMN stage2_pool_truck_shadow.reason IS
  'free_tos | gps_free | inflight_anchor | inflight_gps | inflight_held — 왜 풀에 들었나. 재현율은 이 값으로 가른다.';
COMMENT ON COLUMN stage2_pool_truck_shadow.free_in_s IS '매처가 쓴 자유까지 초(비용 base). 0 = 지금 빔.';
COMMENT ON COLUMN stage2_pool_truck_shadow.pos_src IS 'gps_live | gps_stale | pos_hist | drop_est — 매처가 쓴 위치의 출처.';
COMMENT ON COLUMN stage2_pool_truck_shadow.gps_age_s IS '그 틱에서 마지막 GPS 픽스 나이(초). NULL = 픽스 없음.';
