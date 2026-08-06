-- Local production for K_EMPTY / K_EMPTY_R (Oracle original: sql/e4_k_empty_decomposition.sql).
-- PLAN-extractor.md CHUNK7 7-2(c). Same caps/shift buckets as the original; the ONE structural
-- difference is the source grain: JOB_ORDER_HISTORY carries multiple transition rows per job
-- (hence the original's GROUP BY job-key + MAX(...) to collapse them), while tos_handover_label
-- is already one row per completed job (contno,point,seqno PK) -- no collapse step needed.
-- un_trv_rng only lands from CHUNK7 7-2(b)'s deploy time onward (mig0138); rows completed
-- before that are NULL and correctly excluded by the BETWEEN filter below (NULL fails BETWEEN).
-- $1,$2 = window start/end (timestamptz UTC), applied on comp_ts (job completion instant --
-- matches the Oracle original's JOB_HIST_DATE/TIME predicate window).
WITH jobs AS (
  SELECT jobtype,
         comp_ts,
         trv_rng    AS lndn,
         un_trv_rng AS un_lndn,
         CASE
           WHEN to_char(comp_ts AT TIME ZONE 'Asia/Kuala_Lumpur', 'HH24')::int BETWEEN 0 AND 7  THEN 'Night'
           WHEN to_char(comp_ts AT TIME ZONE 'Asia/Kuala_Lumpur', 'HH24')::int BETWEEN 8 AND 15  THEN 'Day'
           ELSE 'Evening'
         END AS shift
    FROM tos_handover_label
   WHERE comp_ts >= $1 AND comp_ts < $2
     AND jobtype IN ('DS', 'LD')
     AND trv_rng    BETWEEN 0 AND 5000
     AND un_trv_rng BETWEEN 0 AND 5000
)
SELECT jobtype,
       shift,
       count(*)::float8                                              AS jobs,
       round((sum(un_lndn) / nullif(sum(lndn + un_lndn), 0))::numeric, 4)::float8 AS k_empty_ratio,
       round(avg(un_lndn)::numeric, 1)::float8                        AS avg_empty_m,
       round(avg(lndn)::numeric, 1)::float8                           AS avg_laden_m,
       round(sum(un_lndn)::numeric, 0)::float8                        AS total_empty_m,
       round(sum(lndn)::numeric, 0)::float8                           AS total_laden_m,
       NULL::float8                                                   AS distinct_blocks
       -- distinct_blocks has no local analogue (tos_handover_label carries `topos`, the yard
       -- work-point, not CRNT_PSN_IDX_NO1 block id) -- NULL (honest "not available"), not a
       -- fabricated 0. Never used downstream (neither src_empty's Rust fold nor raw_k_empty's
       -- consumers key off it).
  FROM jobs
 GROUP BY jobtype, shift
HAVING count(*) >= 50  -- mirrors the Oracle original's HAVING COUNT(*) >= 50
 ORDER BY total_empty_m DESC NULLS LAST
 FETCH FIRST 50 ROWS ONLY  -- mirrors the original's FETCH FIRST 50 ROWS ONLY
