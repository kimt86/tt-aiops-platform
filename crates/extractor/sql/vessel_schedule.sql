-- Live vessel schedule from TOS VSB_VOYAGE (voyage master). The deadline source: estimated
-- work-complete / departure / berth / cut-off, plus actuals and planned discharge/load counts.
-- Bounded to currently-relevant voyages: not cancelled, departing within a recent window (so
-- berthed + soon-to-berth voyages are covered, departed-long-ago drop off). Date columns are
-- VARCHAR YYYYMMDD; the predicate is a plain string compare. Small result (tens of rows).
SELECT
  VSB_VOY_VESSEL  AS vessel,
  VSB_VOY_VOYAGE  AS voyage,
  VSB_VOY_STATUS  AS status,
  VSB_VOY_BERTHNO AS berthno,
  VSB_VOY_ESTBER_DATE || VSB_VOY_ESTBER_TIME AS estber,
  VSB_VOY_ESTWKC_DATE || VSB_VOY_ESTWKC_TIME AS estwkc,
  VSB_VOY_ESTDEP_DATE || VSB_VOY_ESTDEP_TIME AS estdep,
  VSB_VOY_CUTOFF_DATE || VSB_VOY_CUTOFF_TIME AS cutoff,
  VSB_VOY_ACTBER_DATE || VSB_VOY_ACTBER_TIME AS actber,
  VSB_VOY_ACTDEP_DATE || VSB_VOY_ACTDEP_TIME AS actdep,
  TO_NUMBER(VSB_VOY_DISVAN DEFAULT NULL ON CONVERSION ERROR)  AS disvan,
  TO_NUMBER(VSB_VOY_LOADVAN DEFAULT NULL ON CONVERSION ERROR) AS loadvan
FROM TOSADM.VSB_VOYAGE
WHERE NVL(VSB_VOY_CANCEL, 'N') = 'N'
  AND VSB_VOY_ESTDEP_DATE >= TO_CHAR(SYSDATE - 2, 'YYYYMMDD')
  AND VSB_VOY_ESTDEP_DATE <= TO_CHAR(SYSDATE + 10, 'YYYYMMDD')
