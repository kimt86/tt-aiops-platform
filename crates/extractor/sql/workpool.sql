-- Live work pool: individual container moves still to do (JOB_ORDER_LIST is the live
-- twin of JOB_ORDER_HISTORY, but also retains completed rows, so we MUST filter to
-- live). JOBSTATUS: C=Complete A=Active Q=Queued P=Planned B=Blocked. ONE bounded scan
-- pulls THREE things at once (Oracle-load-conscious, PLAN-extractor CHUNK7 7-1(a) —
-- this used to be two Oracle round-trips: this scan + a separate SQL_ASSIGNED call):
--   A (any jobtype)              -> the "assigned" TT roster (any active job)
--   B, Q (any jobtype)           -> also the "assigned" TT roster when YTNO present
--   DS/LD + YTNO present (A, or Q pre-pickup) -> live_workpool as DISPATCHED rows
--   Q + DS/LD + YTNO empty       -> live_candidate + live_workpool (UNASSIGNED demand)
-- The extractor splits all of this in Rust: rows with a non-empty YTNO -> live_assigned_tt
-- (any jobtype, status A/B/Q — same population SQL_ASSIGNED used to select); DS/LD rows
-- WITH a truck -> live_workpool dispatched rows regardless of status ('Q'+YTNO = dispatched,
-- pre-pickup — ★2026-08-24 전까지는 이 행을 버려 배차 탐지가 픽업까지 늦었다. jobstatus 는
-- 원문 보존); DS/LD + Q + empty YTNO -> live_candidate (aggregated by QC for discharge /
-- by source block for load) + individual live_workpool rows with ytno=NULL. CRE_DT within
-- ~2 days bounds the scan and drops stale orphans.
--
-- NO queue join here: queuenames (e.g. '02D-L') are reused across vessels/voyages over
-- time, so joining JOB_QUEUE_SCHEDULE on (queuename, vessel) fans out against historic
-- queue rows. The QC is attached downstream in Postgres against the clean, current
-- live_workqueue snapshot (unique per vessel+queuename), avoiding the fan-out entirely.
SELECT
  l.JOB_ODR_QUEUENAME  AS queuename,
  l.JOB_ODR_VESSEL     AS vessel,
  l.JOB_ODR_VOYAGE     AS voyage,
  l.JOB_ODR_JOBTYPE    AS jobtype,
  l.JOB_ODR_JOBSTATUS  AS jobstatus,
  l.JOB_ODR_YT_STATUS  AS yt_status,
  l.JOB_ODR_YTNO       AS ytno,
  l.JOB_ODR_ARMGC      AS armgc,
  l.JOB_ODR_ETW_DT     AS etw_dt,
  l.JOB_ODR_ACTV_DT    AS actv_dt,
  TO_CHAR(l.UPD_DT, 'YYYYMMDDHH24MISS') AS upd_dt,  -- row last-update ≈ dispatch time (D_tos); DATE→string for parse_etw
  TO_CHAR(l.CRE_DT, 'YYYYMMDDHH24MISS') AS cre_dt,  -- 작업지시가 만들어진 시각. 이미 아래 WHERE 절이
                                                    -- 쓰는 컬럼이라 조회 부하는 늘지 않는다. '지시 생성 →
                                                    -- 실제 작업'을 추정이 아니라 실측하려고 값으로도 뽑는다.
                                                    -- ⚠ DATE 를 그대로 두면 툴박스가 JSON 숫자로 바꿔
                                                    --   Option<String> 디코드가 배치째 실패한다 → TO_CHAR 필수.
  l.YT_DIS_DT          AS yt_dis_dt,  -- ★TOS 가 이 트럭을 배차한 시각(권위값·mig 0148).
                                      -- 위 upd_dt 는 "≈ 배차 시각"인 대리값일 뿐 — 행이 나중에
                                      -- 또 갱신되면 뒤로 밀린다(실측 중앙 0초지만 p90 1,382초).
                                      -- 배차 시점을 앵커로 쓸 때는 이 컬럼을 쓴다.
                                      -- ⚠ VARCHAR2(14) 라 TO_CHAR 금지(ALL_TAB_COLUMNS 실측).
                                      --   이미 'YYYYMMDDHH24MISS' 문자열이다. seqno 와 같은 경우.
  SUBSTR(l.JOB_ODR_CONTNO, 1, 11) AS contno,
  l.JOB_ODR_MSNSEQ     AS msnseq,   -- ⚠ 항상 비어 있다(660/660). 순번처럼 보이지만 쓸 수 없다.
  l.JOB_ODR_SEQNO      AS seqno,    -- ★크레인 작업 순번. 배치 발행시각 꼴 문자열이라 사전순=시간순.
                                    -- 구역 안 순서의 권위 값이다(끝난 작업 298,074쌍에 100% 일치).
                                    -- 동률 = 트윈(상자 2개·무브 1회). 개정되므로 "다음 하나"가 아니라
                                    -- "앞에 몇 개"를 세는 데 쓴다. VARCHAR2 라 TO_CHAR 불필요. mig 0127.
  l.JOB_ODR_YT_TOPOS   AS yt_topos,
  l.CRNT_PSN_IDX_NO1   AS from_pos,
  l.YT_TO_PSN_IDX_NO1  AS to_pos,
  l.JOB_ODR_TWINTANDEM AS twintandem,
  l.JOB_ODR_TWINKEY    AS twinkey   -- twin pair grouping: same twinkey = 2 different containers, 1 truck
FROM TOSADM.JOB_ORDER_LIST l
WHERE l.JOB_ODR_COMPDATE IS NULL
  AND l.JOB_ODR_JOBSTATUS IN ('A', 'B', 'Q')
  AND l.CRE_DT >= TRUNC(SYSDATE) - 2
ORDER BY l.JOB_ODR_QUEUENAME, l.JOB_ODR_ETW_DT
