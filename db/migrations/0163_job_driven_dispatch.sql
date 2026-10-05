-- 0163: 작업 기준 배차 — TOS 연계 방식에 맞춘 매칭 (2026-10-06 사용자 확정 설계)
--
-- 연계: 우리가 작업↔트럭 짝을 TOS 에 통보하면 TOS 가 **즉시 적용**한다. TOS 는 일하는 트럭에
-- 다음 작업 예약을 받지 않고, 우리가 안 보낸 빈 트럭은 그냥 둔다. 그래서 이 경계부터:
--   · 1계층(마감 도래) = **내보내는 짝**. 후보 트럭은 **지금 빈 트럭만**(free_tos·free_gps).
--   · 2계층(마감 미도래) = **내보내지 않는 계획**. 후보 = 지금 빈 트럭 + 곧 빌 트럭 중
--     "그 작업 배차 마감 전에 실제로 비었을 비율 ≥ 80%" 인 것(learn_soon_free_reliability).
--   · 두 층을 **함께** 푼다(08-25 "따로 풀기"를 사용자가 바꿈). 1계층 덮개는 마감 이른 순 우선으로
--     먼저 정하고(1계층만 풀 때와 같은 수), 그 안에서 전체 빈 차 주행을 최소로 한다.
--   · 재지향(pool_ver 8: 새 작업이 배차된 트럭을 빼앗기) 제거 — "새 작업은 스왑에 안 낀다".
--   · 스왑 단계 신설: 이미 배차되고 픽업 전인 짝들끼리만 맞바꾼다(stage2_swap_shadow).
-- 멱등: 두 번 돌려도 안전하다(IF NOT EXISTS·COMMENT 덮어쓰기).
SET lock_timeout = '5s';

-- ── 곧 빌 트럭 신뢰도 ──────────────────────────────────────────────────────────────────────
-- 후보 풀에 "곧 빔"으로 든 (트럭, 틱) 쌍이 그 뒤 s초 안에 **실제로 짐을 내린 비율**.
-- 정답지 = TOS 내림 기록(적하 qc_move_log·양하 tos_handover_label — 매처 자유 판정과 같은 원천).
-- 갈래 = (풀 사유, 그 트럭의 지금 작업유형, 예측 남은 시간 구간). 창 = 24시간(추적 30분을 다 채운 행만).
-- 갱신 = api 의 selfcal 루프(15분)가 CONCURRENTLY 로. 실측 2.4초(2026-10-06·28만 행).
-- ⚠분모가 (트럭, 틱) 쌍이라 오래 머무는 트럭이 더 무겁다 — 매 틱 판정에 쓰는 값이라 그게 맞는 가중이다.
CREATE MATERIALIZED VIEW IF NOT EXISTS learn_soon_free_reliability AS
WITH p AS (
  SELECT p.ts, p.ytno, p.reason, p.jobtype,
         CASE WHEN p.free_in_s <= 0 THEN 0 WHEN p.free_in_s <= 60 THEN 60 WHEN p.free_in_s <= 300 THEN 300
              WHEN p.free_in_s <= 900 THEN 900 ELSE 3600 END AS pred_b
    FROM stage2_pool_truck_shadow p
   WHERE p.ts > now() - interval '24 hours 30 minutes' AND p.ts <= now() - interval '30 minutes'
     AND p.reason LIKE 'inflight%' AND p.pool_ver >= 8 AND p.jobtype IN ('DS','LD')
), d AS MATERIALIZED (
  SELECT p.*, CASE p.jobtype
     WHEN 'LD' THEN (SELECT min(q.comp_ts) FROM qc_move_log q WHERE q.trk_id = p.ytno AND q.jobtype = 'LD'
                        AND q.comp_ts > p.ts AND q.comp_ts <= p.ts + interval '30 minutes')
     WHEN 'DS' THEN (SELECT min(h.comp_ts) FROM tos_handover_label h WHERE h.ytno = p.ytno AND h.jobtype = 'DS'
                        AND h.comp_ts > p.ts AND h.comp_ts <= p.ts + interval '30 minutes')
   END AS drop_ts FROM p
)
SELECT reason, jobtype, pred_b::int4 AS pred_b, count(*)::int4 AS n,
       (count(*) FILTER (WHERE drop_ts <= ts + interval '60 seconds')::real   / count(*)) AS f60,
       (count(*) FILTER (WHERE drop_ts <= ts + interval '120 seconds')::real  / count(*)) AS f120,
       (count(*) FILTER (WHERE drop_ts <= ts + interval '180 seconds')::real  / count(*)) AS f180,
       (count(*) FILTER (WHERE drop_ts <= ts + interval '300 seconds')::real  / count(*)) AS f300,
       (count(*) FILTER (WHERE drop_ts <= ts + interval '450 seconds')::real  / count(*)) AS f450,
       (count(*) FILTER (WHERE drop_ts <= ts + interval '600 seconds')::real  / count(*)) AS f600,
       (count(*) FILTER (WHERE drop_ts <= ts + interval '900 seconds')::real  / count(*)) AS f900,
       (count(*) FILTER (WHERE drop_ts <= ts + interval '1200 seconds')::real / count(*)) AS f1200,
       (count(*) FILTER (WHERE drop_ts <= ts + interval '1800 seconds')::real / count(*)) AS f1800,
       now() AS built_at
  FROM d GROUP BY 1, 2, 3;
CREATE UNIQUE INDEX IF NOT EXISTS learn_soon_free_reliability_key
  ON learn_soon_free_reliability (reason, jobtype, pred_b);
COMMENT ON MATERIALIZED VIEW learn_soon_free_reliability IS
  '곧 빌 트럭 신뢰도(mig 0163): 후보 풀 (트럭,틱) 쌍이 s초 안에 실제로 짐을 내린 비율 f<s>. '
  '갈래 = (reason, jobtype=그 트럭의 지금 작업, pred_b=예측 남은 시간 구간 0/60/300/900/3600). '
  '매처가 2계층 후보 자격(배차 마감 전에 비었을 비율 ≥ 0.8)과 그 시각을 여기서 읽는다. n<30 갈래는 안 쓴다.';

-- ── 스왑 추천 ──────────────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS stage2_swap_shadow (
  ts          timestamptz NOT NULL,
  tick        int8,
  ytno_a      text NOT NULL,
  ytno_b      text NOT NULL,
  jobtype     text NOT NULL,
  a_qc        text, a_queuename text, a_contno text,   -- 트럭 A 가 지금 배차받은 작업
  b_qc        text, b_queuename text, b_contno text,   -- 트럭 B 가 지금 배차받은 작업
  a_before_s  int4, b_before_s int4,                   -- 지금 짝 그대로 갈 때 빈 차 주행(초)
  a_after_s   int4, b_after_s  int4,                   -- 맞바꾸면 A→B작업, B→A작업 빈 차 주행(초)
  gain_s      int4 NOT NULL,                           -- (a_before+b_before) − (a_after+b_after)
  a_dist_m    real, b_dist_m real,                     -- 각자 지금 목적지(픽업 지점)까지 거리
  PRIMARY KEY (ts, ytno_a)
);
CREATE INDEX IF NOT EXISTS stage2_swap_shadow_a ON stage2_swap_shadow (ytno_a, ts);
CREATE INDEX IF NOT EXISTS stage2_swap_shadow_b ON stage2_swap_shadow (ytno_b, ts);
COMMENT ON TABLE stage2_swap_shadow IS
  '스왑 추천(mig 0163): 이미 배차되고 픽업 전인 두 트럭의 행선지를 맞바꾸는 추천. 새 작업·새 빈 트럭은 '
  '끼지 않는다(사용자 확인 — TOS 도 기 패칭 결과 안에서만 스왑). 억제 3조건: 이득 ≥ 180초 · 둘 다 '
  '목적지 500m 밖 · 한 트럭은 자유 사이(한 번의 배차) 동안 1회만(그 뒤 이 표에 있으면 동결). 같은 '
  '작업유형끼리만. 그림자라 TOS 가 적용하지 않으므로 같은 짝이 남아도 다시 추천하지 않는다(동결).';

-- ── 판별자 ─────────────────────────────────────────────────────────────────────────────────
ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS match_ver int2;
ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS free_q80_s int4;
ALTER TABLE stage2_match_shadow ADD COLUMN IF NOT EXISTS veh_jobtype text;
COMMENT ON COLUMN stage2_match_shadow.match_ver IS
  '매칭 규칙 판. NULL = 경계 이전 · 1 = 작업 기준 배차(mig 0163·2026-10-06~): 1계층 = 내보내는 짝(지금 빈 '
  '트럭만) · 2계층 = 내보내지 않는 계획(곧 빌 트럭 포함) · 두 층 함께 풀기 · 재지향 없음. '
  '⚠경계 전후로 1계층 모집단이 다르다(곧 빌 트럭이 빠짐) — 시계열은 이 값으로 먼저 가를 것.';
COMMENT ON COLUMN stage2_match_shadow.match_tier IS
  '발행 계층: 1 = 마감 도래 슬롯 · 2 = 마감 미도래 지시(2026-08-25 mig 0161부터). NULL = 0161 경계 이전'
  '(그때는 1계층만 존재). ★mig 0163(match_ver=1)부터 1 = **내보내는 짝**(TOS 즉시 적용 대상)·2 = **내보내지 '
  '않는 계획**. 종전 시계열과 비교할 때는 match_tier IS DISTINCT FROM 2 로 거를 것.';
COMMENT ON COLUMN stage2_match_shadow.free_q80_s IS
  '2계층 곧 빌 트럭의 자격 시각(mig 0163): 신뢰도 표에서 실제로 비었을 비율이 처음 0.8 이상이 되는 s(초). '
  '자격 = 이 값 ≤ 배차 마감까지 남은 초. 지금 빈 트럭 = 0. NULL = 경계 이전 또는 1계층.';
COMMENT ON COLUMN stage2_match_shadow.veh_jobtype IS
  '배정된 트럭의 지금(또는 방금 끝낸) 작업유형(mig 0163) — 곧 빌 트럭의 실제 내림을 채점할 때 어느 로그'
  '(적하 qc_move_log·양하 tos_handover_label)를 볼지 정한다. jobtype 컬럼(배정될 작업의 유형)과 다르다.';

ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS match_ver int2;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS n_free int4;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS n_soon_ok int4;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS t1_cov_alone int4;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS t1_cov int4;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS t1_skip_n int4;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS seq_t1_cost_s int8;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS seq_t2_cost_s int8;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS seq_t2_n int4;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS joint_t1_cost_s int8;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS joint_t2_cost_s int8;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS joint_t2_n int4;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS swap_cand_n int4;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS swap_n int4;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS swap_gain_s int8;
COMMENT ON COLUMN stage2_solver_shadow.match_ver IS
  '매칭 규칙 판(stage2_match_shadow.match_ver 와 같은 값). NULL = mig 0163 경계 이전.';
COMMENT ON COLUMN stage2_solver_shadow.n_free IS '지금 빈 트럭 수(1계층 후보 = 1계층 슬롯 상한). mig 0163.';
COMMENT ON COLUMN stage2_solver_shadow.n_soon_ok IS '신뢰도 표에 자격 시각이 있는 곧 빌 트럭 수(2계층 후보가 될 수 있는 것). mig 0163.';
COMMENT ON COLUMN stage2_solver_shadow.t1_cov_alone IS
  '1계층만 풀 때 트럭을 받는 급한 작업 슬롯 수(마감 이른 순 우선 덮개). mig 0163.';
COMMENT ON COLUMN stage2_solver_shadow.t1_cov IS
  '함께 풀었을 때 트럭을 받은 급한 작업 슬롯 수. ★t1_cov_alone 과 항상 같아야 한다(함께 풀기가 급한 작업을 덜 덮으면 결함).';
COMMENT ON COLUMN stage2_solver_shadow.t1_skip_n IS
  '마감이 더 이른 급한 작업이 트럭을 못 받았는데, 더 늦은 급한 작업의 트럭을 넘기면 받을 수 있었던 경우의 수. 0 이어야 한다.';
COMMENT ON COLUMN stage2_solver_shadow.seq_t1_cost_s IS
  '비교용 — 따로 풀기(1계층 먼저·2계층은 남은 트럭)였다면의 1계층 빈 차 주행 합(초). joint_* 와 같은 간선·같은 계획 가치.';
COMMENT ON COLUMN stage2_solver_shadow.joint_t1_cost_s IS
  '함께 풀기(실제 추천)의 1계층 빈 차 주행 합(초). seq 대비 늘면 1계층이 계획을 위해 먼 트럭을 감수한 것.';
COMMENT ON COLUMN stage2_solver_shadow.joint_t2_n IS '함께 풀기에서 계획(2계층)이 성립한 수. seq_t2_n 과 비교.';
COMMENT ON COLUMN stage2_solver_shadow.swap_cand_n IS '스왑 후보(배차됨·픽업 전·신선 GPS·목적지 500m 밖·미동결) 트럭 수.';
COMMENT ON COLUMN stage2_solver_shadow.swap_n IS '이번 틱 스왑 추천 쌍 수(stage2_swap_shadow 행 수).';
COMMENT ON COLUMN stage2_solver_shadow.swap_gain_s IS '이번 틱 스왑 추천의 빈 차 주행 감소 합(초).';

COMMENT ON COLUMN stage2_pool_truck_shadow.pool_ver IS
  '풀 규칙 판. 1~7 = 2026-08-19~24 경계(livemap.rs POOL_VER 주석) · 8 = 재지향 가능 공차 갈래(mig 0160·08-25) · '
  '9 = 재지향 갈래 제거 + **배차됨·픽업 전 트럭은 풀 밖**(곧 빌 트럭이 아니라 이제 일을 시작할 트럭이다 — '
  'v8 에선 픽업 지점 500m 안이면 곧-빔 갈래로 새어 들어왔다)·mig 0163·2026-10-06. 재현율은 이 값으로 가를 것.';
