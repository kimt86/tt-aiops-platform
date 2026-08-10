-- 신규 활성 항차 시딩(2026-08-10 점검에서 발견된 구멍의 수리).
--
-- 접안으로 활성 목록에 막 들어온 항차의 계획 행들은 UPD_DT 가 과거(계획은 접안 전 작성)라
-- stowplan_delta.sql 의 워터마크 조건에 절대 걸리지 않는다. 종전에는 시간당 화해(recon)가
-- 메울 때까지 최대 1시간 동안 그 배의 순번(planseq)이 거울에 없었다(실측 drift 스파이크
-- 5,164행이 항차 13에서 15로 늘던 시각과 일치).
--
-- 이 질의는 델타 질의 뒤에 UNION ALL 로 붙어 "거울에 행이 0개인 활성 항차"만 통째로
-- 읽는다 — 별도 왕복이 아니라 같은 왕복이다(왕복 증가 0). 인덱스는 스냅샷과 동일하게
-- IDX_VSP_SHIP_VVCONT 선두 3컬럼(VESSEL, VOYAGE, DISLOAD 명시)을 탄다.
--
-- 컬럼 목록은 stowplan_delta.sql 과 자리·이름이 같아야 한다(UNION ALL + 한 배치 파싱).
-- 끝나가는 항차(전 행 COMPDATE 채워짐 = 거울 0행)도 목록에 걸리지만 여기 조건으로
-- 0행이 돌아와 무해하다 — 그 항차가 활성 목록을 떠날 때까지 몇 틱 헛짚는 비용은
-- 같은 왕복 안의 술어 몇 개뿐이다.
SELECT
  v.VSP_SHP_VESSEL     AS vessel,
  v.VSP_SHP_VOYAGE     AS voyage,
  v.VSP_SHP_QUEUENAME  AS queuename,
  v.VSP_SHP_DISLOAD    AS disload,
  v.VSP_SHP_CONTNO     AS contno,
  v.VSP_SHP_PLANSEQ    AS planseq,
  v.VSP_SHP_COMPDATE   AS compdate,
  TO_CHAR(v.UPD_DT,'YYYYMMDDHH24MISS') AS upd
FROM TOSADM.VSP_SHIP v
WHERE v.VSP_SHP_DISLOAD IN ('D', 'L')
  AND v.VSP_SHP_PLANST    = 'P'
  AND v.VSP_SHP_COMPDATE IS NULL
  AND (v.VSP_SHP_VESSEL, v.VSP_SHP_VOYAGE) IN (__SEED_VOYAGES__)
