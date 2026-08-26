-- "TOS 배차 순간 그 트럭에 우리 추천이 있었나" — 분모 감사와 놓침 분해 (2026-08-26 · 트럭 축)
--   psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/ld_pool_absence.sql      (약 3초)
--   창 바꾸기: psql ... -v win_lo="'2026-08-25T07:45:00Z'" -f scripts/ld_pool_absence.sql
--
-- ■ 왜 (2026-08-26 사용자 지시 · HANDOFF)
-- "TOS 가 배차한 그 트럭에 대해 우리 추천이 99% 는 있어야 한다"가 목표다. 첫 측정이 적하 77.3% ·
-- 양하 82.0% 였고, 빈 곳의 대부분이 "그 트럭이 우리 후보 명단에 아예 없었다"였다.
--
-- ■ 이 스크립트가 하는 일
-- 비교기가 센 "TOS 배차 사건"을 **트럭이 새로 비었는가**로 가른다. 자유 정답지는 원천 드랍 로그다.
--   A 새 요청  = 직전 사건 이후 자유 사건이 있었다 → 트럭이 실제로 비어서 일을 받은 순간.
--   B 자유 없음 = 직전 사건 이후 자유 사건이 없다 → 직전 배차가 아직 안 끝났는데 또 배차 기록이 났다.
-- 트럭 축 합격 기준은 A 로 재는 것이 맞다. B 는 "트럭이 물어본 순간"이 아니기 때문이다.
--
-- ⚠ B 를 "일하는 중"이라 부르지 말 것 — 실측(⑦절)은 B 트럭이 T1 에 상자를 싣고 있는 비율이
--   3~5% 뿐이다. 대부분은 **이미 배차받아 빈 차로 픽업하러 가는 중**이다. 그래서 B 는
--   "우리가 신경 안 써도 되는 것"이 아니다 — 그중 다른 크레인으로 넘어간 몫은 재지향(pool_ver 8)
--   설계와 겹친다. B 를 분모에서 빼는 근거는 "트럭이 새로 비지 않았다" 하나이지, "할 일이 없었다"가 아니다.
--
-- ■ 분모 (한 문장씩)
--   전체 사건 = 창 안에서 비교기가 채점한 TOS 배차 행을 **(트럭, 배차시각) 단위로 중복 제거**한 것
--               (`t1_ver=1` · `reco_src NOT NULL` · DS/LD).
--   A 새 요청 = 위 사건 중 **직전 사건 이후 자유 사건이 있었던** 것. 창의 첫 사건은 자유 뒤 120초 이내일 때만.
--   B 자유 없음 = 직전 사건 이후 자유 사건이 **없는** 것.
--   자유 정답지 = 적하 `qc_move_log`(QC 가 내려 트럭이 빔) · 양하 `tos_handover_label`.
--   픽업 앵커   = 적하 `rtg_move_log`(블록 픽업) · 양하 `qc_move_log`(QC 픽업). 풀의 in-flight 예측과 같은 계열.
--   커버됨     = T1 에 그 트럭에 대한 우리 추천이 150초 이내로 서 있었다(보드 STALE_S 와 같은 기준).
--
-- ■ 읽을 때 주의
--   · `pool_ver` 로 반드시 가른다(여기서는 8). 판이 다르면 명단 규칙이 다르다.
--   · `tt_move_log` 를 이 모집단의 자유 정답지로 쓰지 말 것 — B 사건에서 트립이 빠져 있어
--     "몇 분째 놀고 있었다" 같은 착시가 난다(조사 중 두 번 걸렸다). 자유는 원천 드랍 로그로 본다.
--   · 자유 로그와 풀의 in-flight 앵커가 **같은 무브로그 가족**이다 → 어떤 트립이 통째로 빠지면
--     B 로 오분류되면서 동시에 풀에서도 빠진다(상관된 결측). 크기는 ⑧절에서 잰다.
--   · `assigned_tt_hist` 는 자유 로그 분류기와는 독립이지만 **풀과는 독립이 아니다**
--     (라이브 쌍둥이 `live_assigned_tt` 가 `free_tos` 갈래를 억제하는 입력이다). 반증에 쓸 때 이 점을 감안한다.

\set ON_ERROR_STOP on
\pset null '-'
\if :{?win_lo} \else \set win_lo '2026-08-25T07:45:00Z' \endif
\if :{?pool_ver} \else \set pool_ver 8 \endif

BEGIN;
SET LOCAL statement_timeout = '300s';

CREATE TEMP TABLE b ON COMMIT DROP AS
  SELECT :'win_lo'::timestamptz AS lo,
         -- 위 경계를 명단 기록 끝에서 15분 물린다: 갓 들어온 사건은 채점 재료가 덜 찼다.
         -- (자유 원천 착지 지연 실측 중앙 33~34초·최대 180초라 15분이면 넉넉하다.)
         (SELECT max(ts) FROM stage2_pool_truck_shadow WHERE pool_ver = :pool_ver) - interval '15 minutes' AS hi;

\echo ''
\echo '=== ⓪ 창과 보관 한계 — 창이 원천 보관 기간 밖으로 나가면 결과가 조용히 뒤집힌다 ==='
SELECT src AS "원천", oldest AT TIME ZONE 'Asia/Kuala_Lumpur' AS "가장 오래된 기록(MYT)",
       CASE WHEN (SELECT lo FROM b) >= oldest THEN 'OK' ELSE '⚠ 창이 보관 밖 — 이 결과는 못 믿는다' END AS "판정"
  FROM (SELECT 'stage2_pool_truck_shadow' src, min(ts) oldest FROM stage2_pool_truck_shadow WHERE pool_ver = :pool_ver
        UNION ALL SELECT 'assigned_tt_hist', min(as_of_ts) FROM assigned_tt_hist
        UNION ALL SELECT 'truck_pos_hist', min(ts) FROM truck_pos_hist
        UNION ALL SELECT 'qc_move_log', min(comp_ts) FROM qc_move_log
        UNION ALL SELECT 'tos_handover_label', min(comp_ts) FROM tos_handover_label) s ORDER BY 1;
SELECT lo AT TIME ZONE 'Asia/Kuala_Lumpur' AS "창 시작(MYT)",
       hi AT TIME ZONE 'Asia/Kuala_Lumpur' AS "창 끝(MYT)",
       round(extract(epoch FROM hi-lo)/3600.0, 1) AS "시간" FROM b;

CREATE TEMP TABLE frees ON COMMIT DROP AS
  SELECT trk_id AS ytno, comp_ts AS free_ts FROM qc_move_log, b
   WHERE jobtype = 'LD' AND trk_id IS NOT NULL AND comp_ts >= b.lo - interval '3 hours' AND comp_ts <= b.hi
  UNION ALL
  SELECT ytno, comp_ts FROM tos_handover_label, b
   WHERE jobtype = 'DS' AND ytno IS NOT NULL AND comp_ts >= b.lo - interval '3 hours' AND comp_ts <= b.hi;
CREATE INDEX ON frees (ytno, free_ts);
-- 픽업 앵커: "지금 상자를 싣고 있나"를 판별한다(마지막 픽업 > 마지막 자유).
CREATE TEMP TABLE picks ON COMMIT DROP AS
  SELECT trk_id AS ytno, comp_ts AS pick_ts FROM rtg_move_log, b
   WHERE jobtype = 'LD' AND trk_id IS NOT NULL AND comp_ts >= b.lo - interval '3 hours' AND comp_ts <= b.hi
  UNION ALL
  SELECT trk_id, comp_ts FROM qc_move_log, b
   WHERE jobtype = 'DS' AND trk_id IS NOT NULL AND comp_ts >= b.lo - interval '3 hours' AND comp_ts <= b.hi;
CREATE INDEX ON picks (ytno, pick_ts);
ANALYZE frees; ANALYZE picks;

-- 사건: (트럭, 배차시각) 단위. ⚠유형(DS/LD)이 한 쌍 안에서 갈리는 경우가 있어(⑨절에서 크기를 잰다)
--       **행 수가 많은 쪽으로 결정적으로** 고른다 — 삽입 순서에 라벨이 걸리지 않게.
--       ⚠상관 서브쿼리로 하면 라이브에서 안 끝난다(실측). 전부 집합 연산으로.
CREATE TEMP TABLE raw ON COMMIT DROP AS
  SELECT d.tos_ytno AS ytno, d.t1_ts, d.jobtype, d.qc, d.queuename, d.ts
    FROM dispatch_compare_shadow d, b
   WHERE d.t1_ver = 1 AND d.reco_src IS NOT NULL AND d.jobtype IN ('DS','LD')
     AND d.t1_ts >= b.lo AND d.t1_ts <= b.hi;
CREATE INDEX ON raw (ytno, t1_ts);
ANALYZE raw;
CREATE TEMP TABLE jt ON COMMIT DROP AS
  SELECT DISTINCT ON (ytno, t1_ts) ytno, t1_ts, jobtype
    FROM (SELECT ytno, t1_ts, jobtype, count(*) AS n FROM raw GROUP BY 1,2,3) g
   ORDER BY ytno, t1_ts, n DESC, jobtype;
CREATE INDEX ON jt (ytno, t1_ts);
ANALYZE jt;
CREATE TEMP TABLE ev ON COMMIT DROP AS
  SELECT DISTINCT ON (r.ytno, r.t1_ts) r.ytno, r.t1_ts, j.jobtype, r.qc, r.queuename
    FROM raw r JOIN jt j ON j.ytno = r.ytno AND j.t1_ts = r.t1_ts AND j.jobtype = r.jobtype
   ORDER BY r.ytno, r.t1_ts, r.ts ASC;
CREATE INDEX ON ev (ytno, t1_ts);
ANALYZE ev;
-- 넓은 핸드오버 원천(⑧절 전용): 유형 불문 전 무브로그. 한 번만 훑는다.
CREATE TEMP TABLE any_ho ON COMMIT DROP AS
  SELECT trk_id AS ytno, comp_ts AS ts FROM qc_move_log, b
   WHERE trk_id IS NOT NULL AND comp_ts >= b.lo - interval '3 hours' AND comp_ts <= b.hi
  UNION ALL
  SELECT trk_id, comp_ts FROM rtg_move_log, b
   WHERE trk_id IS NOT NULL AND comp_ts >= b.lo - interval '3 hours' AND comp_ts <= b.hi
  UNION ALL
  SELECT ytno, comp_ts FROM tos_handover_label, b
   WHERE ytno IS NOT NULL AND comp_ts >= b.lo - interval '3 hours' AND comp_ts <= b.hi;
CREATE INDEX ON any_ho (ytno, ts);
ANALYZE any_ho;

CREATE TEMP TABLE evc ON COMMIT DROP AS
SELECT e.*,
  lag(t1_ts)     OVER (PARTITION BY ytno ORDER BY t1_ts) AS prev_t1,
  lag(qc)        OVER (PARTITION BY ytno ORDER BY t1_ts) AS prev_qc,
  lag(queuename) OVER (PARTITION BY ytno ORDER BY t1_ts) AS prev_q,
  (SELECT max(f.free_ts) FROM frees f WHERE f.ytno = e.ytno AND f.free_ts <= e.t1_ts) AS last_free,
  (SELECT max(p.pick_ts) FROM picks p WHERE p.ytno = e.ytno AND p.pick_ts <= e.t1_ts) AS last_pick,
  NOT EXISTS (SELECT 1 FROM stage2_match_shadow s
               WHERE s.ytno = e.ytno AND s.ts <= e.t1_ts AND s.ts >= e.t1_ts - interval '150 seconds') AS no_reco,
  (SELECT p.reason FROM stage2_pool_truck_shadow p
     WHERE p.ytno = e.ytno AND p.pool_ver = :pool_ver
       AND p.ts <= e.t1_ts AND p.ts >= e.t1_ts - interval '150 seconds'
     ORDER BY p.ts DESC LIMIT 1) AS pool_reason,
  (SELECT min(p.ts) FROM stage2_pool_truck_shadow p
     WHERE p.ytno = e.ytno AND p.pool_ver = :pool_ver
       AND p.ts > e.t1_ts AND p.ts <= e.t1_ts + interval '15 minutes') AS pool_after,
  (SELECT h.ts FROM truck_pos_hist h WHERE h.ytno = e.ytno AND h.ts <= e.t1_ts
     ORDER BY h.ts DESC LIMIT 1) AS last_pos_ts,
  (SELECT a.jobstatus FROM assigned_tt_hist a WHERE a.ytno = e.ytno
     AND a.as_of_ts <= e.t1_ts AND a.as_of_ts >= e.t1_ts - interval '150 seconds'
     ORDER BY a.as_of_ts DESC LIMIT 1) AS tos_status
FROM ev e;
ALTER TABLE evc ADD COLUMN kind text;
UPDATE evc SET kind =
  CASE WHEN last_free IS NULL                                                 THEN 'C_자유기록 없음'
       WHEN prev_t1 IS NULL AND t1_ts - last_free <= interval '120 seconds'   THEN 'A_새 요청'
       WHEN prev_t1 IS NULL                                                   THEN 'D_창 첫사건·판정보류'
       WHEN last_free > prev_t1                                               THEN 'A_새 요청'
       ELSE 'B_직전 사건 뒤 자유 없음' END;
CREATE INDEX ON evc (jobtype, kind);
ANALYZE evc;

\echo ''
\echo '=== ① 분모 감사 — 비교기가 센 "배차 사건"의 정체 ==='
SELECT jobtype AS "유형", kind AS "사건 종류", count(*) AS "건수",
       round(100.0*count(*)/sum(count(*)) OVER (PARTITION BY jobtype), 1) AS "비중%",
       round(100.0*count(*) FILTER (WHERE NOT no_reco)/count(*), 1) AS "커버%"
  FROM evc GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '=== ② 트럭 축 커버리지 — 분모를 고치기 전/후 ==='
SELECT jobtype AS "유형", count(*) AS "전체 사건",
       round(100.0*count(*) FILTER (WHERE NOT no_reco)/count(*), 1) AS "전체 기준 커버%",
       count(*) FILTER (WHERE kind = 'A_새 요청') AS "새 요청",
       round(100.0*count(*) FILTER (WHERE kind='A_새 요청' AND NOT no_reco)
             / NULLIF(count(*) FILTER (WHERE kind='A_새 요청'),0), 1) AS "새 요청 기준 커버%"
  FROM evc GROUP BY 1 ORDER BY 1;

\echo ''
\echo '=== ③ 진짜 남은 몫 — 새 요청인데 추천이 없던 사건의 원인 ==='
\echo '    ⚠비율로 읽을 것. 절대건수는 창이 바뀌면 달라진다.'
SELECT jobtype AS "유형", count(*) AS "놓침",
  count(*) FILTER (WHERE pool_reason IS NOT NULL) AS "명단엔 있었음",
  round(100.0*count(*) FILTER (WHERE pool_reason IS NOT NULL)/count(*),1) AS "명단있음%",
  count(*) FILTER (WHERE pool_reason IS NULL AND last_pos_ts IS NULL) AS "위치행 없음",
  count(*) FILTER (WHERE pool_reason IS NULL AND last_pos_ts IS NOT NULL
                     AND t1_ts - last_pos_ts > interval '10800 seconds') AS "위치행 3h 초과",
  count(*) FILTER (WHERE pool_reason IS NULL AND last_pos_ts IS NOT NULL
                     AND t1_ts - last_pos_ts <= interval '10800 seconds') AS "위치 신선한데 명단 밖",
  count(*) FILTER (WHERE t1_ts - last_free <= interval '90 seconds') AS "자유 뒤 90초 안",
  count(*) FILTER (WHERE pool_after IS NOT NULL AND pool_after - t1_ts <= interval '120 seconds') AS "T1 뒤 120초 안 명단 진입"
  FROM evc WHERE kind = 'A_새 요청' AND no_reco GROUP BY 1 ORDER BY 1;

\echo ''
\echo '=== ⑦ B 는 무엇인가 — 이름을 붙이기 전에 잰다 ==='
\echo '    싣고 있음 = 마지막 픽업 > 마지막 자유(풀의 in-flight 앵커와 같은 계열).'
SELECT jobtype AS "유형", kind AS "사건 종류", count(*) AS "건수",
  round(100.0*count(*) FILTER (WHERE last_pick IS NOT NULL AND (last_free IS NULL OR last_pick > last_free))
        /count(*),1) AS "T1에 싣고 있음%",
  round(100.0*count(*) FILTER (WHERE prev_qc = qc AND prev_q = queuename)/count(*),1) AS "직전과 같은 크레인+구역%",
  round(100.0*count(*) FILTER (WHERE prev_qc IS DISTINCT FROM qc)/count(*),1) AS "직전과 다른 크레인%"
  FROM evc WHERE kind IN ('A_새 요청','B_직전 사건 뒤 자유 없음') GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '=== ④ 반증 1 — TOS 자신의 배차 목록이 T1 에 이 트럭을 뭐라고 부르나 ==='
\echo '    예측: B 가 "직전 배차를 아직 안 끝냈다"면 TOS 목록에 실려 있어야 하고, A 는 없어야 한다.'
\echo '    ⚠이 표는 우리 풀과 완전히 독립은 아니다(라이브 쌍둥이가 풀 입력) — 자유 로그 분류기와만 독립이다.'
SELECT jobtype AS "유형", kind AS "사건 종류", count(*) AS "건수",
  round(100.0*count(*) FILTER (WHERE tos_status = 'A')/count(*),1) AS "작업 활성 A%",
  round(100.0*count(*) FILTER (WHERE tos_status = 'Q')/count(*),1) AS "대기 배정 Q%",
  round(100.0*count(*) FILTER (WHERE tos_status IS NOT NULL)/count(*),1) AS "목록에 실림(A+Q)%"
  FROM evc WHERE kind IN ('A_새 요청','B_직전 사건 뒤 자유 없음') GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '=== ⑤ 위약 — 층을 맞춰도 A/B 차이가 남는가 ==='
\echo '    ⚠축을 "직전 배차 이후 경과"로 잡는다. "자유 이후 경과"는 A 에선 논 시간이지만'
\echo '    B 에선 직전 작업 수행시간까지 포함해 **두 팔에서 서로 다른 양**이 된다(비교 불가).'
SELECT jobtype AS "유형",
  CASE WHEN t1_ts - prev_t1 <= interval '60 seconds'  THEN '0~60초'
       WHEN t1_ts - prev_t1 <= interval '300 seconds' THEN '1~5분'
       WHEN t1_ts - prev_t1 <= interval '900 seconds' THEN '5~15분'
       ELSE '15분+' END AS "직전 배차 뒤 경과",
  count(*) FILTER (WHERE kind='A_새 요청') AS "A 건수",
  round(100.0*count(*) FILTER (WHERE kind='A_새 요청' AND NOT no_reco)
        / NULLIF(count(*) FILTER (WHERE kind='A_새 요청'),0), 1) AS "A 커버%",
  count(*) FILTER (WHERE kind LIKE 'B_%') AS "B 건수",
  round(100.0*count(*) FILTER (WHERE kind LIKE 'B_%' AND NOT no_reco)
        / NULLIF(count(*) FILTER (WHERE kind LIKE 'B_%'),0), 1) AS "B 커버%"
  FROM evc WHERE prev_t1 IS NOT NULL GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '=== ⑧ 상관된 결측의 상한 — 자유 원천을 전 유형으로 넓히면 B 가 얼마나 A 로 옮겨가나 ==='
\echo '    자유 로그와 풀의 in-flight 앵커가 같은 무브로그 가족이라, 트립이 통째로 빠지면'
\echo '    B 로 오분류되면서 동시에 풀에서도 빠진다. 그 상한을 잰다.'
SELECT e.jobtype AS "유형", count(*) AS "B 건수",
  count(*) FILTER (WHERE EXISTS (SELECT 1 FROM any_ho h
      WHERE h.ytno = e.ytno AND h.ts > e.prev_t1 AND h.ts <= e.t1_ts)) AS "넓은 원천엔 사건 있음",
  round(100.0*count(*) FILTER (WHERE EXISTS (SELECT 1 FROM any_ho h
      WHERE h.ytno = e.ytno AND h.ts > e.prev_t1 AND h.ts <= e.t1_ts))/count(*),1) AS "상한%"
  FROM evc e WHERE e.kind = 'B_직전 사건 뒤 자유 없음' GROUP BY 1 ORDER BY 1;

\echo ''
\echo '=== ⑨ 라벨 모호성 — (트럭,배차시각) 한 쌍에 DS/LD 가 섞인 경우 ==='
SELECT count(*) AS "모호한 쌍", (SELECT count(*) FROM ev) AS "전체 사건",
       round(100.0*count(*)/NULLIF((SELECT count(*) FROM ev),0),2) AS "비중%"
  FROM (SELECT d.tos_ytno, d.t1_ts FROM dispatch_compare_shadow d, b
         WHERE d.t1_ver=1 AND d.reco_src IS NOT NULL AND d.jobtype IN ('DS','LD')
           AND d.t1_ts>=b.lo AND d.t1_ts<=b.hi
         GROUP BY 1,2 HAVING count(DISTINCT d.jobtype) > 1) m;

\echo ''
\echo '=== ⑥ 안정성 — 최근 6시간만으로 다시 ==='
SELECT jobtype AS "유형", kind AS "사건 종류", count(*) AS "건수",
       round(100.0*count(*)/sum(count(*)) OVER (PARTITION BY jobtype), 1) AS "비중%",
       round(100.0*count(*) FILTER (WHERE NOT no_reco)/count(*), 1) AS "커버%"
  FROM evc, b WHERE t1_ts >= b.hi - interval '6 hours' GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '=== ⑩ 시간대별 (3시간) — 새 요청 기준 커버리지가 흔들리는가 ==='
SELECT date_trunc('hour', t1_ts AT TIME ZONE 'Asia/Kuala_Lumpur')
         - make_interval(hours => (extract(hour FROM t1_ts AT TIME ZONE 'Asia/Kuala_Lumpur')::int % 3)) AS "구간(MYT)",
       jobtype AS "유형", count(*) AS "새 요청",
       round(100.0*count(*) FILTER (WHERE NOT no_reco)/count(*), 1) AS "커버%"
  FROM evc WHERE kind = 'A_새 요청' GROUP BY 1,2 ORDER BY 1,2;

ROLLBACK;
