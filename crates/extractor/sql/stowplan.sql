-- 적부계획의 상자별 작업 순번(VSP_SHP_PLANSEQ). 구역 안 순서의 권위 값이다 — TOS 의 ITV 배차기도
-- 이것으로 정렬하며, **적하와 양하가 같은 식**이다:
--     RANK() OVER (PARTITION BY 크레인 ORDER BY JOB_QUE_PLND_DATE||TIME, VSP_SHP_PLANSEQ) … WHERE RN=1
--     (com.clt.tos.itv.supervisor-impl/.../LoadableJob.xml — 양하 444·499, 적하 757)
-- 작업지시 표에는 순서가 없다(MSNSEQ 는 전부 빔, SEQNO 는 발행시각→완료시각으로 덮어씀).
--
-- ⚠ 부하는 아래 세 조건이 전부다. 하나라도 빠지면 수백만 행짜리 질의가 된다.
--   ① PLANST='P'           **이번 항차에서 할 일로 계획된 행만.**
--                          'B'(BAPLIE 신고분)는 순번이 없다 — 그걸 같이 세면 "양하는 13.6%뿐"이라는
--                          오판이 나온다(mig 0128 에서 내가 그렇게 틀렸다). TOS 도 P 만 쓴다.
--   ② COMPDATE IS NULL     아직 안 한 것만 = 남은 일. 실측상 P 행 수 = 큐 카운터의 남은 일.
--   ③ (VESSEL,VOYAGE) IN   지금 작업중인 항차만. 목록은 Postgres 에서 만들어 넘긴다(Oracle 부하 0).
--
-- ⚠ DISLOAD IN ('D','L') 은 **필터가 아니라 인덱스용**이다. IDX_VSP_SHIP_VVCONT 가
--   VESSEL→VOYAGE→DISLOAD→CONTNO 라, 값을 명시해야 선두 3컬럼을 그대로 탄다.
--
-- 소요(번갈아 3회 중앙값, 2026-08-05): 적·양하 **6.0초** / 적하만 4.1초.
-- ⚠ 편차가 크다(같은 질의가 4.2초와 15.2초) — **단발 측정으로 판단하지 말 것.**
-- 5분 주기라 시간당 Oracle 점유는 약 72초. 워크풀(4.3초 × 40회 = 173초)보다 가볍다.
--
-- 스냅샷이다 — 계획은 개정되므로 매 주기 전체를 다시 읽고 표를 통째로 바꾼다.
SELECT
  v.VSP_SHP_VESSEL     AS vessel,
  v.VSP_SHP_VOYAGE     AS voyage,
  v.VSP_SHP_QUEUENAME  AS queuename,
  v.VSP_SHP_DISLOAD    AS disload,   -- 'D'=양하 'L'=적하. 구역 이름으로 유추하지 않는다
  v.VSP_SHP_CONTNO     AS contno,
  v.VSP_SHP_PLANSEQ    AS planseq    -- NUMBER 다. 툴박스가 JSON 숫자로 주므로 i64 로 받는다
                                     -- (문자열로 받으면 배치째 디코드 실패 — 과거 실제 사고)
FROM TOSADM.VSP_SHIP v
WHERE v.VSP_SHP_DISLOAD IN ('D', 'L')
  AND v.VSP_SHP_PLANST    = 'P'
  AND v.VSP_SHP_COMPDATE IS NULL
  AND (v.VSP_SHP_VESSEL, v.VSP_SHP_VOYAGE) IN (__VOYAGES__)
