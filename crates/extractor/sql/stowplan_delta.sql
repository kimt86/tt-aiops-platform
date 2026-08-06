-- 적부계획 델타 조회(mig 0135). 5분 전체교체(stowplan.sql)를 UPD_DT 인덱스 델타로 바꾼다.
--
-- stowplan.sql과 다른 점 둘:
--   ① VSP_SHP_COMPDATE IS NULL 필터를 뺀다 — 완료된 행도 봐야 거울에서 지울 수 있다.
--      (완료가 UPD_DT를 갱신한다는 것은 실측됨: 오늘 63,750행 중 17,039 완료.)
--   ② v.UPD_DT >= TO_DATE('{wm}','YYYYMMDDHH24MISS') 를 더한다 — IDX_VSP_SHIP_UPD_DT 사용.
-- 나머지 세 조건(PLANST='P', DISLOAD IN, VESSEL/VOYAGE 목록)은 그대로 — 완화하면 표 전체로 번진다.
--
-- {wm}는 코드가 만든 14자 숫자 문자열만 들어간다(RULES: Oracle에 그대로 삽입되는 리터럴).
SELECT
  v.VSP_SHP_VESSEL     AS vessel,
  v.VSP_SHP_VOYAGE     AS voyage,
  v.VSP_SHP_QUEUENAME  AS queuename,
  v.VSP_SHP_DISLOAD    AS disload,
  v.VSP_SHP_CONTNO     AS contno,
  v.VSP_SHP_PLANSEQ    AS planseq,
  v.VSP_SHP_COMPDATE   AS compdate,               -- NOT NULL 이면 완료 → 거울에서 삭제
  TO_CHAR(v.UPD_DT,'YYYYMMDDHH24MISS') AS upd      -- 워터마크 전진용
FROM TOSADM.VSP_SHIP v
WHERE v.VSP_SHP_DISLOAD IN ('D', 'L')
  AND v.VSP_SHP_PLANST    = 'P'
  AND v.UPD_DT           >= TO_DATE('{wm}','YYYYMMDDHH24MISS')
  AND (v.VSP_SHP_VESSEL, v.VSP_SHP_VOYAGE) IN (__VOYAGES__)
