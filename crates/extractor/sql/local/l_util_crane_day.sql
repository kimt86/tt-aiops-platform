-- Local nightly-day production for K_UTIL_CRANE (Oracle original:
-- sql/e1c_k_util_crane_merged_intervals.sql). Straight per-machine merge (no
-- vessel/voyage partition -- the original doesn't partition by vessel either).
-- $1 = business date.
--
-- ★2026-08-10 QC 절반 재정의(사용자 지시): qc_move_log.dispatch_ts(구명 st_ts·mig0147)는 크레인 시작이 아니라
-- 트럭 배정 시각이라, [st_ts, comp_ts] 병합은 트럭 이동시간까지 크레인 가동으로 세어
-- 가동률을 약 2배 부풀렸다(실측 63~69% → 추정 시작 기준 35~38%). QC 는 트윈을
-- 들어올림으로 접고 추정 물리 시작(greatest(직전 들어올림 완료, 완료−학습 무브시간))으로
-- 바쁨 구간을 만든다 — 산식 설명은 l_qc_q.sql 머리 참조.
-- ⚠ YC(rtg_move_log) 절반은 그대로: 그쪽 st_ts 는 진짜 물리 시작이다(겹침 1.3% 실측).
--    machno LIKE 'RTG%' 는 원본 Oracle 필터 그대로(ES% 제외).
-- ⚠ QC 절반은 jobtype 무필터(원본과 동일 모집단) — LD/DS 외 유형은 학습 무브시간이
--    없어 유형 중앙값→100초로 폴백한다.
WITH m0 AS (
  SELECT machno, jobtype AS jt, comp_ts,
         CASE WHEN EXTRACT(EPOCH FROM (comp_ts - LAG(comp_ts) OVER (PARTITION BY machno ORDER BY comp_ts))) <= 2
              THEN 0 ELSE 1 END AS new_lift
    FROM qc_move_log
   WHERE machno ~ '^(C|CR|DC|M|Z)[0-9]+$'
     AND business_date = $1
),
lifts AS (
  SELECT machno, MIN(jt) AS jt, MAX(comp_ts) AS e
    FROM (SELECT m0.*, SUM(new_lift) OVER (PARTITION BY machno ORDER BY comp_ts) AS lift_id FROM m0) x
   GROUP BY machno, lift_id
),
mt AS (
  SELECT qc, jobtype, med_sec FROM learn_qc_move_time WHERE shift = 'ALL'
),
jt_med AS (
  SELECT jobtype, percentile_cont(0.5) WITHIN GROUP (ORDER BY med_sec) AS med FROM mt GROUP BY jobtype
),
all_med AS (
  SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY med_sec) AS med FROM mt
),
moves AS (
  SELECT l.machno, 'QC' AS machine_type,
         GREATEST(
           l.e - make_interval(secs => COALESCE(t.med_sec, j.med, a.med, 100)),
           COALESCE(MAX(l.e) OVER (PARTITION BY l.machno ORDER BY l.e
                                   ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING),
                    l.e - make_interval(secs => COALESCE(t.med_sec, j.med, a.med, 100)))
         ) AS s,
         l.e
    FROM lifts l
    LEFT JOIN mt t     ON t.qc = l.machno AND t.jobtype = l.jt
    LEFT JOIN jt_med j ON j.jobtype = l.jt
    CROSS JOIN all_med a
  UNION ALL
  SELECT machno, 'YC' AS machine_type, st_ts AS s, comp_ts AS e
    FROM rtg_move_log
   WHERE machno LIKE 'RTG%'
     AND st_ts IS NOT NULL
     AND business_date = $1
),
flagged AS (
  SELECT machno, machine_type, s, e,
         CASE WHEN s > MAX(e) OVER (PARTITION BY machno
                                     ORDER BY s
                                     ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING)
              THEN 1 ELSE 0 END AS new_grp
    FROM moves
),
grouped AS (
  SELECT machno, machine_type, s, e,
         SUM(new_grp) OVER (PARTITION BY machno ORDER BY s) AS gid
    FROM flagged
),
merged AS (
  SELECT machno, machine_type, gid,
         MIN(s) AS grp_start, MAX(e) AS grp_end, COUNT(*) AS moves_in_grp
    FROM grouped
   GROUP BY machno, machine_type, gid
)
SELECT machno,
       machine_type,
       count(*)::float8                                                          AS interval_groups,
       sum(moves_in_grp)::float8                                                 AS total_moves,
       round(sum(EXTRACT(EPOCH FROM (grp_end - grp_start)))::numeric)::float8    AS active_sec_merged,
       round((sum(EXTRACT(EPOCH FROM (grp_end - grp_start))) / 86400.0)::numeric, 4)::float8 AS k_util_merged_24h,
       round(avg(EXTRACT(EPOCH FROM (grp_end - grp_start)))::numeric, 1)::float8 AS avg_grp_sec,
       max(EXTRACT(EPOCH FROM (grp_end - grp_start)))::float8                    AS longest_grp_sec
  FROM merged
 GROUP BY machno, machine_type
 ORDER BY total_moves DESC
-- (LIMIT 60 제거 2026-08-10 — 원본(e1c)과 동시 제거)
