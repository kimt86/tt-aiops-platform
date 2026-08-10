-- Local production for QC_MOVE_TIME (Oracle original: sql/qc_move_time.sql).
-- qc_move_log already only ever carries QC machines (machno ^[CMZ][0-9]+$ per
-- crates/extractor's ingestion filter, glossary in PLAN-extractor.md) so the
-- regex here is a no-op safety net, not a narrowing filter. Rolling 3-day window
-- (comp_ts >= now() - 3 days), same D(06-17)/N(else)/ALL grouping-sets shape as
-- the original's GROUPING SETS ((qc,jt,shift),(qc,jt)), same 1..300s gap cap.
--
-- ★트윈 보정(2026-08-10): 트윈(들어올림 1회·상자 2개)은 완료가 0~2초 차이 나는 연속
-- 2행으로 남아, 행 단위 간격 학습에 가짜 1~2초 표본을 만들었다(전체 간격의 ~16%가
-- 0~2초 — 상자 트윈율 18.2%와 정합. 그 편향으로 med가 DS 90s/LD 111s로 눌려 있었고
-- 보정 후 99s/115s — 실측). 같은 크레인에서 완료가 2초 이내로 붙은 행들을 "들어올림"
-- 하나로 접고, 들어올림 사이 간격으로 학습한다. 0초 동시각 쌍은 원래 캡(1..300)이
-- 걸렀지만 1~2초 쌍이 통과하던 것이 문제였다.
WITH m0 AS (
  SELECT machno AS qc,
         jobtype AS jt,
         comp_ts AS e,
         CASE WHEN EXTRACT(EPOCH FROM (comp_ts - LAG(comp_ts) OVER (PARTITION BY machno ORDER BY comp_ts))) <= 2
              THEN 0 ELSE 1 END AS new_lift
    FROM qc_move_log
   WHERE machno ~ '^[CMZ][0-9]+$'
     AND jobtype IN ('LD', 'DS')
     AND comp_ts >= now() - interval '3 days'
),
m AS (  -- 들어올림(리프트) 단위로 병합: 트윈 = 1행
  SELECT qc, MIN(jt) AS jt, MAX(e) AS e,
         to_char(MAX(e) AT TIME ZONE 'Asia/Kuala_Lumpur', 'HH24') AS hh
    FROM (SELECT m0.*, SUM(new_lift) OVER (PARTITION BY qc ORDER BY e) AS lift_id FROM m0) x
   GROUP BY qc, lift_id
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
