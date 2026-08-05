-- 0128: **적부계획 순번**(VSP_SHIP.VSP_SHP_PLANSEQ)을 실어온다. 구역 안 상자 순서의 진짜 출처다.
--
-- ■ 왜 필요한가
-- 구역 안에서 "이 상자가 몇 번째인가"를 우리는 작업지시 생성시각으로 **추정**하고 있었다.
-- 그런데 작업지시 표에는 순서가 없다:
--   · `JOB_ODR_MSNSEQ` : 660/660 전부 비어 있음
--   · `JOB_ODR_SEQNO`  : 열린 지시에서는 **발행 시각**(cre_ts 와 ±18초), 완료되면 **완료 시각**으로
--                        덮어쓰인다. 그래서 끝난 작업으로 채점하면 100% 로 보이는 함정이 있다.
--                        (같은 함정 3번째: st_ts → ETW → SEQNO)
--   · `JOB_ODR_POINT`  : 순서 아님(단독 49% = 무작위, 더해도 62.8→63.2%)
-- 그 결과 상자 79.5% 가 다른 상자와 같은 값을 공유하고(최대 26개 한 묶음), 순번이 사실상 임의였다.
--
-- ■ TOS 자신은 어디를 보는가 (OSS 소스 확인)
-- ITV 배차기가 크레인별 작업을 고르는 정렬이 이것이다:
--     ORDER BY JOB_QUE_PLND_DATE||JOB_QUE_PLND_TIME,  -- 구역의 계획 시각
--              VSP_SHP_PLANSEQ                        -- ★적부계획 순번
--   (com.clt.tos.itv.supervisor-impl/src/ibatissql/LoadableJob.xml:444,499,757)
-- 즉 순서는 작업지시가 아니라 **선박 적부계획 표**에 있다. 우리는 그 표를 안 보고 있었다.
--
-- ■ 실측 (2026-08-05)
--   · 적하 PLANSEQ **100% 채워짐**(4,725/4,725). 양하는 13.6% 뿐이라 **적하 전용**이다.
--   · 알갱이: 구역당 상자 중앙 35개 · 구역 안 서로 다른 순번값 중앙 35개
--     ⇒ **값 하나당 1.0 상자 = 상자 단위 진짜 순번.**
--   · 부하: 적하·미완료·작업중 항차로 좁히면 **2.3초 / 4,725행**(기존 워크풀 질의 기준선 1.2초).
--     좁히지 않으면 16.4초/22,233행이라 반드시 좁혀서 쓴다.
--
-- ■ 범위를 좁히는 세 조건 (부하의 전부가 여기서 결정된다)
--   ① `VSP_SHP_DISLOAD='L'`        — 양하는 값이 없으니 가져올 이유가 없다
--   ② `VSP_SHP_COMPDATE IS NULL`   — 이미 끝난 계획 행 제외 = 남은 일만
--   ③ (선박,항차) IN (지금 작업중인 항차)  — 우리 Postgres 에서 만들어 넘긴다(Oracle 부하 0)
--   ⚠ 셋 중 하나라도 빠지면 표 전체(수백만 행) 쪽으로 넘어간다.
--
-- ■ 인덱스 (조회 전 확인함)
--   IDX_VSP_SHIP_QNAME  : VESSEL→VOYAGE→QUEUENAME→PLANSEQ→…  (구역 단위 정렬에 적합)
--   IDX_VSP_SHIP_VVCONT : VESSEL→VOYAGE→DISLOAD→CONTNO→…     (위 ①③ 에 적합)
--   IDX_VSP_SHIP_UPD_DT : UPD_DT                              (증분 전환이 필요해지면 여기로)
--
-- ■ 이 표의 성격
-- 스냅샷이다(매 주기 전체 교체). 계획은 개정되고, 우리는 "지금 계획"만 필요하다.
-- ⚠ 작업지시가 **없는 상자도 들어온다** — 그게 이 표의 존재 이유다. 우리 작업 목록은 지금
--    작업중인 구역 남은 일의 44% 만 담고 있어서, 순번을 그 안에서만 매기면 늘 앞자리가 나왔다.
--
-- 멱등.

CREATE TABLE IF NOT EXISTS live_stow_plan (
  vessel     text        NOT NULL,
  voyage     text        NOT NULL,
  queuename  text        NOT NULL,
  contno     text        NOT NULL,
  planseq    integer,
  as_of_ts   timestamptz NOT NULL,
  PRIMARY KEY (vessel, voyage, queuename, contno)
);

CREATE INDEX IF NOT EXISTS live_stow_plan_cont_idx ON live_stow_plan (contno);
CREATE INDEX IF NOT EXISTS live_stow_plan_q_idx    ON live_stow_plan (vessel, voyage, queuename, planseq);

COMMENT ON TABLE live_stow_plan IS
  '적부계획의 상자별 작업 순번(TOS VSP_SHIP). 구역 안 순서의 권위 값 — TOS 배차기도 이것으로 '
  '정렬한다(LoadableJob.xml). **적하 전용**(양하는 원천이 13.6% 만 채워져 있다). '
  '매 주기 전체 교체되는 스냅샷. 작업지시가 아직 없는 상자도 포함한다. mig 0128.';

COMMENT ON COLUMN live_stow_plan.planseq IS
  'VSP_SHP_PLANSEQ — 구역 안 상자 순번. 실측상 값 하나당 상자 1.0개(진짜 상자 단위 순번). '
  '작업지시의 seqno/cre_ts 는 최대 26개가 한 값을 공유해 순번 구실을 못 한다.';
