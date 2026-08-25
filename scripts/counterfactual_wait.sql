-- 반사실 대기 측정 — "우리 추천대로 그 시각에 그 트럭을 보냈다면 크레인 앞에서 몇 분 기다렸을까".
--   psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/counterfactual_wait.sql
--
-- ⚠⚠ 2026-08-19 정정 두 건 — 먼저 읽을 것
-- (1) **TOS 배차시각과의 비교(②)는 판정 축에서 내렸다.** 현장은 차량이 작업을 고르는 pull 구조라
--     `dispatch_ts` 는 "TOS 가 언제 결정했나"가 아니라 **트럭이 언제 비었나**일 뿐이다(실측: 자유 →
--     배차 중앙 15초/38초). ②는 이제 **어느 상자를 언제 크레인이 다뤘는지 찾는 기계적 연결고리**로만
--     쓴다. 합격 기준은 `scripts/pull_model_coverage.sql` 로 옮겼다.
-- (2) **`comp_ts` 는 인계가 끝난 시각**이라 아래 "대기"에는 크레인의 픽업/드랍 동작 시간이 들어 있다.
--     실측(트럭 도착 → comp_ts 하위 분위수, 7일): 대기가 거의 없는 구간이 **양하 29~48초 / 적하 22~40초**.
--     ⇒ ①에 그만큼 뺀 열을 같이 낸다(:hs_ds / :hs_ld). 크기가 작아 결론은 바뀌지 않는다.
--
-- ■ 무엇을 재나
-- 시점이 곧 지시다 — 필요보다 일찍 보내면 트럭이 크레인 앞에서 기다린다. 이 스크립트는 최근 7일
-- 우리가 **처음 추천한 상자**마다, 그 추천대로 즉시 출발했다면 트럭이 크레인 앞에 언제 도착했을지를
-- 우리 자신의 준비시간 모델로 추정하고, 크레인이 그 상자를 **실제로 다룬 시각**과 맞댄다.
--
--   우리 도착(ready) = 추천시각 + arrival_s(무부하주행·곧 빔 대기 포함·p50) + lead_extra_s(그 뒤 QC 인계까지)
--   대기 상한        = comp_ts − ready        (크레인 순서가 고정이라 가정 · 음수 = 트럭이 늦음)
--   대기 하한        = max(0, 직전 무브 comp_ts − ready)  (앞 상자가 끝나기 전 도착이면 그만큼은 확실히 기다림)
--
-- ■ 같은 잣대의 TOS (반드시 나란히 볼 것)
-- 같은 상자를 TOS 는 언제 배차했고 크레인은 언제 다뤘나. `TOS 배차→처리` 와 `우리 추천→처리` 의 차이가
-- 곧 우리가 더한(또는 덜은) 시간이다 — 이 수치는 준비시간 모델과 무관하다.
--
-- ■ 위약 (③) — 잣대 검증
-- TOS 자기 배차에 같은 식(배차시각 + TOS 트럭 arrival + lead_extra)을 적용하면 대기 중앙값이 0 부근에
-- 와야 한다. 안 오면 **잣대(준비시간 모델)가 틀린 것**이지 우리가 이른 게 아니다 — ①의 절대값을 믿지 말고
-- ②의 상대값만 읽을 것.
--
-- ■ 한계 (먼저 읽을 것)
-- comp_ts 는 TOS 배차의 결과다. 크레인이 이 상자를 기다리고 있었다면(굶김) 우리 트럭이 먼저 왔을 때
-- 크레인은 더 일찍 처리했을 것이므로 **상한은 과대**다. 그래서 하한을 같이 내고 굶김 유무로 층화한다.
-- 층화 변수(직전 무브 간격)는 comp_ts 를 공유하므로 결과에서 완전히 독립이 아니다 — ⑤에서 일치율을 낸다.
--
-- ■ 분모
-- "최근 8일~1일 전에 우리가 처음 추천한 (상자, 작업유형)" 이 출발점이고, 그 중 TOS 가 우리 첫 추천
-- 이후(−90초 허용) 배차했고 크레인 처리 실적이 있는 것이 ①~⑤의 분모다. ⓪에 걸러진 수를 전부 적는다.

\set ON_ERROR_STOP on
\pset null '-'
-- 순수 픽업/드랍 동작 시간(초) — 트럭 도착→comp_ts 의 p10(대기 없는 구간). 아래 ⑦에서 재측정한다.
\set hs_ds 48
\set hs_ld 40

BEGIN;
SET LOCAL statement_timeout = '60s';

-- ─────────────────────────────────────────────────────────────────────────────────────────
-- 표본 조립 (임시 표 · ROLLBACK 으로 사라진다)
-- ─────────────────────────────────────────────────────────────────────────────────────────
CREATE TEMP TABLE cf AS
WITH r AS (                                   -- 상자당 첫 추천 (헤드라인) + TOS 배차 직전 마지막 추천 (민감도)
  SELECT contno, jobtype,
         min(ts)                                          AS first_ts,
         (array_agg(ytno        ORDER BY ts))[1]           AS first_ytno,
         (array_agg(arrival_s   ORDER BY ts))[1]           AS first_arrival_s,
         (array_agg(od_p90_s    ORDER BY ts))[1]           AS first_arrival_p90_s,
         (array_agg(lead_extra_s ORDER BY ts))[1]          AS first_extra_s,
         (array_agg(qc          ORDER BY ts))[1]           AS first_qc
    FROM stage2_match_shadow
   WHERE ts BETWEEN now() - interval '8 days' AND now() - interval '1 day'
     AND contno IS NOT NULL AND jobtype IN ('DS','LD')
     AND match_tier IS DISTINCT FROM 2 -- 2계층(미리 배정·mig 0161)이 first_ts 를 몇 시간 당겨 wait_upper 를 부풀린다
   GROUP BY contno, jobtype
), d AS (                                     -- 같은 상자의 TOS 배차 (첫 추천 −90초 이후 가장 이른 것)
  SELECT r.*, t.ytno AS tos_ytno, t.dispatch_ts AS tos_dis_ts, b.dispatch_ts AS tos_before_ts
    FROM r
    LEFT JOIN LATERAL (
      SELECT ytno, dispatch_ts FROM tt_move_log t
       WHERE t.contno = r.contno AND t.jobtype = r.jobtype
         AND t.dispatch_ts >= r.first_ts - interval '90 seconds'
         AND t.dispatch_ts <  r.first_ts + interval '12 hours'
       ORDER BY t.dispatch_ts LIMIT 1) t ON true
    LEFT JOIN LATERAL (                       -- 우리 첫 추천보다 90초 넘게 먼저 배차된 경우 (분모에서 빠지는 이유를 적기 위해)
      SELECT dispatch_ts FROM tt_move_log b
       WHERE b.contno = r.contno AND b.jobtype = r.jobtype
         AND b.dispatch_ts <  r.first_ts - interval '90 seconds'
         AND b.dispatch_ts >= r.first_ts - interval '24 hours'
       ORDER BY b.dispatch_ts DESC LIMIT 1) b ON t.dispatch_ts IS NULL
), c AS (                                     -- 크레인이 실제로 다룬 시각 (배차 이후 첫 comp_ts) + 그 크레인의 직전 무브
  SELECT d.*, q.machno, q.comp_ts,
         p.comp_ts AS prev_comp_ts
    FROM d
    LEFT JOIN LATERAL (
      SELECT machno, comp_ts FROM qc_move_log q
       WHERE q.contno = d.contno AND q.jobtype = d.jobtype
         AND q.comp_ts >= d.tos_dis_ts AND q.comp_ts < d.tos_dis_ts + interval '12 hours'
       ORDER BY q.comp_ts LIMIT 1) q ON d.tos_dis_ts IS NOT NULL
    LEFT JOIN LATERAL (
      SELECT comp_ts FROM qc_move_log p
       WHERE p.machno = q.machno AND p.comp_ts < q.comp_ts
       ORDER BY p.comp_ts DESC LIMIT 1) p ON q.comp_ts IS NOT NULL
), l AS (                                     -- 민감도: TOS 배차 직전 마지막 추천
  SELECT c.contno, c.jobtype, m.ts AS last_ts, m.arrival_s AS last_arrival_s, m.lead_extra_s AS last_extra_s
    FROM c
    JOIN LATERAL (
      SELECT ts, arrival_s, lead_extra_s FROM stage2_match_shadow m
       WHERE m.contno = c.contno AND m.jobtype = c.jobtype
         AND m.ts >= c.first_ts AND m.ts <= c.tos_dis_ts
       ORDER BY m.ts DESC LIMIT 1) m ON c.tos_dis_ts IS NOT NULL
)
SELECT c.*,
       l.last_ts, l.last_arrival_s, l.last_extra_s,
       -- 우리 도착 추정과 대기 (초)
       c.first_ts + make_interval(secs => c.first_arrival_s + c.first_extra_s)               AS ready_ts,
       EXTRACT(epoch FROM c.comp_ts - (c.first_ts + make_interval(secs => c.first_arrival_s + c.first_extra_s)))::int      AS wait_upper_s,
       GREATEST(0, EXTRACT(epoch FROM c.prev_comp_ts - (c.first_ts + make_interval(secs => c.first_arrival_s + c.first_extra_s))))::int AS wait_lower_s,
       EXTRACT(epoch FROM c.comp_ts - (c.first_ts + make_interval(secs => c.first_arrival_p90_s + c.first_extra_s)))::int  AS wait_upper_p90arr_s,
       EXTRACT(epoch FROM c.comp_ts - (l.last_ts + make_interval(secs => l.last_arrival_s + l.last_extra_s)))::int         AS wait_upper_last_s,
       -- 같은 상자의 TOS 와 우리 (준비시간 모델 무관)
       EXTRACT(epoch FROM c.tos_dis_ts - c.first_ts)::int  AS we_earlier_s,        -- 우리 첫 추천 → TOS 배차 (양수 = 우리가 이르다)
       EXTRACT(epoch FROM c.comp_ts - c.tos_dis_ts)::int   AS tos_dis_to_crane_s,  -- TOS 배차 → 크레인 처리
       EXTRACT(epoch FROM c.comp_ts - c.first_ts)::int     AS ours_to_crane_s,     -- 우리 추천 → 크레인 처리
       -- 층화: 그 크레인이 이 상자 직전에 얼마나 놀았나
       EXTRACT(epoch FROM c.comp_ts - c.prev_comp_ts)::int AS crane_gap_s
  FROM c LEFT JOIN l USING (contno, jobtype);

CREATE INDEX ON cf (jobtype);
ANALYZE cf;

-- 위약용: 비교기의 정밀 행만 (ts 인덱스로 자른다 — 원표에는 (tos_ytno,t1_ts) 인덱스가 없어 직접 조인하면 60초를 넘긴다)
CREATE TEMP TABLE cmp AS
SELECT qc, tos_ytno, t1_ts, tos_arrival_s
  FROM dispatch_compare_shadow
 WHERE ts > now() - interval '9 days' AND t1_ver = 1 AND reason <> 'now' AND tos_arrival_s IS NOT NULL;
CREATE INDEX ON cmp (tos_ytno, t1_ts);
ANALYZE cmp;

-- ─────────────────────────────────────────────────────────────────────────────────────────
\echo ''
\echo '════ ⓪ 분모 — 우리가 처음 추천한 (상자,작업유형) 이 어디까지 살아남았나 ════'
SELECT jobtype AS 작업,
       count(*)                                        AS "첫 추천 상자",
       count(*) FILTER (WHERE tos_dis_ts IS NULL AND tos_before_ts IS NOT NULL) AS "TOS 가 우리보다 90초+ 먼저 배차",
       count(*) FILTER (WHERE tos_dis_ts IS NULL AND tos_before_ts IS NULL)     AS "12h 안 TOS 배차 없음",
       count(*) FILTER (WHERE tos_dis_ts IS NOT NULL AND comp_ts IS NULL) AS "배차는 됐으나 처리 실적 없음",
       count(comp_ts)                                  AS "★분모(배차+처리 실적)",
       count(*) FILTER (WHERE comp_ts IS NOT NULL AND prev_comp_ts IS NULL) AS "직전 무브 없음(하한 불가)",
       min(first_ts)::timestamp(0) AS 시작, max(first_ts)::timestamp(0) AS 끝
  FROM cf GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ① 헤드라인 — 우리 추천대로 보냈다면 트럭은 크레인 앞에서 얼마나 기다렸나 (분모=⓪★ · 분) ════'
\echo '   상한 = 크레인 실제 처리 − 우리 도착 · 하한 = 직전 무브 끝 − 우리 도착 (0 미만은 0) · 음수 상한 = 트럭이 늦음'
SELECT jobtype AS 작업, count(*) AS 상자,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY wait_upper_s)/60)::numeric,1) AS "대기 상한 중앙",
       round((percentile_cont(0.9) WITHIN GROUP (ORDER BY wait_upper_s)/60)::numeric,1) AS "상한 p90",
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY wait_lower_s)/60)::numeric,1) AS "대기 하한 중앙",
       round((percentile_cont(0.9) WITHIN GROUP (ORDER BY wait_lower_s)/60)::numeric,1) AS "하한 p90",
       round(100.0*count(*) FILTER (WHERE wait_upper_s < 0)/count(*),1)                  AS "늦음 %(상한<0)",
       round(100.0*count(*) FILTER (WHERE wait_lower_s >= 600)/count(*),1)               AS "확실히 10분+ 대기 %",
       round(100.0*count(*) FILTER (WHERE wait_upper_s >= 600)/count(*),1)               AS "최대 10분+ 대기 %",
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY wait_upper_s - CASE WHEN jobtype='DS' THEN :hs_ds ELSE :hs_ld END)/60)::numeric,1) AS "상한 중앙(동작 제외)"
  FROM cf WHERE comp_ts IS NOT NULL GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ② 같은 상자의 TOS 와 나란히 — 준비시간 모델과 무관한 값 (분모=⓪★ · 분) ════'
\echo '   "우리가 이른 만큼" = TOS 배차 − 우리 첫 추천. 우리 추천→처리 − TOS 배차→처리 = 우리가 더한 시간.'
SELECT jobtype AS 작업, count(*) AS 상자,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY we_earlier_s)/60)::numeric,1)        AS "우리가 이른 만큼 중앙",
       round((percentile_cont(0.9) WITHIN GROUP (ORDER BY we_earlier_s)/60)::numeric,1)        AS "p90",
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY tos_dis_to_crane_s)/60)::numeric,1)  AS "TOS 배차→처리 중앙",
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY ours_to_crane_s)/60)::numeric,1)     AS "우리 추천→처리 중앙",
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY first_arrival_s + first_extra_s)/60)::numeric,1) AS "우리 준비시간 추정 중앙"
  FROM cf WHERE comp_ts IS NOT NULL GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ③ 위약 — TOS 자기 배차에 같은 잣대 (배차시각 + TOS 트럭 arrival + lead_extra) → 대기 중앙이 0 부근이어야 잣대가 맞다 ════'
\echo '   TOS 트럭 arrival 은 비교기(dispatch_compare_shadow·t1_ver=1·정밀행)의 tos_arrival_s. 짝이 안 붙는 상자는 뺀다(수를 적는다).'
WITH p AS (
  SELECT f.jobtype, f.comp_ts, f.tos_dis_ts, f.first_extra_s, x.tos_arrival_s
    FROM cf f
    JOIN LATERAL (
      SELECT tos_arrival_s FROM cmp x
       WHERE x.tos_ytno = f.tos_ytno AND x.qc = f.machno
         AND x.t1_ts BETWEEN f.tos_dis_ts - interval '5 seconds' AND f.tos_dis_ts + interval '5 seconds'
       ORDER BY abs(EXTRACT(epoch FROM x.t1_ts - f.tos_dis_ts)) LIMIT 1) x ON f.comp_ts IS NOT NULL
)
SELECT jobtype AS 작업, count(*) AS "짝 붙은 상자",
       (SELECT count(*) FROM cf WHERE cf.jobtype = p.jobtype AND comp_ts IS NOT NULL) AS "분모(⓪★)",
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM comp_ts - tos_dis_ts) - tos_arrival_s - first_extra_s)/60)::numeric,1) AS "위약 대기 중앙(분)",
       round((percentile_cont(0.9) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM comp_ts - tos_dis_ts) - tos_arrival_s - first_extra_s)/60)::numeric,1) AS "p90",
       round(100.0*count(*) FILTER (WHERE EXTRACT(epoch FROM comp_ts - tos_dis_ts) - tos_arrival_s - first_extra_s < 0)/count(*),1) AS "늦음 %"
  FROM p GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ④ 층화 — 크레인이 이 상자 직전에 5분 넘게 놀았나 (굶김) · 굶긴 층에서는 상한을 믿지 말고 하한을 볼 것 ════'
SELECT jobtype AS 작업,
       CASE WHEN crane_gap_s > 300 THEN '굶김(간격>5분)' ELSE '연속 작업' END AS 층,
       count(*) AS 상자,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY wait_upper_s)/60)::numeric,1) AS "상한 중앙",
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY wait_lower_s)/60)::numeric,1) AS "하한 중앙",
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY we_earlier_s)/60)::numeric,1) AS "우리가 이른 만큼 중앙"
  FROM cf WHERE comp_ts IS NOT NULL AND prev_comp_ts IS NOT NULL GROUP BY 1,2 ORDER BY 1,2;

\echo ''
\echo '════ ⑤ 층화 변수가 결과에서 파생됐나 — 굶김 여부 ↔ (상한>중앙) 일치율. ~100% 면 동어반복, 50% 부근이면 독립 ════'
WITH m AS (
  SELECT jobtype, percentile_cont(0.5) WITHIN GROUP (ORDER BY wait_upper_s) AS med FROM cf WHERE comp_ts IS NOT NULL GROUP BY 1
)
SELECT f.jobtype AS 작업, count(*) AS 상자,
       round(100.0*count(*) FILTER (WHERE (crane_gap_s > 300) = (wait_upper_s > m.med))/count(*),1) AS "굶김↔상한부호 일치율 %",
       round(100.0*count(*) FILTER (WHERE (EXTRACT(minute FROM first_ts)::int % 2 = 0) = (wait_upper_s > m.med))/count(*),1) AS "위약(짝수분)↔상한부호 일치율 %"
  FROM cf f JOIN m USING (jobtype) WHERE comp_ts IS NOT NULL AND prev_comp_ts IS NOT NULL GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ⑥ 민감도 — 첫 추천 대신 TOS 배차 직전 마지막 추천 · arrival p90 (분모=⓪★ · 분) ════'
SELECT jobtype AS 작업, count(*) AS 상자,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY wait_upper_s)/60)::numeric,1)         AS "상한 중앙(첫 추천·p50)",
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY wait_upper_p90arr_s)/60)::numeric,1)  AS "상한 중앙(첫 추천·arrival p90)",
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY wait_upper_last_s)/60)::numeric,1)    AS "상한 중앙(마지막 추천)",
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM last_ts - first_ts))/60)::numeric,1) AS "첫→마지막 추천 간격 중앙"
  FROM cf WHERE comp_ts IS NOT NULL GROUP BY 1 ORDER BY 1;

\echo ''
\echo '════ ⑦ 관측 기준선 — 현행 운영에서 트럭이 크레인에 도착해 인계가 끝날 때까지 (분·GPS 도착 앵커) ════'
\echo '    양하=빈 차 도착→QC 픽업 완료 / 적하=실은 차 도착→QC 드랍 완료. 하위 분위수 ~= 대기 없는 순수 동작.'
\echo '    ⚠①과 구성이 다르다(여기는 GPS 실측 도착, ①은 우리 모델 추정 도착) — 나란히 놓되 같은 값처럼 읽지 말 것.'
WITH a AS (
  SELECT v.ytno, v.jobtype,
         CASE WHEN v.jobtype='DS' THEN v.empty_arrived_at ELSE v.laden_arrived_at END AS arr_at
    FROM tt_cycle_v2 v WHERE v.dropped_at > now()-interval '7 days' AND v.jobtype IN ('DS','LD')
), j AS (
  SELECT a.*, q.comp_ts FROM a
  JOIN LATERAL (SELECT comp_ts FROM qc_move_log q
                 WHERE q.trk_id = a.ytno AND q.jobtype = a.jobtype
                   AND q.comp_ts BETWEEN a.arr_at AND a.arr_at + interval '90 minutes'
                 ORDER BY q.comp_ts LIMIT 1) q ON a.arr_at IS NOT NULL
)
SELECT jobtype AS 작업, count(*) AS n,
  round(percentile_cont(0.05) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM comp_ts-arr_at))::numeric,0) AS "p05(초)",
  round(percentile_cont(0.10) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM comp_ts-arr_at))::numeric,0) AS "p10(초)",
  round((percentile_cont(0.50) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM comp_ts-arr_at))/60)::numeric,1) AS "중앙(분)",
  round((percentile_cont(0.90) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM comp_ts-arr_at))/60)::numeric,1) AS "p90(분)"
 FROM j GROUP BY 1 ORDER BY 1;

ROLLBACK;
