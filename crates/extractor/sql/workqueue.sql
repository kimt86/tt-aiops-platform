-- Live per-QC work-queue plan (JOB_QUEUE_SCHEDULE). Each row is one (crane, vessel,
-- queue) chunk the QC works in JOB_QUE_SEQ order; TOTALQTY/COMPQTY give progress.
-- Bounded to currently-relevant queues: not deleted, touched within ~1 day, and either
-- not-yet-finished OR finished within the last ~6h (so the UI can show the last few
-- COMPLETED bays before NOW). Small result (tens-to-low-hundreds). No date token: live
-- state right now. JOB_QUE_ACTIVEYN is unreliable (NULL in practice) so NOT a filter.
SELECT
  s.JOB_QUE_CRANENO   AS qc,
  s.JOB_QUE_VESSEL    AS vessel,
  s.JOB_QUE_VOYAGE    AS voyage,
  s.JOB_QUE_QUEUENAME AS queuename,
  s.JOB_QUE_DISLOAD   AS disload,
  s.JOB_QUE_SEQ       AS seq,
  s.JOB_QUE_TOTALQTY  AS total_qty,
  s.JOB_QUE_COMPQTY   AS comp_qty,
  s.JOB_QUE_PLANQTY   AS plan_qty
FROM TOSADM.JOB_QUEUE_SCHEDULE s
WHERE NVL(s.DELT_FLG, 'N') <> 'Y'
  AND s.JOB_QUE_CRANENO IS NOT NULL
  AND s.UPD_DT >= TRUNC(SYSDATE) - 1
  AND ( NVL(s.JOB_QUE_TOTALQTY, 0) > NVL(s.JOB_QUE_COMPQTY, 0)
        OR s.UPD_DT >= SYSDATE - 0.25 )
ORDER BY s.JOB_QUE_CRANENO, s.JOB_QUE_SEQ
