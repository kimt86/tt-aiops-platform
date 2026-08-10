-- Local parity for K_MPH (Oracle original: sql/c07_k_mph_realtime.sql, MCH_OPERATION).
-- Same filter (machno ^[CMZ][0-9]+$, jobtype LD/DS), same window bound as the Oracle shift
-- ⚠ 2026-08-10까지는 ^C[0-9]+$ 였다 — 장비 마스터(CDY_MACHINE_TYPE) 확인 결과 M·Z 계열도
--   같은 QC(STS)라 포함(안벽 무브의 ~26%). 이 필터의 권위는 마스터이지 접두사가 아니다.
-- predicate (comp_ts BETWEEN start/end). Per-crane moves + active_hours(distinct hour
-- bucket of comp_ts) so the caller can fold with the SAME active-hours-weighted formula
-- as src_mph_vessels. $1,$2 = window start/end (timestamptz UTC).
-- (이력) 한때 원본의 `FETCH FIRST 30 ROWS ONLY` 를 그대로 재현하는 LIMIT 30 이 있었다 —
-- 패리티를 위해서였는데, 2026-08-10 그 상한 자체가 사전조사 탐색 질의의 잔재로 판명되어
-- **원본(c07)과 여기서 동시에 제거**했다(패리티는 계속 성립). 활성 QC가 상한(30)을 넘는
-- 교대에서 덜 바쁜 크레인이 headline 표본에서 잘리던 문제가 함께 사라졌다.
SELECT machno                                                        AS qc_machno,
       count(*)::float8                                              AS moves,
       count(*) FILTER (WHERE jobtype = 'LD')::float8                AS load_moves,
       count(*) FILTER (WHERE jobtype = 'DS')::float8                AS discharge_moves,
       count(DISTINCT date_trunc('hour', comp_ts))::float8           AS active_hours,
       round(
         (count(*)::numeric
            / nullif(count(DISTINCT date_trunc('hour', comp_ts)), 0)), 2
       )::float8                                                     AS k_mph_per_active_hour,
       count(DISTINCT trk_id)::float8                                AS distinct_trucks,
       count(DISTINCT contno)::float8                                AS distinct_containers,
       min(comp_ts)                                                  AS first_move,
       max(comp_ts)                                                  AS last_move
  FROM qc_move_log
 WHERE machno ~ '^(C|CR|DC|M|Z)[0-9]+$'
   AND jobtype IN ('LD', 'DS')
   AND comp_ts >= $1 AND comp_ts < $2
 GROUP BY machno
 ORDER BY moves DESC
-- (LIMIT 30 제거 2026-08-10 — 상한 제거·[CMZ] 확장을 Oracle 원본(c07)과 동시에 적용, 패리티 유지)
