-- ONE bounded round-trip for the whole 60s work-pool tick (CHUNK8 2026-08-10).
-- Round-trip fixed cost (~2s) dominates payload (PLAN-extractor CHUNK7 재판정:
-- full SELECT 2.61s vs COUNT(*) 2.01s), so the two per-tick scans — JOB_QUEUE_SCHEDULE
-- and JOB_ORDER_LIST — are folded into a single UNION ALL, discriminated by SRC:
--   SRC='WQ' → live_workqueue rows (per-QC queue plan + progress)
--   SRC='WP' → the JOB_ORDER_LIST scan (split in Rust into live_assigned_tt /
--              live_workpool / live_candidate exactly as before)
-- Each branch's WHERE is byte-identical to sql/workqueue.sql / sql/workpool.sql —
-- those files stay as the WORKPOOL_FETCH=split kill-switch path; keep all three in
-- sync when editing. Column-level rationale (seqno, cre_dt, TO_CHAR traps, no queue
-- join) lives in those two files — not repeated here.
-- NULL padding is CAST per branch so Oracle's UNION typing never guesses; NUMBER
-- columns stay NUMBER (JSON numbers → Option<i64>, RULES 3).
SELECT
  'WQ' AS src,
  s.JOB_QUE_QUEUENAME AS queuename,
  s.JOB_QUE_VESSEL    AS vessel,
  s.JOB_QUE_VOYAGE    AS voyage,
  s.JOB_QUE_CRANENO   AS qc,
  s.JOB_QUE_DISLOAD   AS disload,
  s.JOB_QUE_SEQ       AS seq,
  s.JOB_QUE_TOTALQTY  AS total_qty,
  s.JOB_QUE_COMPQTY   AS comp_qty,
  s.JOB_QUE_PLANQTY   AS plan_qty,
  CAST(NULL AS VARCHAR2(8))  AS jobtype,
  CAST(NULL AS VARCHAR2(8))  AS jobstatus,
  CAST(NULL AS VARCHAR2(8))  AS yt_status,
  CAST(NULL AS VARCHAR2(16)) AS ytno,
  CAST(NULL AS VARCHAR2(16)) AS armgc,
  CAST(NULL AS VARCHAR2(20)) AS etw_dt,
  CAST(NULL AS VARCHAR2(20)) AS actv_dt,
  CAST(NULL AS VARCHAR2(14)) AS upd_dt,
  CAST(NULL AS VARCHAR2(14)) AS cre_dt,
  CAST(NULL AS VARCHAR2(11)) AS contno,
  CAST(NULL AS VARCHAR2(20)) AS msnseq,
  CAST(NULL AS VARCHAR2(40)) AS seqno,
  CAST(NULL AS VARCHAR2(20)) AS yt_topos,
  CAST(NULL AS VARCHAR2(20)) AS from_pos,
  CAST(NULL AS VARCHAR2(20)) AS to_pos,
  CAST(NULL AS VARCHAR2(8))  AS twintandem,
  CAST(NULL AS VARCHAR2(40)) AS twinkey
FROM TOSADM.JOB_QUEUE_SCHEDULE s
WHERE NVL(s.DELT_FLG, 'N') <> 'Y'
  AND s.JOB_QUE_CRANENO IS NOT NULL
  AND s.UPD_DT >= TRUNC(SYSDATE) - 1
  AND ( NVL(s.JOB_QUE_TOTALQTY, 0) > NVL(s.JOB_QUE_COMPQTY, 0)
        OR s.UPD_DT >= SYSDATE - 0.25 )
UNION ALL
SELECT
  'WP' AS src,
  l.JOB_ODR_QUEUENAME  AS queuename,
  l.JOB_ODR_VESSEL     AS vessel,
  l.JOB_ODR_VOYAGE     AS voyage,
  CAST(NULL AS VARCHAR2(16)) AS qc,
  CAST(NULL AS VARCHAR2(8))  AS disload,
  CAST(NULL AS NUMBER) AS seq,
  CAST(NULL AS NUMBER) AS total_qty,
  CAST(NULL AS NUMBER) AS comp_qty,
  CAST(NULL AS NUMBER) AS plan_qty,
  l.JOB_ODR_JOBTYPE    AS jobtype,
  l.JOB_ODR_JOBSTATUS  AS jobstatus,
  l.JOB_ODR_YT_STATUS  AS yt_status,
  l.JOB_ODR_YTNO       AS ytno,
  l.JOB_ODR_ARMGC      AS armgc,
  l.JOB_ODR_ETW_DT     AS etw_dt,
  l.JOB_ODR_ACTV_DT    AS actv_dt,
  TO_CHAR(l.UPD_DT, 'YYYYMMDDHH24MISS') AS upd_dt,
  TO_CHAR(l.CRE_DT, 'YYYYMMDDHH24MISS') AS cre_dt,
  SUBSTR(l.JOB_ODR_CONTNO, 1, 11) AS contno,
  l.JOB_ODR_MSNSEQ     AS msnseq,
  l.JOB_ODR_SEQNO      AS seqno,
  l.JOB_ODR_YT_TOPOS   AS yt_topos,
  l.CRNT_PSN_IDX_NO1   AS from_pos,
  l.YT_TO_PSN_IDX_NO1  AS to_pos,
  l.JOB_ODR_TWINTANDEM AS twintandem,
  l.JOB_ODR_TWINKEY    AS twinkey
FROM TOSADM.JOB_ORDER_LIST l
WHERE l.JOB_ODR_COMPDATE IS NULL
  AND l.JOB_ODR_JOBSTATUS IN ('A', 'B', 'Q')
  AND l.CRE_DT >= TRUNC(SYSDATE) - 2
ORDER BY src, queuename, etw_dt
