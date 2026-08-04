-- Live work pool: individual container moves still to do (JOB_ORDER_LIST is the live
-- twin of JOB_ORDER_HISTORY, but also retains completed rows, so we MUST filter to
-- live). JOBSTATUS: C=Complete A=Active Q=Queued P=Planned B=Blocked. ONE bounded scan
-- pulls BOTH (Oracle-load-conscious): A = dispatched in-flight moves (ETW + assigned
-- TT, the QC task cards); Q = the UNASSIGNED candidate demand (no truck yet). The
-- extractor splits them in Rust — A → live_workpool, Q (aggregated by QC for discharge
-- / by source block for load) → live_candidate. CRE_DT within ~2 days bounds the scan
-- and drops stale orphans.
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
  SUBSTR(l.JOB_ODR_CONTNO, 1, 11) AS contno,
  l.JOB_ODR_MSNSEQ     AS msnseq,
  l.JOB_ODR_YT_TOPOS   AS yt_topos,
  l.CRNT_PSN_IDX_NO1   AS from_pos,
  l.YT_TO_PSN_IDX_NO1  AS to_pos,
  l.JOB_ODR_TWINTANDEM AS twintandem,
  l.JOB_ODR_TWINKEY    AS twinkey   -- twin pair grouping: same twinkey = 2 different containers, 1 truck
FROM TOSADM.JOB_ORDER_LIST l
WHERE l.JOB_ODR_COMPDATE IS NULL
  AND l.JOB_ODR_JOBTYPE IN ('DS', 'LD')
  AND l.JOB_ODR_JOBSTATUS IN ('A', 'Q')
  AND l.CRE_DT >= TRUNC(SYSDATE) - 2
ORDER BY l.JOB_ODR_QUEUENAME, l.JOB_ODR_ETW_DT
