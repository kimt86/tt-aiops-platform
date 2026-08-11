-- 0151: D_tos(TOS 가 이 트럭을 배차한 시각)를 **전용 컬럼**으로 분리한다.
--
-- ■ 무엇을 고치는가 — 같은 날 내가 만든 오염
-- mig 0149 사이클에서 `dispatch_pred_sample.tos_upd_dt` 에 쓰는 **두 경로 중 하나만** 원천을
-- `yt_dis_ts` 로 바꿨다. 결과로 한 컬럼에 두 정의가 섞였다:
--
--   block(0) workpool.rs — 나중에 배차된 상자 → UPD_DT   (24시간 실측 8,454행 · 81%)
--   block(3) workpool.rs — 첫 기록 때 이미 배차된 상자 → YT_DIS_DT (1,924행 · 19%)
--
-- 두 경로는 `became_assigned_at IS NULL` 로 행 단위 상호배타라 섞인 게 눈에 안 띈다. 더 나쁜
-- 것은 **가르는 선이 배차 시점 그 자체와 상관**된다는 점이다 — "우리가 처음 봤을 때 이미
-- 배차돼 있었나"가 곧 층화 변수가 되므로, D_tos 를 쓰는 분석은 조용히 두 정의를 섞는다.
-- 이 저장소가 반복해서 데인 함정(결과에서 파생된 변수로 층화)과 같은 모양이다.
--
-- ■ 어떻게 고치는가
-- 기존 컬럼의 뜻을 바꾸지 않는다. `tos_upd_dt` 는 **두 경로 모두 UPD_DT** 로 되돌리고(원래 뜻),
-- 권위값은 새 컬럼 `tos_dis_ts` 에 **두 경로 모두** 담는다. 이름이 값과 맞고, 과거 구간의 뜻도
-- 그대로 보존된다.
--
--   tos_upd_dt  = UPD_DT (행 마지막 갱신) — "배차 시각의 상한"이라는 원래 라벨 그대로
--   tos_dis_ts  = YT_DIS_DT (배차 시각 실물·mig 0148) — D_tos 분석은 이걸 쓴다
--
-- 경계: 2026-08-11. 그 이전 구간은 `tos_dis_ts` 가 NULL 이다(소급하지 않는다 — live_workpool 은
-- 매 틱 통째 교체되는 스냅샷이라 지나간 행의 YT_DIS_DT 가 남아 있지 않다).
--
-- ⚠ mig 0048 이 이 컬럼을 "UPD_DT" 로 문서화한 것은 이제 다시 맞다.
--
-- 멱등.

ALTER TABLE dispatch_pred_sample ADD COLUMN IF NOT EXISTS tos_dis_ts timestamptz;

COMMENT ON COLUMN dispatch_pred_sample.tos_dis_ts IS
  'TOS 가 이 트럭을 배차한 시각(JOB_ORDER_LIST.YT_DIS_DT · mig 0151). D_tos 분석의 권위값. '
  '2026-08-11 이전은 NULL(소급 불가 — live_workpool 은 매 틱 교체되는 스냅샷이다).';

COMMENT ON COLUMN dispatch_pred_sample.tos_upd_dt IS
  'TOS 행 마지막 갱신 시각(UPD_DT) — 배차 시각의 **상한**이다(mig 0151 로 원래 뜻 복원). '
  '⚠배차 시각 자체가 필요하면 tos_dis_ts 를 쓸 것. 둘의 격차는 실측 중앙 0초 · p90 1,382초.';
