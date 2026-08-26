-- 적하 트럭이 "TOS 배차 순간 우리 추천이 없다"는 22% 의 정체 (2026-08-26 · 트럭 축)
--   psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/ld_pool_absence.sql        (약 30초)
--   창 바꾸기: psql ... -v win_lo="'2026-08-25T07:45:00Z'" -f scripts/ld_pool_absence.sql
--
-- ■ 왜 (2026-08-26 사용자 지시 · HANDOFF)
-- "TOS 가 배차한 그 트럭에 대해 우리 추천이 99% 는 있어야 한다"가 목표다. 첫 측정에서 적하 77.3% ·
-- 양하 82.0% 가 나왔고, 빈 곳의 대부분이 "그 트럭이 우리 후보 명단에 아예 없었다"였다(적하 22.0%).
-- 이 스크립트는 그 22% 를 원인별로 가른다.
--
-- ■ ★결론 먼저 — 22% 의 대부분은 결함이 아니라 분모였다
-- 비교기(dispatch_compare_shadow)가 세는 "TOS 배차 사건"에는 **트럭이 이미 하던 일의 배차시각이 다시
-- 찍힌 것**이 섞여 있다. 그 트럭은 그 순간 일하는 중이므로 우리 명단에 없는 것이 정상이고, 추천할
-- 것도 없다. 정답지 드랍 로그로 "직전 사건 뒤 이 트럭이 실제로 자유로워졌나"를 물어 가르면:
--     · 새 요청(자유가 있었다)  = 적하 96.8% / 양하 96.2% 커버  ← 진짜 성적
--     · 진행 중(자유가 없었다)  = 적하 42.5% / 양하 56.3%      ← 애초에 물어본 적 없는 순간
-- 새 요청만의 96.8% 는 기존 합격 기준인 풀 재현율 ⑮ 96.4% 와 사실상 같은 값이다 — 두 잣대가 어긋난
-- 것처럼 보였던 것도 같은 분모 문제였다.
--
-- ■ 분모 (한 문장씩)
--   전체 사건   = 창 안에서 비교기가 채점한 TOS 배차 행을 **(트럭, 배차시각) 단위로 중복 제거**한 것
--                 (t1_ver=1 · reco_src NOT NULL · DS/LD). 같은 배차가 구역·갱신마다 여러 행이 되므로
--                 반드시 이 단위로 줄인다.
--   새 요청(A)  = 그 트럭의 **직전 사건 이후에 자유 사건이 있었던** 배차. 즉 트럭이 실제로 비었고
--                 그래서 TOS 가 일을 준 순간. (창의 첫 사건은 자유 뒤 120초 이내일 때만 A.)
--   진행 중(B)  = 직전 사건 이후 자유 사건이 **없는** 배차. 트럭이 하던 일의 배차시각이 다시 찍힌 것.
--   자유 정답지 = 적하는 `qc_move_log`(QC 가 상자를 내려 트럭이 빔) · 양하는 `tos_handover_label`.
--                 ⚠GPS `tt_cycle_log.dropped_at` 은 33% 를 놓쳐 채점에 쓰지 않는다.
--   커버됨      = T1 시점에 그 트럭에 대한 우리 추천이 150초 이내로 서 있었다(보드 STALE_S 와 같은 기준).
--
-- ■ 읽을 때 주의
--   · `pool_ver` 로 반드시 가른다(여기서는 8). 판이 다르면 명단 규칙이 다르다.
--   · `tt_move_log` 를 이 모집단의 자유 정답지로 쓰지 말 것 — 진행 중 사건에서 트립이 빠져 있어
--     "12분째 놀고 있었다" 같은 착시가 난다(2026-08-26 실측: 놓친 사건의 58% 만 정확한 트립 보유).
--     자유는 위의 원천 드랍 로그로 본다.
--   · B 를 "우리가 놓친 것"으로 세면 안 된다. 다만 **재지향**(배차됨·픽업 전 공차)은 B 안에 있고
--     그건 별개 설계 항목이다(pool_ver 8 · 슬롯 불산입).

\set ON_ERROR_STOP on
\pset null '-'
\if :{?win_lo} \else \set win_lo '2026-08-25T07:45:00Z' \endif
\if :{?pool_ver} \else \set pool_ver 8 \endif

BEGIN;
SET LOCAL statement_timeout = '300s';

CREATE TEMP TABLE b ON COMMIT DROP AS
  SELECT :'win_lo'::timestamptz AS lo,
         -- 위쪽 경계는 명단 기록의 끝에서 15분 물린다: 갓 들어온 사건은 아직 채점 재료가 덜 찼다.
         (SELECT max(ts) FROM stage2_pool_truck_shadow WHERE pool_ver = :pool_ver) - interval '15 minutes' AS hi;
SELECT lo AT TIME ZONE 'Asia/Kuala_Lumpur' AS "창 시작(MYT)",
       hi AT TIME ZONE 'Asia/Kuala_Lumpur' AS "창 끝(MYT)",
       round(extract(epoch FROM hi-lo)/3600.0, 1) AS "시간" FROM b;

-- 자유 정답지
CREATE TEMP TABLE frees ON COMMIT DROP AS
  SELECT trk_id AS ytno, comp_ts AS free_ts FROM qc_move_log, b
   WHERE jobtype = 'LD' AND trk_id IS NOT NULL
     AND comp_ts >= b.lo - interval '3 hours' AND comp_ts <= b.hi
  UNION ALL
  SELECT ytno, comp_ts FROM tos_handover_label, b
   WHERE jobtype = 'DS' AND ytno IS NOT NULL
     AND comp_ts >= b.lo - interval '3 hours' AND comp_ts <= b.hi;
CREATE INDEX ON frees (ytno, free_ts);
ANALYZE frees;

-- 사건: (트럭, 배차시각) 단위
CREATE TEMP TABLE ev ON COMMIT DROP AS
  SELECT DISTINCT ON (tos_ytno, t1_ts)
         tos_ytno AS ytno, t1_ts, jobtype, qc, queuename
    FROM dispatch_compare_shadow, b
   WHERE t1_ver = 1 AND reco_src IS NOT NULL AND jobtype IN ('DS','LD')
     AND t1_ts >= b.lo AND t1_ts <= b.hi
   ORDER BY tos_ytno, t1_ts, ts ASC;
CREATE INDEX ON ev (ytno, t1_ts);
ANALYZE ev;

CREATE TEMP TABLE evc ON COMMIT DROP AS
SELECT e.*,
  lag(t1_ts) OVER (PARTITION BY ytno ORDER BY t1_ts) AS prev_t1,
  (SELECT max(f.free_ts) FROM frees f WHERE f.ytno = e.ytno AND f.free_ts <= e.t1_ts) AS last_free,
  (SELECT min(f.free_ts) FROM frees f WHERE f.ytno = e.ytno AND f.free_ts >  e.t1_ts) AS next_free,
  NOT EXISTS (SELECT 1 FROM stage2_match_shadow s
               WHERE s.ytno = e.ytno AND s.ts <= e.t1_ts AND s.ts >= e.t1_ts - interval '150 seconds') AS no_reco,
  (SELECT p.reason FROM stage2_pool_truck_shadow p
     WHERE p.ytno = e.ytno AND p.pool_ver = :pool_ver
       AND p.ts <= e.t1_ts AND p.ts >= e.t1_ts - interval '150 seconds'
     ORDER BY p.ts DESC LIMIT 1) AS pool_reason,
  (SELECT h.ts FROM truck_pos_hist h WHERE h.ytno = e.ytno AND h.ts <= e.t1_ts
     ORDER BY h.ts DESC LIMIT 1) AS last_pos_ts
FROM ev e;
ALTER TABLE evc ADD COLUMN kind text;
UPDATE evc SET kind =
  CASE WHEN last_free IS NULL                                       THEN 'C_자유기록 없음'
       WHEN prev_t1 IS NULL AND t1_ts - last_free <= interval '120 seconds' THEN 'A_새 요청'
       WHEN prev_t1 IS NULL                                         THEN 'D_창 첫사건·판정보류'
       WHEN last_free > prev_t1                                     THEN 'A_새 요청'
       ELSE 'B_진행 중(직전 사건 뒤 자유 없음)' END;
CREATE INDEX ON evc (jobtype, kind);
ANALYZE evc;

\echo ''
\echo '=== ① 분모 감사 — 비교기가 센 "배차 사건"의 정체 ==='
SELECT jobtype AS "유형", kind AS "사건 종류", count(*) AS "건수",
       round(100.0*count(*)/sum(count(*)) OVER (PARTITION BY jobtype), 1) AS "비중%",
       count(*) FILTER (WHERE NOT no_reco) AS "추천 있었음",
       round(100.0*count(*) FILTER (WHERE NOT no_reco)/count(*), 1) AS "커버%"
  FROM evc GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '=== ② 트럭 축 커버리지 — 분모를 고치기 전/후 ==='
SELECT jobtype AS "유형",
       count(*) AS "전체 사건",
       round(100.0*count(*) FILTER (WHERE NOT no_reco)/count(*), 1) AS "전체 기준 커버%",
       count(*) FILTER (WHERE kind = 'A_새 요청') AS "새 요청",
       round(100.0*count(*) FILTER (WHERE kind='A_새 요청' AND NOT no_reco)
             / NULLIF(count(*) FILTER (WHERE kind='A_새 요청'),0), 1) AS "새 요청 기준 커버%"
  FROM evc GROUP BY 1 ORDER BY 1;

\echo ''
\echo '=== ③ 진짜 남은 몫 — 새 요청인데 추천이 없던 사건의 원인 ==='
SELECT jobtype AS "유형", count(*) AS "놓침",
  count(*) FILTER (WHERE pool_reason IS NOT NULL) AS "명단엔 있었음(슬롯 못 받음)",
  count(*) FILTER (WHERE pool_reason IS NULL AND last_pos_ts IS NULL) AS "위치 기록 아예 없음",
  count(*) FILTER (WHERE pool_reason IS NULL AND last_pos_ts IS NOT NULL
                     AND t1_ts - last_pos_ts > interval '10800 seconds') AS "위치 3h 상한 초과",
  count(*) FILTER (WHERE pool_reason IS NULL AND last_pos_ts IS NOT NULL
                     AND t1_ts - last_pos_ts <= interval '10800 seconds') AS "위치 멀쩡한데 명단 밖",
  round(percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM t1_ts - last_free))) AS "자유→배차 중앙(초)",
  count(*) FILTER (WHERE t1_ts - last_free <= interval '90 seconds') AS "자유 뒤 90초 안"
  FROM evc WHERE kind = 'A_새 요청' AND no_reco GROUP BY 1 ORDER BY 1;

\echo ''
\echo '=== ④ 반증 1 — B(진행 중)가 정말 "일하는 중"인가 ==='
\echo '    TOS 자신의 배차 목록(assigned_tt_hist) 이 T1 에 이 트럭을 뭐라고 부르고 있었나.'
\echo "    A=작업 활성 · Q=대기 배정 · 없음=목록에 아예 없음. 우리 명단·자유 로그와 무관한 독립 원천이다."
SELECT jobtype AS "유형", kind AS "사건 종류", count(*) AS "건수",
  count(*) FILTER (WHERE st = 'A') AS "TOS: 작업 활성(A)",
  round(100.0*count(*) FILTER (WHERE st = 'A')/count(*), 1) AS "A%",
  count(*) FILTER (WHERE st = 'Q') AS "TOS: 대기 배정(Q)",
  count(*) FILTER (WHERE st IS NULL) AS "TOS 목록에 없음"
  FROM (SELECT evc.*, (SELECT a.jobstatus FROM assigned_tt_hist a
                        WHERE a.ytno = evc.ytno AND a.as_of_ts <= evc.t1_ts
                          AND a.as_of_ts >= evc.t1_ts - interval '150 seconds'
                        ORDER BY a.as_of_ts DESC LIMIT 1) AS st FROM evc) z
 WHERE kind IN ('A_새 요청','B_진행 중(직전 사건 뒤 자유 없음)')
 GROUP BY 1,2 ORDER BY 1,2;

\echo '=== ④ 반증 2 — 위약: 자유 뒤 경과시간을 맞춰도 차이가 남는가 ==='
\echo '    "B 는 그냥 오래 놀던 트럭이라 놓쳤을 뿐"이면, 경과시간을 같게 맞추면 차이가 사라져야 한다.'
SELECT jobtype AS "유형",
  CASE WHEN t1_ts - last_free <= interval '60 seconds' THEN '0~60초'
       WHEN t1_ts - last_free <= interval '300 seconds' THEN '1~5분'
       WHEN t1_ts - last_free <= interval '900 seconds' THEN '5~15분'
       ELSE '15분+' END AS "자유 뒤 경과",
  count(*) FILTER (WHERE kind='A_새 요청') AS "A 건수",
  round(100.0*count(*) FILTER (WHERE kind='A_새 요청' AND NOT no_reco)
        / NULLIF(count(*) FILTER (WHERE kind='A_새 요청'),0), 1) AS "A 커버%",
  count(*) FILTER (WHERE kind LIKE 'B_%') AS "B 건수",
  round(100.0*count(*) FILTER (WHERE kind LIKE 'B_%' AND NOT no_reco)
        / NULLIF(count(*) FILTER (WHERE kind LIKE 'B_%'),0), 1) AS "B 커버%"
  FROM evc WHERE last_free IS NOT NULL GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '=== ⑤ 안정성 — 최근 6시간만으로 다시 ==='
SELECT jobtype AS "유형", kind AS "사건 종류", count(*) AS "건수",
       round(100.0*count(*)/sum(count(*)) OVER (PARTITION BY jobtype), 1) AS "비중%",
       round(100.0*count(*) FILTER (WHERE NOT no_reco)/count(*), 1) AS "커버%"
  FROM evc, b WHERE t1_ts >= b.hi - interval '6 hours' GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '=== ⑥ 시간대별 (3시간) — 새 요청 기준 커버리지가 흔들리는가 ==='
SELECT date_trunc('hour', t1_ts AT TIME ZONE 'Asia/Kuala_Lumpur')
         - make_interval(hours => (extract(hour FROM t1_ts AT TIME ZONE 'Asia/Kuala_Lumpur')::int % 3)) AS "구간(MYT)",
       jobtype AS "유형", count(*) AS "새 요청",
       round(100.0*count(*) FILTER (WHERE NOT no_reco)/count(*), 1) AS "커버%"
  FROM evc WHERE kind = 'A_새 요청' GROUP BY 1,2 ORDER BY 1,2;

ROLLBACK;
