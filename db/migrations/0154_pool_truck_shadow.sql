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
-- pool_ver: 1 = 첫 배포(2026-08-19 12:57 MYT~) · 2 = 픽업 가드 + 앵커 status 필터 제거(15:09 KST~). 재현율은 판으로 가른다.

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

-- ⚠COMMENT 는 이 파일이 아니라 **0156 이 소유한다**(2026-08-21 2차 리뷰). 스키마는 멱등이지만 COMMENT 는 마지막에
-- 실행된 파일이 이기므로, 여기에도 두면 0154 를 다시 돌릴 때 0156 의 정정(pool_ver 1→5 등)이 되돌아간다.
