-- 0148: TOS **배차 시각**을 대리값이 아니라 실물 컬럼으로 받는다.
--
-- ■ 왜
-- 지금까지 "TOS 가 이 트럭을 언제 붙였나"를 `UPD_DT`(행 마지막 갱신)로 **대신** 읽었다.
-- workpool.sql 주석도 "UPD_DT ≈ dispatch time (D_tos)" 라고 근사임을 적어뒀다.
-- 근사인 이유가 실측으로 확인됐다(2026-08-11, YTNO 붙은 373행):
--
--   UPD_DT - YT_DIS_DT :  중앙 0초 · 5초 이내 230/373(61.7%) · p90 1,382초 · 최대 12,757초
--
-- 갓 배차된 행은 둘이 같지만, 행이 나중에 또 갱신되면 `UPD_DT` 만 뒤로 밀린다. 즉
-- **배차 시각의 고정 앵커가 아니다.** `YT_DIS_DT` 는 밀리지 않는다.
--
-- ■ 문서 3건이 어긋나 있었다 — 실측으로 닫음
-- mig 0048 : "JOB_ORDER_LIST 는 YT_DIS_DT=**after-arrival** 만 있다"  → **틀렸다**
-- mig 0092 · 0115 : "YT_DIS_DT = TOS 가 이 트럭을 배차한 순간"        → **맞다**
-- mig 0030:15 : tos_handover_label.dis_ts 를 "truck arrived / discharged at block (YT_DIS_DT)"
--   라고 적었다 → 0048 과 같은 틀린 뜻이다. ⚠단 그쪽은 JOB_ORDER_**HISTORY** 라 아래 프로브가
--   덮은 범위가 아니다. 그 표에 대한 근거는 mig 0115 ④(dis_ts 가 ST_DT=배차시각과 항등)에 있고,
--   여기서는 **서술이 같은 방식으로 틀렸다는 사실만** 적어둔다.
--
-- 근거(TOSADM.JOB_ORDER_LIST · JOB_ODR_COMPDATE 가 비고 · JOBSTATUS in (A,B,Q) · YTNO 채워짐
--      · CRE_DT >= TRUNC(SYSDATE)-1 → **370행** · 2026-08-11):
--
--   상태 A · 작업미시작(ACTV_DT 비어 있음)  176행 → YT_DIS_DT 전부 채워짐
--   상태 Q · 작업미시작                     141행 → YT_DIS_DT 전부 채워짐
--   상태 A · 작업시작함                      51행 → YT_DIS_DT 전부 채워짐
--   상태 B · 작업시작함                       2행 → YT_DIS_DT 전부 채워짐
--                                        ─────────
--                                          370행   ("비어 있음" 묶음은 0행)
--
-- 결정적인 것은 **위 두 줄(317행)**이다: 트럭이 붙었는데 작업을 시작도 안 한 행에 이미 값이
-- 있으므로 도착 후 기입일 수 없다. 아래 두 줄은 분모를 닫으려고 같이 적는다.
--
-- ⚠ 이 파일 위쪽 UPD_DT 격차 통계의 분모는 373행으로 다르다 — 같은 조건에 시각 파싱
--   가드(LENGTH(TRIM(YT_DIS_DT))=14)를 더한 **별도 프로브**이고 몇 분 뒤에 돌았다.
--
-- ■ Oracle 부하 0
-- `JOB_ORDER_LIST` 는 이미 매분 스캔하는 표이고 이 행들도 이미 읽고 있다. SELECT 목록에
-- 한 줄 더한 것뿐이라 조회 계획도 왕복 수도 바뀌지 않는다(cre_ts·mig 0123 와 같은 방식).
--
-- ⚠ TO_CHAR 금지 — `YT_DIS_DT` 는 **VARCHAR2(14)** 다(ALL_TAB_COLUMNS 실측). 이미
--   'YYYYMMDDHH24MISS' 문자열이라 그대로 뽑는다. UPD_DT/CRE_DT 는 DATE 라 TO_CHAR 가
--   필요했는데, 이 컬럼에 같은 처리를 하면 안 된다(seqno 와 같은 경우).
--
-- 멱등.

ALTER TABLE live_workpool ADD COLUMN IF NOT EXISTS yt_dis_ts timestamptz;

COMMENT ON COLUMN live_workpool.yt_dis_ts IS
  'JOB_ORDER_LIST.YT_DIS_DT — TOS 가 이 트럭을 배차한 시각(권위값·mig 0148). '
  'upd_ts 와 달리 이후 행 갱신에 밀리지 않는다(실측 p90 격차 1,382초). '
  '탐지 지연을 재거나 배차 시점을 앵커로 쓸 때는 upd_ts 가 아니라 이 컬럼을 쓴다. '
  '⚠분모 주의: 미배차 행(ytno 빈 값)은 이 값이 NULL 이다 — 정상이다. '
  '표 전체로 채움률을 재면 41% 쯤 나와 결손처럼 보인다. '
  '분모는 반드시 ytno 가 채워진 행으로 잡을 것(그 분모에서는 100%).';
