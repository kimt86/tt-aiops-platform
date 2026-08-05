-- 적부계획의 상자별 작업 순번(VSP_SHP_PLANSEQ). 구역 안 순서의 권위 값이다 — TOS 의 ITV 배차기도
-- 이것으로 정렬한다(LoadableJob.xml: ORDER BY JOB_QUE_PLND_DATE||TIME, VSP_SHP_PLANSEQ).
-- 작업지시 표에는 순서가 없다(MSNSEQ 는 전부 빔, SEQNO 는 발행시각→완료시각으로 덮어씀).
--
-- ⚠ 부하는 아래 세 조건이 전부다. 하나라도 빠지면 수백만 행짜리 질의가 된다.
--   ① DISLOAD='L'          적하 전용. 양하는 원천이 13.6% 만 채워져 있어 가져올 값이 없다.
--   ② COMPDATE IS NULL     아직 안 한 계획만 = 남은 일.
--   ③ (VESSEL,VOYAGE) IN   지금 작업중인 항차만. 목록은 Postgres 에서 만들어 넘긴다(Oracle 부하 0).
-- 실측(2026-08-05): 셋 다 걸면 2.3초/4,725행. ①만 빼면 16.4초/22,233행.
--
-- 인덱스 IDX_VSP_SHIP_VVCONT(VESSEL→VOYAGE→DISLOAD→CONTNO→…) 가 ①③ 을 그대로 받는다.
-- 스냅샷이다 — 계획은 개정되므로 매 주기 전체를 다시 읽고 표를 통째로 바꾼다.
SELECT
  v.VSP_SHP_VESSEL     AS vessel,
  v.VSP_SHP_VOYAGE     AS voyage,
  v.VSP_SHP_QUEUENAME  AS queuename,
  v.VSP_SHP_CONTNO     AS contno,
  v.VSP_SHP_PLANSEQ    AS planseq   -- NUMBER 다. 툴박스가 JSON 숫자로 주므로 i64 로 받는다
                                    -- (문자열로 받으면 배치째 디코드 실패 — 과거 실제 사고)
FROM TOSADM.VSP_SHIP v
WHERE v.VSP_SHP_DISLOAD   = 'L'
  AND v.VSP_SHP_COMPDATE IS NULL
  AND (v.VSP_SHP_VESSEL, v.VSP_SHP_VOYAGE) IN (__VOYAGES__)
