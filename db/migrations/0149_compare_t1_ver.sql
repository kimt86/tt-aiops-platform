-- 0149: 비교기의 **T1(트럭 위치를 되감는 시각)** 원천이 바뀌었음을 판(ver)으로 가른다.
--
-- ■ 무엇이 바뀌었나
-- `spawn_dispatch_compare` 와 fair_compare 는 "TOS 가 배차한 그 순간 트럭들이 어디 있었나"를
-- 되감아 우리 선택과 비교한다. 그 순간(T1)을 **`upd_ts`(행 마지막 갱신)로 잡고 있었다.**
-- mig 0148 이 이게 배차 시각이 아님을 실측으로 보였다:
--
--   배차행 175건 중 90건(51.4%)이 `upd_ts <> yt_dis_ts` · 격차 p90 2,170초(36분)
--
-- 절반가량이 실제 배차보다 최대 수십 분 뒤의 순간으로 트럭 위치를 되감고 있었다는 뜻이다.
-- T1 을 `yt_dis_ts`(= TOS `YT_DIS_DT`)로 바꿨다.
--
-- ■ 왜 판을 가르나
-- 같은 표에 **같은 컬럼인데 뜻이 다른 두 구간**이 생긴다. 섞어 읽으면 "비교기가 개선됐다"와
-- "모집단이 달라졌다"가 구별되지 않는다(pool_mode·pred_ver·bias_ver 전례와 같은 상황).
--
--   t1_ver IS NULL  → T1 = upd_ts   (2026-08-11 경계 이전 · 77만 행)
--   t1_ver = 1      → T1 = yt_dis_ts (경계 이후)
--
-- **경계 이전 행을 고치지 않는다.** 되감을 원본(그 시점 트럭 위치)이 이미 없다.
--
-- ■ 키는 그대로 둔다
-- PK `(qc, queuename, tos_ytno, tos_upd)` 의 `tos_upd` 는 계속 `upd_ts` 를 담는다. 중복 제거용
-- 토큰으로는 여전히 유효하고, 여기까지 바꾸면 PK 의미가 바뀌면서 백로그 77만 행이 통째로
-- 재비교된다. 결함은 키가 아니라 T1 이었다.
--
-- 멱등.

ALTER TABLE dispatch_compare_shadow ADD COLUMN IF NOT EXISTS t1_ver smallint;

COMMENT ON COLUMN dispatch_compare_shadow.t1_ver IS
  '트럭 위치를 되감은 시각(T1)의 원천 판별자(mig 0149). '
  'NULL = upd_ts(행 마지막 갱신 · 2026-08-11 경계 이전) / 1 = yt_dis_ts(TOS 배차 시각 실물). '
  '집계할 때 반드시 이 값으로 먼저 가를 것 — NULL 구간은 절반가량이 실제 배차보다 '
  '뒤(격차 p90 2,170초)의 순간으로 되감겨 있다.';

COMMENT ON COLUMN dispatch_compare_shadow.tos_upd IS
  'TOS 행 마지막 갱신 시각(UPD_DT). ⚠배차 시각이 아니다 — 중복 제거용 키로만 쓴다(mig 0149). '
  '배차 시각은 live_workpool.yt_dis_ts 이고, T1 은 t1_ver=1 부터 그 값을 쓴다.';
