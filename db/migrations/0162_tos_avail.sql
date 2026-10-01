-- 0162 — TOS 가 "배차 가능"하다고 보는 트럭 목록의 시간표 (2026-09-30, 2026-10-01 커밋 단위 재생으로 개정)
--
-- ■ 무엇
-- TOS 배차기는 `tlc.mover.getMoverList`(V$SQL sql_id d0bfnbyhqq52j, 초당 ~12회 실행)로 배차 가능 트럭을 뽑는다:
--     CDY_MACHINE.ONOFF='O' ∧ 유형 SC/YT/AT
--     ∧ JOB_ORDER_LIST 에 YTNO=그 트럭 ∧ NVL(YT_STATUS,'X') IN ('A','B','F','R','X','Q') 인 행이 없음
--     ∧ 열린 장비정지(MCH_EQ_WORKSTOP) 없음
-- 트럭은 이 목록에 몇 초만 머문다 — 1분마다 찍어서는 거의 못 본다. 그래서 1분마다 **같은 순간의 스냅샷 + 그 사이
-- 변경 기록**(JOB_ORDER_HISTORY I/U/D 와 행마다의 커밋 번호 ORA_ROWSCN, 로그인·로그아웃 시각)을 받아, 직전 스냅샷에서
-- **커밋 순서대로, 커밋 하나를 한 걸음으로** 다시 짜 맞춘다(extractor `tos-avail`, 유닛 tt-tos-avail).
-- 커밋 단위인 이유: 스왑은 한 커밋 안에서 "떼기 → 붙이기"가 따로 찍히지만 그 사이는 남에게 보인 적이 없고,
-- "작업 끝 → 신규 배차"는 191쌍 전부 다른 커밋이었다(2026-10-01 15분 실측, 1초 미만 2쌍 포함).
--
-- ■ 정확도
--   · 사전 실측(2026-09-30 14:13~14:23 MYT, 2초 간격 정답 297회, 커밋 단위 이전 방식): 목록에 있던 (트럭,순간)
--     1,281쌍 중 97.3% 를 맞게 올렸고 우리가 올린 것의 96.1% 가 맞았다. 로그온을 빼면 정밀도 67%.
--     (해석은 따로: 로그아웃 때 TOS 가 그 트럭의 작업을 떼어내는 것이 관찰됐고 — 로그온을 모르면 그 순간 트럭이
--      "풀린 것"처럼 보인다. 가설.)
--   · 운영 중에는 tos_avail_check 가 매 틱 **스냅샷 순간 한 점**에서 잰다(틱 사이 순간의 오류는 못 본다).
--
-- ■ 늦음: 목록은 **사후에** 정확하다. 1분마다 수집하므로 "지금"은 최대 ~1분 전까지만 안다.
--   tos_avail_anchor.as_of_ts 이후 시각에 대해서는 아무것도 말하지 않는다.

CREATE TABLE IF NOT EXISTS tos_avail_interval (
  ytno          text        NOT NULL,
  enter_ts      timestamptz NOT NULL,
  exit_ts       timestamptz,
  enter_cause   text        NOT NULL,
  exit_cause    text,
  enter_program text,
  exit_program  text,
  exit_contno   text,
  exit_jobtype  text,
  pool          text,
  span          tstzrange GENERATED ALWAYS AS (tstzrange(enter_ts, exit_ts, '[)')) STORED,
  PRIMARY KEY (ytno, enter_ts)
);
-- 트럭당 열린 구간은 하나뿐이다(수집기가 이 제약에 기대 중복 등재를 건너뛴다).
CREATE UNIQUE INDEX IF NOT EXISTS tos_avail_interval_one_open ON tos_avail_interval (ytno) WHERE exit_ts IS NULL;
-- "시각 t 에 목록에 누가 있었나" = WHERE span @> t
CREATE INDEX IF NOT EXISTS tos_avail_interval_span ON tos_avail_interval USING gist (span);

COMMENT ON TABLE tos_avail_interval IS
  'TOS 배차 가능 목록(getMoverList)에 트럭이 머문 구간. 시각 t 의 목록 = WHERE span @> t '
  '— 단 t <= tos_avail_anchor.as_of_ts 일 때만 유효(exit_ts NULL 은 "마지막 수집 때 아직 목록에 있음"이지 지금도라는 뜻이 아니다). '
  'exit_cause=gap 구간의 끝과 그 뒤 첫 init 사이는 모르는 구간이다 — 빈 목록과 구별할 것. '
  '장비정지는 스냅샷 때만 반영된다(틱 사이 변화는 다음 스냅샷의 reconcile 로). '
  '시각은 커밋 순간의 하한(그 커밋의 마지막 기록 시각)이고 걸음마다 1µs 이상 뒤로 민다. '
  '⚠ 목록에 있다 ≠ 배차 후보: 풀이 없거나(pool NULL) 선박 작업과 무관한 풀의 트럭도 몇 시간씩 목록에 있다 — '
  '선박 배차 비교에는 풀로 거를 것. mig 0162.';
COMMENT ON COLUMN tos_avail_interval.enter_cause IS
  'release=마지막 바쁜 작업지시가 풀림 · login=로그인 · late=앵커 이전 시각에 찍혔지만 앵커 뒤에 커밋된 변경(앵커 직후에 붙임) · '
  'reconcile=스냅샷 대조로 바로잡음(재생이 놓친 것) · init=첫 수집/공백 뒤 재시작';
COMMENT ON COLUMN tos_avail_interval.exit_cause IS
  'dispatch=작업지시가 붙음(배차·스왑·수동 포함, 누가 붙였는지는 exit_program) · logout · late · reconcile · '
  'gap=수집 공백(또는 Oracle 시계 역행)으로 끝을 모름 — 마지막으로 안 시각(시계 역행이면 새 스냅샷 시각)에 닫음, '
  '실제로 그때 빠졌다는 뜻 아님';
COMMENT ON COLUMN tos_avail_interval.enter_program IS
  '트럭을 풀어준 변경의 JOB_HIST_PROGRAM — 그 트럭이 붙어 있던 행이 끝났거나(C) 다른 트럭으로 갔거나 지워진 변경. '
  'login/reconcile/init 은 NULL. release 인데 NULL 이면 그 변경의 프로그램 값 자체가 비어 있던 것.';
COMMENT ON COLUMN tos_avail_interval.exit_program IS
  '트럭을 바쁘게 만든 변경의 JOB_HIST_PROGRAM(예: NEW_ITV_DISPATCHING). 한 커밋에 여럿이면 그중 마지막.';
COMMENT ON COLUMN tos_avail_interval.exit_contno IS 'exit_cause=dispatch 일 때 트럭에 붙은 작업지시의 컨테이너.';
COMMENT ON COLUMN tos_avail_interval.pool IS 'CDY_MACHINE.CDY_MCHN_YTPOOLNAME — 등재를 쓴 틱의 끝 스냅샷 값(등재 순간보다 최대 ~60초 뒤). TOS 목록 조건에는 풀이 없다 — 참고·필터용.';

CREATE TABLE IF NOT EXISTS tos_avail_check (
  as_of_ts      timestamptz PRIMARY KEY,
  prev_as_of_ts timestamptz,
  mode          text NOT NULL,
  n_truth       int  NOT NULL,
  n_replay      int,
  n_match       int,
  missing       text[],
  extra         text[],
  n_events      int,
  n_late        int,
  n_logon       int,
  n_txn         int,
  n_desync      int
);
-- 2026-09-30 판에서 온 표: 커밋 단위 재생으로 바꾸며 1초 문턱(n_flicker)을 없앴다.
ALTER TABLE tos_avail_check ADD COLUMN IF NOT EXISTS n_txn int;
ALTER TABLE tos_avail_check ADD COLUMN IF NOT EXISTS n_desync int;
ALTER TABLE tos_avail_check DROP COLUMN IF EXISTS n_flicker;

COMMENT ON TABLE tos_avail_check IS
  '매 수집 틱의 정확도 점검. 직전 스냅샷(prev_as_of_ts)에서 변경 기록만으로 재생한 목록(n_replay)을 '
  '이번 스냅샷의 실제 목록(n_truth)과 대조한 것 — 재생 창이 가장 긴 순간(~60초)의 **한 점**이다. '
  '틱 사이에 생겼다 사라진 오류는 여기 안 잡힌다. '
  '재현율 = sum(n_match)/sum(n_truth), 정밀도 = sum(n_match)/sum(n_replay) (mode=''replay'' 만). mig 0162.';
COMMENT ON COLUMN tos_avail_check.mode IS
  'replay=정상 재생 · init=첫 수집 · gap=직전 스냅샷이 너무 오래됐거나(수집 공백 15분 초과) Oracle 시계가 앵커보다 '
  '뒤로 가 재생하지 않음 — init/gap 은 n_replay NULL';
COMMENT ON COLUMN tos_avail_check.missing IS 'TOS 목록에 있는데 재생에 없던 트럭(재현 실패).';
COMMENT ON COLUMN tos_avail_check.extra IS '재생에 있는데 TOS 목록에 없던 트럭(잘못 올림).';
COMMENT ON COLUMN tos_avail_check.n_events IS '새로 적용한 변경 기록 행 수(커밋 번호가 앵커 상한보다 큰 것).';
COMMENT ON COLUMN tos_avail_check.n_txn IS '새로 적용한 커밋 수 — 재생의 걸음 수.';
COMMENT ON COLUMN tos_avail_check.n_late IS '앵커 이전 시각에 찍혔지만 앵커 뒤에 커밋된 기록 때문에 상태가 바뀐 트럭 수.';
COMMENT ON COLUMN tos_avail_check.n_desync IS
  '구간 표 쓰기가 기대와 달랐던 수 — 등재가 이미 열린 구간 때문에 건너뛰어졌거나, 이탈에 닫을 열린 구간이 없었다. 0 이 정상.';

CREATE TABLE IF NOT EXISTS tos_avail_anchor (
  id        smallint PRIMARY KEY DEFAULT 1 CHECK (id = 1),
  as_of_ts  timestamptz NOT NULL,
  snapshot  jsonb       NOT NULL
);
COMMENT ON TABLE tos_avail_anchor IS
  '직전 수집 스냅샷(한 행) — 다음 틱의 재생 출발점. snapshot = {rows:[[contno,point,seqno,ytno,ys]], '
  'machines:{code:[onoff,pool]}, stopped:[code], scn:이 스냅샷에 이미 반영된 커밋 번호 상한(보인 기록 중 최대), '
  'last_ts:구간 표에 쓴 가장 늦은 전이 시각, last_on/last_off:이 스냅샷이 본 트럭별 마지막 로그인·로그아웃 시각}. '
  '다음 틱은 커밋 번호가 scn 보다 큰 기록과, 앵커가 본 값과 달라진 로그인·로그아웃만 적용한다. '
  '이 행을 지우면 다음 틱이 init 으로 새로 시작한다(열린 구간은 마지막 점검 시각에 gap 으로 닫힘). mig 0162.';
