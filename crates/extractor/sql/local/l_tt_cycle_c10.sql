-- ★옛 c10 "회전 리듬" 산식 보존본 (2026-08-10 K_CYCLE 재정의로 표시에서 내림).
-- src_cycle 이 파리티 키 K_CYCLE_C10 으로 내부 수집만 계속한다(비교용·사용자 약속).
-- 표시 산식은 l_tt_cycle.sql(배정→트럭 자유)로 교체됨 — 근거는 그 파일 머리 참조.
-- 아래는 종전 그대로: (Oracle original: sql/c10_k_tt_cycle.sql)
-- Same filter (machno ^[CMZ][0-9]+$ — 2026-08-10 M·Z 확장, jobtype LD/DS, trk_id not null), same 120..1200s cap,
-- gap = consecutive comp_ts per trk_id. $1,$2 = window start/end (timestamptz UTC).
WITH base AS (
  SELECT trk_id, jobtype AS jt, comp_ts
    FROM qc_move_log
   WHERE machno ~ '^[CMZ][0-9]+$'
     AND jobtype IN ('LD', 'DS')
     AND trk_id IS NOT NULL
     AND comp_ts >= $1 AND comp_ts < $2
),
seq AS (
  SELECT trk_id, jt,
         EXTRACT(EPOCH FROM (comp_ts - LAG(comp_ts) OVER (PARTITION BY trk_id ORDER BY comp_ts))) AS gap_sec
    FROM base
),
capped AS (
  SELECT trk_id, jt, gap_sec FROM seq WHERE gap_sec BETWEEN 120 AND 1200
)
SELECT count(DISTINCT trk_id)::float8                                                      AS trucks,
       count(*)::float8                                                                    AS samples,
       round(avg(gap_sec)::numeric, 1)::float8                                             AS avg_sec,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY gap_sec))::numeric, 1)::float8    AS med_sec,
       round((percentile_cont(0.25) WITHIN GROUP (ORDER BY gap_sec))::numeric, 1)::float8   AS p25_sec,
       round((percentile_cont(0.75) WITHIN GROUP (ORDER BY gap_sec))::numeric, 1)::float8   AS p75_sec,
       count(*) FILTER (WHERE jt = 'DS')::float8                                            AS ds_samples,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY gap_sec)
              FILTER (WHERE jt = 'DS'))::numeric, 1)::float8                                AS ds_med_sec,
       count(*) FILTER (WHERE jt = 'LD')::float8                                            AS ld_samples,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY gap_sec)
              FILTER (WHERE jt = 'LD'))::numeric, 1)::float8                                AS ld_med_sec
  FROM capped
