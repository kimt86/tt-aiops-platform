-- K_CYCLE(shift) / K_TT_CYCLE — ★2026-08-10 재정의(사용자 승인).
--
-- 트럭 사이클 = **배정(dispatch_ts) → 트럭 자유(free_ts)**. free_ts 는 마지막 크레인
-- (적하=QC·양하=RTG)이 트럭에서 상자를 들어올린 순간의 TOS 권위 앵커다(tt_move_log,
-- mig0092·커버 98%). 종전 c10(트럭별 연속 QC무브 완료 간격·120~1200s 캡 = "회전 리듬")의
-- 문제 셋: ①캡 밖(20분 초과)을 통째로 버려 현장이 느릴수록 값이 좋아 보이는 역설
-- ②작업 사이 유휴가 섞여 배차 품질과 분리 불가 ③양하의 진짜 끝(야드 RTG 하차)이 안 보임.
-- 새 정의는 캡 없이 전부 세고(표시값=중앙값이라 강건), 트윈은 twin_leg_seq=1 로
-- 한 운반 1회 계수(l_cycle.sql 파리티와 같은 규약).
--
-- 옛 산식은 l_tt_cycle_c10.sql 로 보존 — src_cycle 이 파리티 키 K_CYCLE_C10 으로 내부
-- 수집을 계속한다(표시만 내림). Oracle 킬스위치(KPI_T1_SRC=oracle)도 c10 그대로라 즉시
-- 복귀 가능. ⚠ 2026-08-10 이전 raw_k_tt_cycle·kpi_shift 이력은 c10 값 그대로 보존 —
-- 기간 조회가 이 날짜를 걸치면 두 정의가 섞인다(KC kpi 문서 고지).
--
-- 출력 모양은 c10 판과 동일 — 소비자(shift.rs→kpi_shift / k_tt_cycle.rs→raw_k_tt_cycle /
-- api agg·routes / 프론트 CycleSplit)는 코드 무변경으로 두 표시 경로가 동시에 움직인다.
-- $1,$2 = 창 시작/끝 (timestamptz UTC), free_ts(완료 사건) 기준.
WITH trips AS (
  SELECT ytno, jobtype AS jt, cycle_s::float8 AS cyc
    FROM tt_move_log
   WHERE free_ts >= $1 AND free_ts < $2
     AND cycle_s IS NOT NULL
     AND twin_leg_seq = 1
)
SELECT count(DISTINCT ytno)::float8                                                        AS trucks,
       count(*)::float8                                                                    AS samples,
       round(avg(cyc)::numeric, 1)::float8                                                 AS avg_sec,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY cyc))::numeric, 1)::float8        AS med_sec,
       round((percentile_cont(0.25) WITHIN GROUP (ORDER BY cyc))::numeric, 1)::float8       AS p25_sec,
       round((percentile_cont(0.75) WITHIN GROUP (ORDER BY cyc))::numeric, 1)::float8       AS p75_sec,
       count(*) FILTER (WHERE jt = 'DS')::float8                                            AS ds_samples,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY cyc)
              FILTER (WHERE jt = 'DS'))::numeric, 1)::float8                                AS ds_med_sec,
       count(*) FILTER (WHERE jt = 'LD')::float8                                            AS ld_samples,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY cyc)
              FILTER (WHERE jt = 'LD'))::numeric, 1)::float8                                AS ld_med_sec
  FROM trips
