-- Local production for QC_MOVE_TIME (Oracle original: sql/qc_move_time.sql).
-- qc_move_log already only ever carries QC machines (machno ^[CMZ][0-9]+$ per
-- crates/extractor's ingestion filter, glossary in PLAN-extractor.md) so the
-- regex here is a no-op safety net, not a narrowing filter. Rolling 3-day window
-- (comp_ts >= now() - 3 days), same D(06-17)/N(else)/ALL grouping-sets shape as
-- the original's GROUPING SETS ((qc,jt,shift),(qc,jt)), same 1..300s gap cap.
WITH m AS (
  SELECT machno AS qc,
         jobtype AS jt,
         comp_ts AS e,
         to_char(comp_ts AT TIME ZONE 'Asia/Kuala_Lumpur', 'HH24') AS hh
    FROM qc_move_log
   WHERE machno ~ '^[CMZ][0-9]+$'
     AND jobtype IN ('LD', 'DS')
     AND comp_ts >= now() - interval '3 days'
),
g AS (
  SELECT qc, jt, hh,
         EXTRACT(EPOCH FROM (e - LAG(e) OVER (PARTITION BY qc ORDER BY e))) AS gap
    FROM m
),
gs AS (
  SELECT qc, jt,
         CASE WHEN hh BETWEEN '06' AND '17' THEN 'D' ELSE 'N' END AS shift,
         gap
    FROM g
   WHERE gap BETWEEN 1 AND 300
)
SELECT qc,
       jt AS jobtype,
       CASE WHEN GROUPING(shift) = 1 THEN 'ALL' ELSE shift END AS shift,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY gap))::numeric)::float8 AS med_sec,
       count(*)::float8                                                          AS n
  FROM gs
 GROUP BY GROUPING SETS ((qc, jt, shift), (qc, jt))
HAVING count(*) >= 30
