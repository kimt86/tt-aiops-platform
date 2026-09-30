-- Planned voyage size (container count VAN) per vessel/voyage, for the LIVE vessel
-- panel progress bar. VSS_STT_MOVES is NULL for in-progress voyages, so VAN (planned
-- containers) is the available planned denominator. Latest VAN per voyage over the
-- recent window. Small table (~41K rows) -> TRIVIAL. Read-only.
SELECT
       VSS_STT_VESSEL AS vessel,
       VSS_STT_VOYAGE AS voyage,
       MAX(TO_NUMBER(VSS_STT_VAN DEFAULT NULL ON CONVERSION ERROR)) AS planned_moves
  FROM TOSADM.VSS_STATISTICS
 WHERE VSS_STT_UP_DT >= '{{START_TS}}'
   AND VSS_STT_VAN IS NOT NULL
 GROUP BY VSS_STT_VESSEL, VSS_STT_VOYAGE
 ORDER BY MAX(VSS_STT_UP_DT) DESC
 FETCH FIRST 120 ROWS ONLY
