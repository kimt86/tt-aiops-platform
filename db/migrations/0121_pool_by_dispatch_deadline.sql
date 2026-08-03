-- 0121: 설계 ③ **1단계** — 배차 마감 기준으로 후보 작업 풀을 구성해 보되, **기록만** 한다.
--       실제 배차 판정은 종전 그대로. 두 풀이 어떻게 다른지 같은 틱에서 비교하기 위한 것.
--
-- ■ 설계 (사용자 확정, 2026-08-03)
--   "모든 작업을 `마감 − 여유` 가 이른 순으로 줄 세우고, 그 시각이 지난 것만 담는다.
--    담을 게 트럭보다 적으면 **트럭을 남긴다.** 억지로 채우지 않는다."
--
--   · 마감 = 크레인이 그 컨테이너를 다루는 시각 − 작업유형별 트럭 준비시간(mig 0120)
--   · 묶음 하나에 컨테이너가 여럿이면 **슬롯마다** 마감이 다르다:
--       슬롯 j 마감 = 베이시작 + j × 무브시간 − 준비시간
--     (실측 묶음 크기: 양하 중앙 14개 · 적하 중앙 1개)
--   · 여유 = 우리가 우리 예측을 못 믿는 만큼. 마감에 딱 맞추면 절반은 늦는다.
--
-- ■ 왜 트럭을 남기나
--   우리 출력은 나중에 TOS 가 읽어 **실제 배차 지시**가 된다. 마감에는 이미 이동시간이 들어 있으므로
--   더 일찍 보내면 트럭이 크레인 앞에서 기다리는 시간만 늘어난다. 실측: 지금 우리 추천은 100% 가
--   배차 마감까지 **중앙 15분**이 남아 있다 = 필요보다 15분 일찍 묶고 있다.
--   남은 트럭은 다음 틱에 다시 후보가 되므로 손해가 아니다.
--
-- ■ 이 마이그레이션이 기록하는 것
--   (a) **조용히 빠지는 작업 수** — 사용자 지시. 좌표가 없거나 작업시작 시각이 없어 후보에서 아예
--       제외되는 작업이 지금은 **세어지지 않는다**. 오늘 발견한 버그들이 전부 이렇게 숨어 있었다.
--   (b) 새 규칙 풀 vs 현행 풀의 크기·겹침, 남기는 트럭 수, 마감이 이미 지난 작업 수
--   (c) 묶음 단위 상세 — 두 풀 중 한쪽에라도 들어간 작업의 마감·순위·소속
--
-- 멱등.

-- (a)(b) 틱 단위 집계
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS works_no_eta    integer;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS works_no_coord  integer;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS pool_new_n      integer;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS pool_overlap_n  integer;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS trucks_held_n   integer;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS pool_overdue_n  integer;

COMMENT ON COLUMN stage2_solver_shadow.works_no_eta IS
  '작업시작 시각이 없어 후보에서 제외된 작업 수. 마감을 못 만들므로 새 규칙에서도 빠진다. '
  '조용히 빠지는 것을 막으려고 센다(mig 0121).';
COMMENT ON COLUMN stage2_solver_shadow.works_no_coord IS
  '좌표(적하=야드블록 / 양하=QC)를 못 찾아 제외된 작업 수.';
COMMENT ON COLUMN stage2_solver_shadow.pool_new_n IS
  '새 규칙(마감−여유 기준)이 담았을 묶음 수. 현행 n_works 와 비교한다.';
COMMENT ON COLUMN stage2_solver_shadow.pool_overlap_n IS
  '현행 풀과 새 규칙 풀에 **둘 다** 들어간 묶음 수.';
COMMENT ON COLUMN stage2_solver_shadow.trucks_held_n IS
  '새 규칙이었다면 이번 틱에 쓰지 않고 남겼을 트럭 수(담을 작업이 트럭보다 적을 때).';
COMMENT ON COLUMN stage2_solver_shadow.pool_overdue_n IS
  '새 규칙 풀 중 마감이 이미 지난 슬롯 수. 계속 0 이 아니면 선단이 수요를 못 따라간다는 신호.';

-- (c) 묶음 단위 상세
CREATE TABLE IF NOT EXISTS stage2_pool_shadow (
  ts                    timestamptz NOT NULL,
  qc                    text        NOT NULL,
  vessel                text        NOT NULL,
  queuename             text        NOT NULL,
  jobtype               text,
  n                     integer,     -- 이 묶음의 미배차 컨테이너 수
  work_eta_ts           timestamptz, -- 크레인이 이 베이를 시작하는 시각
  dispatch_deadline_ts  timestamptz, -- 그 시각 − 트럭 준비시간 (슬롯 0 기준)
  dd_slack_s            integer,     -- 마감 − 지금. 음수 = 이미 지남
  due_slots             integer,     -- 이번 틱에 마감이 도래한 슬롯 수 (= 새 규칙의 이 묶음 수요)
  in_current_pool       boolean,     -- 현행 Stage-1 이 담았나
  in_new_pool           boolean,     -- 새 규칙이 담았나
  rank_current          integer,     -- 현행 순서에서의 위치 (담긴 것만)
  rank_new              integer,     -- 새 규칙 순서에서의 위치
  PRIMARY KEY (ts, qc, vessel, queuename)
);
CREATE INDEX IF NOT EXISTS stage2_pool_shadow_ts ON stage2_pool_shadow (ts);

COMMENT ON TABLE stage2_pool_shadow IS
  '설계 ③ 1단계(mig 0121): 마감 기준 후보 풀을 계산만 해서 현행 풀과 나란히 기록한다. '
  '배차 판정에는 쓰지 않는다. 두 풀 중 한쪽에라도 들어간 묶음만 남긴다.';
