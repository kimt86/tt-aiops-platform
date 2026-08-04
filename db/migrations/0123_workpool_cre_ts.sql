-- 0123: 작업지시 **생성 시각**을 받아 저장한다.
--
-- ■ 왜
-- "TOS 가 실제 작업보다 얼마나 먼저 작업지시를 만드나"를 지금은 **재고 깊이로 간접 추정**만 했다
-- (크레인당 열린 지시 28개 ÷ 시간당 처리 20.8개 ≈ 1.27시간치). 직접 재려면 생성 시각이 필요하다.
--
-- 이 값이 왜 중요한가: 상자 단위 상세(컨테이너 번호·야드 위치·트윈)는 **작업지시가 만들어진 것만**
-- 존재한다. 실측 미완 35,149개 중 지시가 있는 건 1,495개(4.3%)뿐이다. 그 창이 얼마나 넓은지가
-- 배차 마감을 상자 단위로 역산할 수 있는 범위를 정한다.
--
-- ■ Oracle 부하 0
-- `CRE_DT` 는 이미 workpool.sql 의 WHERE 절이 쓰는 컬럼이다(`CRE_DT >= TRUNC(SYSDATE)-2`).
-- SELECT 목록에 한 줄 더한 것뿐이라 조회 계획도 부하도 바뀌지 않는다.
--
-- ⚠ TO_CHAR 로 문자열화해서 가져온다. DATE 를 그대로 두면 툴박스가 JSON 숫자로 바꿔
--   `Option<String>` 디코드가 **배치째** 실패한다(기존에 UPD_DT 에서 겪은 함정).
--
-- 멱등.

ALTER TABLE live_workpool ADD COLUMN IF NOT EXISTS cre_ts timestamptz;

COMMENT ON COLUMN live_workpool.cre_ts IS
  'JOB_ORDER_LIST.CRE_DT — 작업지시가 만들어진 시각. '
  '실제 작업(qc_move_log.comp_ts)과 대조하면 "지시 생성 → 작업"을 실측할 수 있다(mig 0123).';
