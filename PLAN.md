# PLAN — 예측 채점기 교정(A) + 무브시간 벽시계화(B)

## STATUS (2026-08-06 마감)

- **CHUNK A: 완료** (커밋 `3ff724d`). 실행자 보고: A1~A5 전부 통과, 테스트 49개 그대로.
  A5 실측: v2행 54(첫 틱)·크레인 29·거리 p10 1.5 < p50 31.1 < p90 40.6분. "이미 크레인
  지난 것" 1/54 → 전체 누적 14/1,641(0.85%) — 수집 지연 경쟁이며 채점기가 comp_ts≥logged_at만
  짝지으므로 qc_comp 점수는 오염 안 됨(pool로 닫힘). 이탈: A5를 10분 대신 3.5분 시점에
  실행(오케스트레이터 조기 재개 지시 탓) — A6이 덮음.
- **A6 게이트: 통과.** 기준선(벽시계 적용 전, n=605): 치우침 DS +13.2분 / LD +12.1분.
  순번 구간별 slot 0~2 → +15.8/+21.3분, slot 17+ → +3.8/+10.0분. ⚠ 2시간 창의
  중도절단으로 높은 순번은 낙관 쪽으로 잘림 — 기울기 판정 보류.
- **CHUNK B: 완료** (같은 커밋). 실행자 보고: 매뷰 117행, DS 126초/LD 183초(기대 범위 안),
  옛값 대비 1.36~1.65배, 배포 08-06 01:31:45 UTC, 적용 직후 예측 거리 p50 31.4→37.3분
  (의도 방향). B2에서 learn_dispatch_lead 동결도 해동.
- **바뀐 가정**: A6에서 오차가 순번에 비례해 커지지 않고 맨앞이 가장 컸다(중도절단 교란
  가능). B의 근거(벽시계)는 구역 오프셋 경로로도 작동하므로 실행은 유효하나, 효과 크기
  예측은 이 표만으로 확정하지 말 것.
- **관찰**: scengen에 기존 doctest 깨짐 1건(assemble.rs:68) — 이번 변경과 무관(diff 0),
  scengen 담당 몫. stage2 쪽 벽시계 값은 클램프(30~600초) 없음 — 극단 크레인 감시.

**다음 액션 (단 하나)**: 항목 3 재측정 — B 배포 후 반나절 이상 지난 뒤
`pred_ver=2 AND resolved_src='qc_comp' AND logged_at > '2026-08-06 01:31:45+00'`
필터로 작업별 치우침 중앙을 재고, 기준선(DS +13.2 / LD +12.1분)과 비교한다.
질의는 ~/.claude/notes/tt-aiops-platform.md 의 "측정 기준선" 절에 그대로 있다.

목적: 배차 작업도달 예측이 15~30분 이르다. 원인 ①성적표를 만드는 기록기가
배선된 예측이 아니라 옛 공식을 적는다 ②무브 하나의 시간을 실제 벽시계보다
작게 배운다. A에서 ①을, B에서 ②를 고친다. A가 끝나 기준선이 나오기 전에는
B를 시작하지 않는다(오케스트레이터가 게이트).

## GLOSSARY — 용어 → 이 저장소의 실물

| 용어 | 실물 |
|---|---|
| 기록기 (legacy front-6 로거) | `crates/api/src/workpool.rs` `spawn_dispatch_pred_logger`(684행)의 블록 (3), 805~887행. `dispatch_pred_sample`에 INSERT(858~884행) |
| 채점기 (resolver) | 같은 함수의 블록 (1a) 766~785행: `resolved_at = qc_move_log.comp_ts`, `resolved_src='qc_comp'`. 블록 (1b) 789~795행: 풀 이탈 시 `'pool'`로 닫음 |
| 상자별 예측 | `Stage2Work.work_eta_ts` — `stage2_work_candidates`(workpool.rs:953)가 계산: 구역 시작 + slot_idx×move_s (1120~1140행) |
| slot_idx (계획 순번) | per-box SQL(workpool.rs:1094~1119)의 `pp` CTE — `live_stow_plan.planseq` 기반 자리 × (1−트윈/2) |
| 크레인 통과 (이미 지난 상자) | per-box SQL의 `NOT EXISTS (SELECT 1 FROM qc_move_log …comp_ts > now()-'12 hours')` — Stage2Work에는 이미 걸러져 있음 |
| 트윈 | 상자 2개=크레인 무브 1회. per-box SQL의 `GROUP BY … COALESCE(NULLIF(w.twinkey,''), w.contno)` + `tw` CTE의 (1−f/2) 환산 |
| 무브시간 (지금) | 테이블 `learn_qc_move_time`(med_sec) — extractor가 Oracle 1~300초 간격 중앙값으로 채움. 소비: workpool.rs 361~373(build_workpool), 1030~1031(stage2_work_candidates) |
| 무브시간 폴백 상수 | workpool.rs:413 `DS_MOVE_S=90.0`, 414 `LD_MOVE_S=110.0`, 1028 `BOX_MOVE_DS_S=99`, 1029 `BOX_MOVE_LD_S=126` |
| 교대정지 | workpool.rs:424 `SHIFT_BREAK_S=500` + 425~430 `shift_breaks_between`(8시간 경계마다 +500초) — 벽시계 학습과 이중계산 아님(20분 초과 간격은 학습에서 제외하므로) |
| 구역전환 | workpool.rs:415 `BAY_CHANGE_S=180.0`, 416 `HATCH_DS_S=340.0`, 417 `HATCH_LD_S=390.0` — 같은 구역 간격만 배우므로 이중계산 아님 |
| 맨앞 밀림 | 코드 아님 — 관측 현상(계획 맨앞 상자가 중앙 1무브 밀림). **이번 범위 밖** |
| 채점 표 | `dispatch_pred_sample` — 컬럼 목록은 mig0046/0048/0084/0113/0117 참조. INSERT 컬럼 16개는 workpool.rs:859~861 |
| 정답지 | `qc_move_log.comp_ts` (크레인↔트럭 인계 완료 시각). ⚠ `st_ts`는 배차시각이므로 절대 쓰지 말 것 |

## 제약 (위반 금지)

- DB `wp_tt`(127.0.0.1:5433, user wp)는 **운영 DB**. 마이그레이션은 `psql -f`로 적용, 전부 멱등. 무거운 탐색 질의 금지. 접속: `PGPASSWORD=wp psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt`
- `learn_qc_move_time` 테이블·`crates/extractor/sql/qc_move_time.sql`·`crates/scengen/**` 수정 금지 (scengen이 읽는다)
- `tos_etw_gateway` / `tt-etw` 관련 일절 손대지 않는다 (공유 인프라)
- livemap.rs의 자체 상수 `DS_MOVE_S=90`/`LD_MOVE_S=132`(3947·3951행)는 **건드리지 않는다** (다른 용도)
- 커밋·푸시하지 않는다 (오케스트레이터가 리뷰 후 수행)
- 빌드: `cargo build --release -p tt-api` (바이너리 이름 `api`). 테스트: `cargo test --workspace` — 현재 49개 전부 통과가 기준
- 배포: `systemctl --user restart tt-api` 후 `systemctl --user is-active tt-api`가 active
- ⚠ `.claude/worktrees/kc-journal/`은 저장소 사본 — grep 결과에서 제외할 것

## CHUNK A — 채점기 교정 (기록기가 배선된 예측을 그대로 적게)

### A1. 마이그레이션 0130 — 판별자·순번 컬럼
파일 생성: `db/migrations/0130_pred_sample_pred_ver.sql`
```sql
-- 0130: dispatch_pred_sample 에 예측 '공식' 판별자와 계획 순번을 더한다.
-- 기록기가 지금까지 적어온 것은 배선된 상자별 예측이 아니라 옛 공식(구역ETA+i/rem×p·ETW순
-- front-6)이었다. 이제 상자별 예측(Stage2Work.work_eta_ts)을 적는다. 두 모집단을 같은 표에서
-- 섞어 읽으면 mig0117 이 고친 것과 같은 사고가 나므로 판별자를 둔다(레거시 행은 NULL).
ALTER TABLE dispatch_pred_sample ADD COLUMN IF NOT EXISTS pred_ver smallint;
ALTER TABLE dispatch_pred_sample ADD COLUMN IF NOT EXISTS slot_idx integer;
COMMENT ON COLUMN dispatch_pred_sample.pred_ver IS
  '예측 공식 판. NULL=레거시 front-6(구역ETA+균등분배·ETW순). 2=상자별(적부계획 slot×move_s). '
  '집계는 반드시 이 값으로 가를 것. 2026-08-06 이후 새 행은 전부 2 (mig 0130).';
COMMENT ON COLUMN dispatch_pred_sample.slot_idx IS
  '기록 시점의 구역 안 순번(Stage2Work.slot_idx). 오차를 순번 수로 가르는 분석용. 레거시 NULL.';
COMMENT ON MATERIALIZED VIEW learn_work_eta_bias IS
  '⚠2026-08-06: 원천 행의 예측 공식이 바뀌었다(pred_ver 참조). 이 매뷰는 pred_ver 를 거르지
  않으므로 전환 후 7일간 두 모집단이 섞인다. 게이지 전용(되먹임 없음)이라 허용. 분석은
  pred_ver=2 로 직접 거를 것.';
```
적용: `PGPASSWORD=wp psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -X -f db/migrations/0130_pred_sample_pred_ver.sql`
**검증**: `PGPASSWORD=wp psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -X -c "\d dispatch_pred_sample" | grep -E "pred_ver|slot_idx"` → 두 줄 출력.

### A2. Stage2Work에 contno 실어 나르기
파일: `crates/api/src/workpool.rs`
1. `struct Stage2Work`(905행)에 필드 추가: `pub(crate) contno: Option<String>,` (주석: 트윈 대표 상자 = min(contno). 채점 조인 키)
2. per-box SQL(1094~1119행) 바깥 SELECT에 `l.contno` 추가, Rust 튜플에 contno 자리를 추가(위치는 실제 SELECT 순서에 맞출 것), 소비 루프(1120~1140)와 `out.push(Stage2Work{...})`(1129 부근)에 `contno: Some(contno)` 바인딩
3. 집계 경로의 push(998행 부근)에는 `contno: None`
**검증**: `cargo build --release -p tt-api` 성공 + `cargo test --workspace` 49개 통과.

### A3. 블록 (3) 교체 — front-6 로거 → 상자별 표본 기록기
파일: `crates/api/src/workpool.rs`, `spawn_dispatch_pred_logger` 안.
- **보존**: 693~804행 전부 — `present` 수집, 블록 (0) D_tos 백필, (1a) 채점, (1b) pool 닫기, (2) `open` 중복방지 셋.
- **삭제**: 805~887행(front-6 루프와 그 보조 `bay`/`seq_of`/`fronts`/`idx`).
- **신설**(같은 자리):
  1. `let cand = match stage2_work_candidates(pool.clone()).await { Ok(v) => v, Err(_) => continue };` (실패 시 이번 틱 기록만 건너뜀 — 채점 블록은 이미 위에서 돌았음)
  2. wp에서 보조 맵 구성: `contno → upd_ts`(D_tos 씨앗용) — `wp.qcs[*].moves[*]`에서 `contno`가 있고 `ytno`가 비지 않은 것의 `upd_ts`
  3. qc별로 `cand` 행을 모아 `work_eta_ts` 오름차순 정렬. `contno`가 None이거나 `work_eta_ts`가 None인 행은 제외
  4. 표본 선택(크레인당 최대 6): 정렬된 n개에서 인덱스 `{0, 1, n/2-1, n/2, n-2, n-1}` 중복 제거(n≤6이면 전부) — 앞·중간·뒤를 고루 적는 것이 목적(front만 적으면 한쪽으로 밀리는 표본이 된다)
  5. 각 표본: `open.contains(contno)`면 건너뜀. INSERT는 기존 SQL(859~861)을 바탕으로 컬럼 2개 추가:
     `..., etw_qc_ts, applied_bias_s, bias_ver, pred_ver, slot_idx) VALUES (..., $14, $15, 2, 2, $16)`
     바인딩: `pred_work_eta_ts = w.work_eta_ts`, `dispatch_deadline_ts = w.dispatch_deadline_ts`,
     `assigned = w.tos_assigned`, `slack_s = NULL`(i32 Option), `lead_s = w.dd_lead_s`(**None이면 그 행 건너뜀** — 기본값 판단 금지),
     `became_assigned_* = 기존 855~857 로직 그대로`(assigned면 now/tick/보조맵의 upd_ts, 아니면 셋 다 None), `etw_qc_ts = NULL`, `applied_bias_s = 0`, `slot_idx = w.slot_idx`
- 기존 상수 `LEAD_LD_S`/`LEAD_DS_S`가 이 블록에서만 쓰였다면 미사용 경고가 날 수 있음 — 그 경우 **STOP AND REPORT** (임의 삭제 금지; 다른 소비처가 있으면 그대로 둔다)
**검증**: `cargo build --release -p tt-api` 성공, `cargo test --workspace` 49개 통과.

### A4. 배포
```
cargo build --release -p tt-api && systemctl --user restart tt-api && sleep 5 && systemctl --user is-active tt-api
```
**검증**: `active` 출력.

### A5. 구조 검증 (배포 10분 후 실행)
```
PGPASSWORD=wp psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -X -c "
SELECT count(*) AS v2행, count(DISTINCT qc) AS 크레인,
       round((percentile_cont(0.1) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM pred_work_eta_ts-logged_at))/60)::numeric,1) AS 거리p10분,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM pred_work_eta_ts-logged_at))/60)::numeric,1) AS 거리p50분,
       round((percentile_cont(0.9) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM pred_work_eta_ts-logged_at))/60)::numeric,1) AS 거리p90분
  FROM dispatch_pred_sample WHERE pred_ver=2;" -c "
SELECT count(*) AS 이미크레인지난것
  FROM dispatch_pred_sample d
 WHERE d.pred_ver=2 AND EXISTS (SELECT 1 FROM qc_move_log m
   WHERE m.contno=d.contno AND m.jobtype=d.jobtype
     AND m.comp_ts BETWEEN d.logged_at-interval '12 hours' AND d.logged_at);"
```
**기대**: v2행 > 0, 거리 p10 < p50 < p90이고 p90이 15분 이상(앞·중간·뒤가 섞였다는 뜻), `이미크레인지난것 = 0`.
숫자를 그대로 보고서에 실을 것.

### [오케스트레이터 체크포인트] A6. 기준선 — 실행자 범위 아님
배포 2시간 후 오케스트레이터가 pred_ver=2 채점 행으로 거리별 치우침을 재고 보고한다. 이 숫자가 나온 뒤에만 청크 B를 위임한다.

## CHUNK B — 무브시간을 벽시계 리듬으로

### B1. 마이그레이션 0131 — 벽시계 리듬 매뷰
파일 생성: `db/migrations/0131_learn_qc_wall_cadence.sql`
```sql
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
```
적용: psql -f. **검증**:
```
PGPASSWORD=wp psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -X -c "
SELECT jobtype, count(*) AS 크레인, round(avg(wall_s)) AS 평균벽시계초
  FROM learn_qc_wall_cadence GROUP BY 1 ORDER BY 1;"
```
**기대**: DS 평균 110~180초, LD 평균 170~260초 범위(실측 기준 139/211 부근). 숫자를 보고서에 실을 것. 범위를 크게 벗어나면 STOP AND REPORT.

### B2. 갱신 배선 (+ 얼어 있던 learn_dispatch_lead 해동)
파일: `crates/api/src/workpool.rs`, 892~896행(`tick % 10 == 0` 블록)에 두 줄 추가:
```rust
let _ = sqlx::query("REFRESH MATERIALIZED VIEW CONCURRENTLY learn_qc_wall_cadence").execute(&pool).await;
let _ = sqlx::query("REFRESH MATERIALIZED VIEW CONCURRENTLY learn_dispatch_lead").execute(&pool).await;
```
(learn_dispatch_lead는 0116에서 만들어진 뒤 **아무도 갱신하지 않아** 준비시간이 08-03 값으로 얼어 있었다. 같은 주기로 해동한다.)
**검증**: `cargo build --release -p tt-api` 성공.

### B3. build_workpool의 move_time에 벽시계 우선 적용
파일: `crates/api/src/workpool.rs` 361~373행. med 맵 구성 **뒤에** 벽시계 오버레이:
```rust
let wall_rows: Vec<(String, String, Option<i32>)> = sqlx::query_as(
    "SELECT qc, jobtype, wall_s FROM learn_qc_wall_cadence WHERE wall_s IS NOT NULL")
    .fetch_all(&pool).await.unwrap_or_default();
```
각 (qc, jobtype)에 대해 기존 맵과 **같은 키 변환**('DS'→'D', 'LD'→'L')으로 `move_time.insert((qc, j), wall_s as f64)`. shift 구분은 벽시계 값에 없음 — 벽시계가 이기는 것이 의도.
**검증**: 빌드 성공.

### B4. stage2_work_candidates의 move_time에도 동일 오버레이
파일: `crates/api/src/workpool.rs` 1030~1031행 맵 구성 뒤, B3과 같은 오버레이(이쪽 맵은 `i64` 값형 — 캐스팅 맞출 것).
**검증**: 빌드 성공 + `cargo test --workspace` 49개 통과.

### B5. 배포
A4와 동일. **검증**: `active`.

### B6. 적용 확인 (배포 5분 후)
```
PGPASSWORD=wp psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -X -c "
SELECT w.qc, w.jobtype, m.med_sec AS 옛값, w.wall_s AS 새값,
       round(w.wall_s::numeric/NULLIF(m.med_sec,0),2) AS 비율
  FROM learn_qc_wall_cadence w LEFT JOIN learn_qc_move_time m
    ON m.qc=w.qc AND m.jobtype=w.jobtype AND m.shift='ALL'
 ORDER BY w.n DESC LIMIT 8;"
```
**기대**: 비율이 대체로 1.2~2.2. 추가로 pred_ver=2 새 행의 예측 거리 p50이 B 배포 전보다 커졌는지(무브시간이 커졌으므로):
```
PGPASSWORD=wp psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -X -c "
SELECT (logged_at > '<B배포시각 UTC>') AS B이후,
       round((percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM pred_work_eta_ts-logged_at))/60)::numeric,1) AS 거리p50분
  FROM dispatch_pred_sample WHERE pred_ver=2 GROUP BY 1 ORDER BY 1;"
```
숫자를 보고서에 실을 것.

## REJECTED APPROACHES — 막히면 이쪽으로 가지 말 것 (전부 조사에서 기각·실측)

- **전역 보정기 부활**: mig0125로 폐기. 보정 없이 201초 오차 vs 보정 시 28~46분 — 보정이 구조를 삼켰다.
- **JOB_ODR_SEQNO/MSNSEQ/POINT를 순서로 쓰기**: SEQNO는 발행시각이며 완료 시 완료시각으로 덮어써짐(사후 채점 100%는 순환). MSNSEQ 전부 빔. POINT 단독 49%=무작위.
- **ETW를 예측 목표·입력으로**: 사후 개정값. 사전 오차 ~20분.
- **tos_etw_gateway 주기·질의 변경**: 공유 인프라, 사용자 금지 지시.
- **extractor의 qc_move_time.sql(Oracle) 확장**: scengen·KPI 소비처 + Oracle 부하. 로컬 qc_move_log로 충분(이 계획의 B1).
- **learn_qc_move_time 제자리 수정**: scengen(assemble.rs:545)이 읽는다. 새 매뷰로 우회(판별자 규율).
- **맨앞 밀림 보정**: 중앙 1무브(p75 4) — 잡음 이하. 항목 3 재측정 뒤에나 재론.
- **front-6 기록 유지 + v2 병행 기록**: `open`/`(1b)`/`(0)` 중복방지·백필이 contno 단독 키라 두 모집단이 서로를 가리고 닫는다. 교체가 맞다.

## OUT OF SCOPE (이번 실행에서 하지 않는다)

- 항목 3: 하루 뒤 매칭 창 재측정 — 수동 체크포인트(오케스트레이터).
- `learn_work_eta_bias` 매뷰 재정의(pred_ver 필터) — 게이지 전용, 전환기 혼합 허용(0130 주석으로 경계 표시).
- `/api/learn/dispatch-pred`(learn.rs:564)의 pred_ver 필터 — 표시 전용, 2일 창이라 자연 수렴.
- livemap.rs의 `DS_MOVE_S`/`LD_MOVE_S` 상수(3947·3951) — 다른 용도(풀 계수).
- scengen 일체.
