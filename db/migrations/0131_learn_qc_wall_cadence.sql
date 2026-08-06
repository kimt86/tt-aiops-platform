-- 0131: 무브 하나의 **벽시계** 시간을 배운다 — learn_qc_move_time(활동 리듬: 1~300초 간격
-- 중앙값)은 300초 넘는 정지(트럭 대기 등·벽시계의 31~48%)를 잘라 1.6~1.8배 낙관적이었다
-- (실측 2026-08-06: DS 89→139초, LD 120→211초). 스케줄 산식은 이 값을 우선 쓴다.
-- learn_qc_move_time 은 scengen 이 읽으므로 제자리 수정 대신 새 매뷰(판별자 규율).
-- 같은 구역 연속 간격만(구역전환은 BAY_CHANGE_S/HATCH_*_S 로 따로 더하므로 제외),
-- 2초 이하 제외(트윈 둘째 상자), 1200초 초과 제외(중식·교대는 SHIFT_BREAK_S 가 따로 더함).
CREATE MATERIALIZED VIEW IF NOT EXISTS learn_qc_wall_cadence AS
WITH g AS (
  SELECT machno AS qc, jobtype, queuename,
         lag(queuename) OVER w AS prev_q,
         EXTRACT(epoch FROM comp_ts - lag(comp_ts) OVER w) AS gap
    FROM qc_move_log
   WHERE comp_ts > now() - interval '3 days' AND queuename ~ '^[0-9]+[HD]-[LD]$'
  WINDOW w AS (PARTITION BY machno ORDER BY comp_ts)
)
SELECT qc, jobtype, round(avg(gap))::int AS wall_s, count(*)::int AS n, now() AS as_of_ts
  FROM g
 WHERE prev_q = queuename AND gap > 2 AND gap <= 1200 AND jobtype IN ('DS','LD')
 GROUP BY qc, jobtype
HAVING count(*) >= 30;
CREATE UNIQUE INDEX IF NOT EXISTS learn_qc_wall_cadence_pk ON learn_qc_wall_cadence (qc, jobtype);
COMMENT ON MATERIALIZED VIEW learn_qc_wall_cadence IS
  '크레인·작업별 무브 하나의 벽시계 평균(같은 구역 연속·트윈 둘째 제외·20분 초과 제외). '
  '스케줄 산식의 move_s 1순위 원천. 갱신은 spawn_dispatch_pred_logger 의 20분 주기 (mig 0131).';
