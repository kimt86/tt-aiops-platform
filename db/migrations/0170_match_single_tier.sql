-- match_ver 2 — 계층 없앰 · 헝가리안 최적 매칭 (2026-10-15 사용자 결정).
--
-- ① 후보 작업 = 데드라인이 (지금 + 여유) 안에 든 슬롯만. 여유 기본 300초, tt-api 환경변수 POOL_MARGIN_S 로 실험.
--    데드라인 이른 순으로 후보 트럭 수까지 고른다. 종전 2계층(데드라인 미도래 지시에 붙이던 계획)은 없앴다.
-- ② 후보 트럭 = 지금 빈 트럭 + 곧 빌 트럭(남은 시간을 아는 것 — 신뢰도 표에 갈래가 있는 것). 종전 0.8 자격 문턱은 없앴다.
-- ③ 비용 = 비기까지 남은 시간 + 빈 차 주행 p50 + 들락날락 벌점. 남은 시간은 지금 빈 트럭 0, 곧 빌 트럭은 **실측 보정값**
--    (learn_soon_free_reliability 에서 그 갈래 트럭이 실제로 비었던 비율이 처음 0.5 이상이 되는 초 — 사용자 결정).
--    마감 이른 슬롯부터 덮을 수 있는 만큼 덮고(마감 순 덮개), 그 슬롯들을 헝가리안으로 한 번에 푼다.
-- ④ 배차 지시(dispatch_sent·고정)는 지금 빈 트럭과 짝지어진 작업만. 곧 빌 트럭과의 짝은 기록만 한다.
-- ⑤ 스왑 후보에서 '목적지 500m 밖' 조건을 뺐다(이득 ≥ 180초 문턱이 그 몫을 한다).
--
-- 스키마: stage2_solver_shadow 에 nullable 컬럼 2개(메타데이터만 바뀌는 ADD COLUMN). 나머지는 COMMENT.
-- 멱등 — 두 번 돌려도 같다. 적용 전 pg_stat_activity 로 이 표의 긴 세션을 확인하고, lock_timeout 으로 API 쓰기를 오래 막지 않는다.
SET lock_timeout = '3s';

ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS pool_margin_s int4;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS soon_assign_n int4;
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS soon_unknown_n int4;

COMMENT ON COLUMN stage2_solver_shadow.pool_margin_s IS
  '이 틱이 쓴 데드라인 여유(초) — 데드라인이 (지금 + 이 값) 안에 든 작업만 후보로 삼았다. 기본 300, tt-api 환경변수 POOL_MARGIN_S(재시작). '
  'NULL = mig 0170 이전(그때는 300 고정). 실험 구간은 이 값으로 가를 것.';
COMMENT ON COLUMN stage2_solver_shadow.soon_assign_n IS
  'match_ver 2(mig 0170): 곧 빌 트럭이 받은 짝 수 — 지시를 내지 않았다(그 작업은 다음 틱에 다시 풀린다). '
  'optimal_n − 이 값 = 지금 빈 트럭과의 짝(지시 냄).';

COMMENT ON COLUMN stage2_match_shadow.match_ver IS
  '매칭 규칙 판. NULL = 경계 이전 · 1 = 작업 기준 배차(mig 0163·2026-10-06~): 1계층 = 내보내는 짝(지금 빈 트럭만) · 2계층 = 내보내지 않는 계획(곧 빌 트럭 포함) · 두 층 함께 풀기 · 재지향 없음. '
  '2 = 계층 없앰(mig 0170·2026-10-15~): 후보 작업 = 데드라인 도래분만 · 후보 트럭 = 지금 빈 + 곧 빌 전부 · 헝가리안(비용 = 남은 시간 + 빈 차 주행) · 지시는 지금 빈 트럭 짝만. '
  '⚠판마다 모집단이 다르다 — 시계열은 이 값으로 먼저 가를 것.';
COMMENT ON COLUMN stage2_match_shadow.match_tier IS
  'match_ver 1: 1 = 내보내는 짝(데드라인 도래·지금 빈 트럭) · 2 = 내보내지 않는 계획(데드라인 미도래). '
  'match_ver 2(mig 0170): 1 = 지금 빈 트럭과의 짝(지시 냄) · 2 = 곧 빌 트럭과의 짝(지시 안 냄 — 작업은 데드라인 도래분). '
  '두 판 모두 "match_tier IS DISTINCT FROM 2" = 보낸 짝만. NULL = mig 0161 이전.';
COMMENT ON COLUMN stage2_match_shadow.free_q80_s IS
  'match_ver 1: 곧 빌 트럭의 자격 시각(실제로 비었을 비율이 처음 0.8 이상이 되는 초, 지금 빈 트럭 = 0). '
  'match_ver 2(mig 0170): 비용에 쓴 남은 시간(실측 보정값 — 그 갈래 트럭이 실제로 비었던 비율이 처음 0.5 이상이 되는 초). 지금 빈 트럭 0.';
COMMENT ON COLUMN stage2_match_shadow.arrival_s IS
  '트럭이 비기까지 시간(지금 빈 트럭 0 · match_ver 1 은 예측값, match_ver 2 는 실측 보정값 = free_q80_s) + 픽업 지점까지 빈 차 주행 p50(초). ⚠mig 0163(match_ver=1)부터 곧 빌 트럭은 주행을 '
  '**내릴 자리**(적하=크레인·양하=블록)에서 잰다 — src_lat/src_lon 은 여전히 지금 위치라, 두 좌표로 다시 계산하면 이 값과 어긋난다. '
  'match_ver 2(mig 0170)부터 이 값(+들락날락 벌점)이 그대로 매칭 비용이다(match_ver 1 은 주행만 비용).';

-- 뜻이 바뀐 계기들(리뷰 CONSIDER) — 종전 뜻은 앞에 그대로 두고 match_ver 2 의 뜻을 덧붙인다.
COMMENT ON COLUMN stage2_solver_shadow.n_soon_ok IS
  'match_ver 1: 신뢰도 표에 자격 시각이 있는 곧 빌 트럭 수(2계층 후보가 될 수 있는 것). mig 0163. '
  'match_ver 2(mig 0170): 남은 시간(실측 보정값)을 아는 곧 빌 트럭 수 — 신뢰도 표에 갈래가 없는 트럭은 후보가 아니다.';
COMMENT ON COLUMN stage2_solver_shadow.trucks_held_n IS
  '새 규칙이었다면 이번 틱에 쓰지 않고 남겼을 트럭 수(담을 작업이 트럭보다 적을 때). '
  'match_ver 2(mig 0170): 후보 트럭(지금 빈 + 곧 빌) 수 − 고른 후보 작업 슬롯 수.';
COMMENT ON COLUMN stage2_solver_shadow.t1_unreach_n IS
  '급한 작업 슬롯 중 갈 수 있는(1,800초 안) 지금 빈 트럭이 아예 없던 수 — 순서 위반이 아니라 도달 불가(mig 0163·리뷰 반영). '
  'match_ver 2(mig 0170): 후보 작업 슬롯 중 간선(고정·1,800초 상한 통과)이 있는 후보 트럭이 아예 없던 수.';
COMMENT ON COLUMN stage2_solver_shadow.t1_cov_alone IS
  '1계층만 풀 때 트럭을 받는 급한 작업 슬롯 수(마감 이른 순 우선 덮개). mig 0163. match_ver 2(mig 0170)부터 NULL.';
COMMENT ON COLUMN stage2_solver_shadow.t1_cov IS
  '함께 풀었을 때 트럭을 받은 급한 작업 슬롯 수. ★t1_cov_alone 과 항상 같아야 한다(함께 풀기가 급한 작업을 덜 덮으면 결함). match_ver 2(mig 0170)부터 NULL.';
COMMENT ON COLUMN stage2_solver_shadow.t1_skip_n IS
  '마감이 더 이른 급한 작업이 트럭을 못 받았는데, 더 늦은 급한 작업의 트럭을 넘기면 받을 수 있었던 경우의 수. 0 이어야 한다. '
  'match_ver 2(mig 0170)부터 NULL — 마감 순 덮개로 헝가리안 입력을 정하므로 구조상 0 이다(어긋나면 tt-api 로그 경고).';
COMMENT ON COLUMN stage2_solver_shadow.optimal_n IS
  '최적 매칭이 낸 짝 수. match_ver 2(mig 0170): 곧 빌 트럭의 예약(지시 안 냄)을 포함한다 — 지시 수는 optimal_n − soon_assign_n.';
COMMENT ON COLUMN stage2_solver_shadow.optimal_cost_s IS
  '최적 매칭 짝들의 arrival_s 합(비기까지 남은 시간 + 빈 차 주행, 벌점 제외). match_ver 2(mig 0170): 예약 포함.';
COMMENT ON COLUMN stage2_solver_shadow.greedy_n IS
  '탐욕 기준선(작업 순서대로 가장 싼 트럭) 짝 수 — 계기 전용. match_ver 2(mig 0170): 정렬 키가 간선 비용(남은 시간 + 주행 + 벌점)으로 바뀌었다(1 은 주행 + 벌점).';
COMMENT ON COLUMN stage2_solver_shadow.greedy_cost_s IS
  '탐욕 기준선 짝들의 arrival_s 합 — 계기 전용. match_ver 2(mig 0170)부터 곧 빌 트럭 짝 포함.';
COMMENT ON COLUMN dispatch_compare_shadow.reco_tier IS
  'T1(TOS 배차 순간)에 유효했던 실제 추천(reco_ytno)의 stage2_match_shadow.match_tier. '
  '⚠이 표에는 match_ver 가 없다 — 2026-10-15 mig 0170 배포 경계 전(1 = 내보내는 짝·2 = 마감 전 계획)과 '
  '후(1 = 지금 빈 트럭 지시·2 = 곧 빌 트럭 예약)를 ts 로 가를 것.';
COMMENT ON COLUMN stage2_solver_shadow.soon_unknown_n IS
  'match_ver 2(mig 0170): 남은 시간(실측 보정값)을 몰라 후보에서 뺀 곧 빌 트럭 수 — 신뢰도 표에 갈래가 없거나(1,800초 안에 절반이 '
  '안 빈 갈래 포함) 표 조회 실패·갱신 2시간 정지. 갑자기 n_soon_ok 만큼 뛰면 표가 죽은 것이다.';
COMMENT ON MATERIALIZED VIEW learn_soon_free_reliability IS
  '곧 빌 트럭 신뢰도(mig 0163): 후보 풀 (트럭,틱) 쌍이 s초 안에 실제로 짐을 내린 비율 f<s>. 갈래 = (reason, jobtype=그 트럭의 지금 작업, '
  'pred_b=예측 남은 시간 구간 0/60/300/900/3600). n<30 갈래는 안 쓴다. '
  'match_ver 1: 매처가 2계층 후보 자격(배차 마감 전에 비었을 비율 ≥ 0.8)과 그 시각을 여기서 읽었다. '
  'match_ver 2(mig 0170): 매처가 곧 빌 트럭의 **남은 시간 = 비율이 처음 0.5 이상이 되는 s** 를 여기서 읽는다(없으면 그 트럭은 후보 아님).';
