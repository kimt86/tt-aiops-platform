# PLAN — 설계③ 풀 전환(A) + DS 시간-투-프리 원천 교체(B) + KC 문서 갱신(C)

## STATUS (2026-08-06 마감)

- **CHUNK A: 완료** (배포 11:21:55 KST). mig0132 `pool_mode` 판별자 + `STAGE2_POOL` 킬스위치
  (기본=설계③ 풀) + 매칭 소비를 `driving`으로 교체. 라이브 확인: `pool_mode=1`,
  `n_works=pool_new_n`(27/42/51), `match_rows>0`, `pool_overlap_n<n_works`(두 풀 병행 계산 유지).
  테스트 49개 통과. 이탈: `with_capacity` 힌트 1곳(무해). scengen doctest 1건 실패는 기존·범위 밖.
- **CHUNK B: 완료** (배포 11:27:04 KST). 보이는 DS 트럭도 무브로그 앵커 duration 우선,
  LD·폴백 사슬(정차앵커→fi_bias→상수)은 else 블록으로 원문 그대로 이동. 관측 로그 실측:
  후보 66대 중 8대가 앵커 적용. 이탈: 관측 카운터를 분기 내 변수로(계획이 허용한 변형).
  ⚠ B 실행자의 정식 최종 보고는 미도착 — 오케스트레이터가 diff·journalctl로 직접 검증해 마감.
- **CHUNK C: 완료.** 두 KC 문서에 전환 고지·굶주림 제외 사유·되돌리기 스위치·벽시계 재학습을
  쉬운 말로 반영. 검증: "2026-08-06" 존재, 내부 이름 노출 0줄.

**다음 액션 (수동 체크포인트 2개, 반나절 뒤)**:
1. 예측 치우침 재측정 — `pred_ver=2 AND resolved_src='qc_comp' AND logged_at > '2026-08-06 01:31:45+00'`
   로 작업별 치우침 중앙을 재고 기준선(DS +13.2 / LD +12.1분)과 비교. 질의는
   ~/.claude/notes/tt-aiops-platform.md "측정 기준선" 절.
2. 풀 전환 효과 — `pool_mode=1` 구간(02:21:55Z 이후)의 feasible/overdue/trucks_held 를
   전환 전과 같은 잣대로 비교. POOL_MARGIN_S(300초) 조정은 이 결과가 나온 뒤에만.

목적: Stage-2 그림자 매칭을 구동하는 작업 풀을 **설계③(마감 기반) 풀로 전환**한다.
지금까지 설계③ 풀은 "계산·기록만" 상태였고, 매칭은 종전 풀(TOS 미배차·크레인당 캡)이
구동했다. 사용자 결정(2026-08-06): ①굶주림 신호는 새 풀에 **넣지 않는다**(판별 정확도
한계 — 예측이 맞으면 굶는 크레인의 일은 어차피 마감이 지나 맨 앞에 온다) ②TOS/관제
소비 채널은 협의 전이므로 **구현 금지** ③나머지 진행.

전부 그림자다 — 이 변경은 추천 기록(stage2_match_shadow)만 바꾸고 현장 배차를 바꾸지
않는다. 다만 되돌릴 킬스위치(STAGE2_POOL=legacy)와 판별자(pool_mode)를 반드시 남긴다.

⚠ 이 파일의 행 번호는 커밋 `d749808` 기준이다. 편집으로 밀리므로 **인용한 코드 문자열로
찾을 것** — 행 번호는 근처를 가리키는 힌트다.

## GLOSSARY — 용어 → 이 저장소의 실물

| 용어 | 실물 |
|---|---|
| Stage-2 그림자 매처 | `crates/api/src/livemap.rs` `spawn_stage2_shadow`(4366행). 60초 틱. 결과를 `stage2_match_shadow`(매치 행)·`stage2_solver_shadow`(틱 요약)·`stage2_pool_shadow`(풀 상세)에 기록 |
| 종전 풀(레거시) | 같은 함수의 `order` 벡터 — TOS 미배차(`works_unassigned`, 4625행)만 담고, 굶주림→(출항 티어)→work-ETA로 정렬(4712~4718행), POINT 1 캡(아래)으로 절단(4730~4817행) |
| POINT 1 캡 | `take_bucket` 내부 함수(4763행) + `cap_by_oi: HashMap<usize, i64>`(4741행) — 크레인당 소화량(`NEED_HORIZON`/무브시간)으로 버킷 수요를 절단한 값 |
| 설계③ 풀(새 풀) | 4818~4892행 블록(주석 "설계 ③ 1단계 (mig 0121) — 마감 기준 풀을 **계산만** 한다"). `due` 벡터(마감 도래 슬롯) → 마감 이른 순 정렬 → 트럭 수만큼 담아 `pool_new`(현재 `Vec<usize>`). `POOL_MARGIN_S=300`(4829행)은 잠정값 — **이번에 손대지 않는다** |
| 굶주림 신호 | `starving` 셋(4405~4408행, `qc_wait_qc_sample.starving_real`). 종전 풀 정렬의 최상위 키. **새 풀에는 넣지 않는다(사용자 결정)** — 셋 자체는 레거시 경로용으로 유지 |
| 매칭 배열 | 4908~4967행: `caps`(버킷당 트럭 몫)·`deadlines`(실행가능 판정용 work-ETA ms)·`dep_slack_w`/`dep_tier_w`(로깅용)·`edges`/`matrix`. 현재 `for &oi in &order` 루프가 채운다(4917행) |
| greedy 베이스라인 | 4968~5002행 — 측정용 비교 기준(추천 아님). `order[wpos]` 인덱싱 2곳(4993행 `works[order[wpos]].0`, 5015행 `works[order[wpos]]`) |
| 최적 매칭 | `optimal_assign`(4123행) 호출부 5004행 — 이것이 추천이 되어 `stage2_match_shadow`에 INSERT(5038~5058행) |
| 킬스위치 패턴 | `NEED_HORIZON_MODE: AtomicU8`(3986행) + `spawn_stage2_shadow` 진입부의 `std::env::var("STAGE2_NEED_HORIZON")` store(4370~4377행). 새 스위치도 같은 모양으로 |
| 판별자 규율 | 같은 표/컬럼의 의미가 바뀌면 판(ver/mode) 컬럼을 두고 COMMENT로 경계를 적는다(전례: `pred_ver` mig0130, `deadline_ver` mig0122) |
| `stage2_solver_shadow` | 틱당 1행 요약. `n_works`=매칭에 공급된 버킷 수, `pool_new_n`/`pool_overlap_n`/`trucks_held_n`/`pool_overdue_n`=설계③ 풀 통계(mig 0121). INSERT는 5071~5090행(현재 26컬럼) |
| 후보 차량 duration | 4549~4587행(보이는 트럭 분기): `classify_tt` 상태가 `soon_idle`/`approaching`/`wait_rtg`면 자유까지 남은 초를 `st_free`(정차 앵커)·`fi_bias`(⑤⑥ 학습)·`free_in`(상수) 사슬로 추정. 이 값들은 GPS 라벨(33% 누락)로 학습됐다 |
| 무브로그 앵커 | `inflight: HashMap<String, i64>`(4463~4486행) — TOS 무브로그 픽업 시각 + `learn_cycle_remaining`으로 "자유까지 남은 초"를 세는 값. 현재는 **침묵(GPS 끊긴) 트럭에만** 쓴다(4523·4542행) |
| DS / LD | DS=양하(배→야드), LD=적하(야드→배). 트럭 상태의 `jobtype` 값. 실측 헤드투헤드(4448~4462행 주석): 자유까지 남은 초 추정이 **DS는 무브로그 우세(311s vs 424s), LD는 GPS 우세(240s vs 659s)** |
| KC 문서 | `kc/dispatch/dispatch-deadline.html`(배차 마감 해설, 08-04판)·`kc/dispatch/stage2-rollout.html`(단계별 적용 계획). 중고등학생 수준 쉬운 한국어, 내부 변수명 노출 금지 |
| 채점 표 | `dispatch_pred_sample` — 이번 변경과 **무관**(예측은 workpool 쪽에서 생성). 건드리지 않는다 |

## 제약 (위반 금지)

- DB `wp_tt`(127.0.0.1:5433, user wp)는 **운영 DB**. 마이그레이션은 `psql -f`로 적용, 멱등 필수.
  접속: `PGPASSWORD=wp psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -X`
- `crates/scengen/**`·`tos_etw_gateway`/`tt-etw` 관련 일절 손대지 않는다
- `web/public/livemap-roadgraph.geojson`의 기존 미커밋 변경을 건드리지 않는다(재생성 산출물)
- 커밋·푸시하지 않는다 (오케스트레이터가 리뷰 후 수행)
- 빌드: `cargo build --release -p tt-api` (바이너리 이름 `api`). 테스트: `cargo test --workspace` — 현재 49개 전부 통과가 기준
- 배포: `systemctl --user restart tt-api` 후 `systemctl --user is-active tt-api`가 `active`
- ⚠ `.claude/worktrees/` 아래는 저장소 사본 — grep 결과에서 제외할 것
- 예측 생성(workpool.rs `stage2_work_candidates`)·채점기(`spawn_dispatch_pred_logger`)는 이번 범위 밖 — 수정 금지

## CHUNK A — 매칭 풀을 설계③ 풀로 전환 (킬스위치 + 판별자 포함)

### A1. 마이그레이션 0132 — pool_mode 판별자
`ls db/migrations | tail -3`으로 0132가 비었는지 확인 후 파일 생성:
`db/migrations/0132_solver_shadow_pool_mode.sql`
```sql
-- 0132: stage2_solver_shadow 에 '어느 풀이 매칭을 구동했나' 판별자를 더한다.
-- 지금까지 n_works·greedy_*·optimal_*·매치 행은 전부 종전 풀(TOS 미배차·크레인당 캡)이
-- 구동한 매칭의 값이었다. 2026-08-06 부터 설계③ 풀(마감순·굶주림 항 없음)로 전환한다
-- (킬스위치 STAGE2_POOL=legacy). 두 모집단을 섞어 읽지 않도록 판별자를 둔다(판별자 규율).
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS pool_mode smallint;
COMMENT ON COLUMN stage2_solver_shadow.pool_mode IS
  '매칭을 구동한 풀. NULL=전환 전(종전 풀), 0=종전 풀(킬스위치), 1=설계③ 마감 풀. '
  'n_works·greedy_*·optimal_* 및 같은 ts 의 stage2_match_shadow 행의 모집단이 이 값에 따라 '
  '다르다. 집계는 반드시 이 값으로 가를 것 (mig 0132).';
COMMENT ON COLUMN stage2_solver_shadow.n_works IS
  '매칭에 실제 공급된 버킷 수(구동 풀 기준 — pool_mode 로 가를 것, mig 0132).';
```
적용: `PGPASSWORD=wp psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -X -f db/migrations/0132_solver_shadow_pool_mode.sql`
**검증**: `... -c "\d stage2_solver_shadow" | grep pool_mode` → 1줄 출력.

### A2. 킬스위치 STAGE2_POOL
파일: `crates/api/src/livemap.rs`
1. `static NEED_HORIZON_MODE: AtomicU8 = AtomicU8::new(0);`(3986행) 아래에 추가:
```rust
/// 매칭을 구동하는 풀. `STAGE2_POOL`: 미설정/그 외 = 1(설계③ 마감 풀·기본) · `legacy` = 0(종전 풀).
/// 종전 풀은 되돌리기 전용으로 남긴다 — 굶주림 정렬·크레인당 캡 로직은 그 경로에만 있다.
static POOL_MODE: AtomicU8 = AtomicU8::new(1);
```
2. `spawn_stage2_shadow` 진입부, `DEP_TIER_MODE.store(`(4378행) 블록 **아래**에 같은 모양으로:
```rust
POOL_MODE.store(
    match std::env::var("STAGE2_POOL").unwrap_or_default().as_str() {
        "legacy" => 0,
        _ => 1, // 기본 = 설계③ 마감 풀 (2026-08-06 사용자 결정)
    },
    Ordering::Relaxed,
);
```
**검증**: `cargo build --release -p tt-api` 성공.

### A3. 설계③ 풀에 버킷당 트럭 몫을 실어 나르기
파일: `crates/api/src/livemap.rs`, 4830~4892행 블록(`let (pool_new, pool_overdue_n, trucks_held_n) = {`).
지금 `kept_new: Vec<usize>`는 버킷 인덱스만 담는다 — 매칭 용량으로 쓰려면 **이 버킷에 배정된
트럭 몫(슬롯 수)**이 필요하다.
1. `let mut kept_new: Vec<usize> = Vec::new();` → `let mut kept_new: Vec<(usize, i64)> = Vec::new();`
2. 담는 루프를:
```rust
for &(oi, slots, _) in &due {
    if acc >= truck_n { break }
    let alloc = slots.min(truck_n - acc); // 마감 도래 슬롯만큼, 남은 트럭 수로 절단
    acc += alloc;
    kept_new.push((oi, alloc));
}
```
3. 이 블록 바로 아래 `kept_new` 소비처 2곳 수정:
   - `kept_slots` 계산(4873~4874행, `kept_new.iter().map(|&oi| work[works[oi].0]...`) →
     `kept_new.iter().map(|&(_, alloc)| alloc).sum()` (이제 배정 몫 그 자체가 있다)
   - `let pool_new_set: ... = pool_new.iter().copied().collect();`(4893행) →
     `pool_new.iter().map(|&(oi, _)| oi).collect();`
4. `rank_new` 구성(5094행, `pool_new.iter().enumerate().map(|(r, &oi)| ...)`) →
   `pool_new.iter().enumerate().map(|(r, &(oi, _))| (oi, r as i32))`
**검증**: `cargo build --release -p tt-api` 성공 (경고 0 신규).

### A4. 구동 풀 선택 + 매칭 루프 교체
파일: `crates/api/src/livemap.rs`.
1. 매칭 배열 구성 직전(주석 `// STAGE 2 — PURE EFFICIENCY MATCHING`, 4905행 부근)에 추가:
```rust
// ── 구동 풀 선택 (mig 0132) ──────────────────────────────────────────────
// (works 인덱스, 이 버킷에 공급할 트럭 몫). 설계③ = 마감 도래 슬롯(트럭 수 절단),
// 종전 = POINT 1 캡. 두 풀 모두 위에서 항상 계산·기록되므로 이 선택은 소비만 가른다.
let pool_mode = POOL_MODE.load(Ordering::Relaxed);
let driving: Vec<(usize, i64)> = if pool_mode == 1 {
    pool_new.clone()
} else {
    order.iter().map(|&oi| (oi, *cap_by_oi.get(&oi).unwrap_or(&0))).collect()
};
```
2. 매칭 배열 루프(4917행) `for &oi in &order {` → `for &(oi, cap_j) in &driving {` 로 바꾸고,
   루프 안의 `let cap_j = *cap_by_oi.get(&oi).unwrap_or(&0);`(4922행)를 **삭제**
   (해당 주석 "Stage-1 capped demand"는 "구동 풀이 배정한 트럭 몫"으로 갱신).
3. greedy 베이스라인의 `order[wpos]` 인덱싱 2곳 → `driving[wpos].0`:
   - 4993행 `work[works[order[wpos]].0].jobtype` → `work[works[driving[wpos].0].0].jobtype`
   - 5015행 `let (wi, wlat, wlon, _eta) = works[order[wpos]];` → `works[driving[wpos].0];`
4. greedy 루프 헤더(4974행 `for (wpos, _oi) in order.iter().enumerate()`) →
   `for (wpos, _) in driving.iter().enumerate()`
5. solver INSERT(5071~5090행):
   - 컬럼 목록 끝에 `,pool_mode` 추가, VALUES에 `$27` 추가
   - `.bind(order.len() as i32)`(5075행, n_works 자리) → `.bind(driving.len() as i32)`
   - 마지막 `.bind(pool_overdue_n)` 뒤에 `.bind(pool_mode as i16)` 추가
6. `order`는 삭제하지 않는다 — 레거시 경로(킬스위치)와 진단 로깅(`dep_urgent_slots`,
   `pool_overlap_n`, `stage2_pool_shadow`)이 계속 쓴다.
**검증**: `cargo build --release -p tt-api` 성공 + `cargo test --workspace` 49개 통과.

### A5. 배포 + 라이브 확인
```
cargo build --release -p tt-api && systemctl --user restart tt-api && sleep 5 && systemctl --user is-active tt-api
```
`active` 확인 후 **3분 이상 기다렸다가**:
```
PGPASSWORD=wp psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -X -c "
SELECT ts, pool_mode, n_trucks, n_works, pool_new_n, pool_overlap_n, trucks_held_n,
       (SELECT count(*) FROM stage2_match_shadow m WHERE m.ts = s.ts) AS match_rows
  FROM stage2_solver_shadow s ORDER BY ts DESC LIMIT 3;"
```
**기대**: 새 행들의 `pool_mode = 1`, `n_works = pool_new_n`, `match_rows > 0`,
`pool_overlap_n < n_works`(두 풀이 계속 둘 다 계산되고 있다는 뜻). 숫자를 그대로 보고서에 실을 것.
`pool_mode`가 NULL이거나 `match_rows = 0`이 계속되면 **STOP AND REPORT**.

## CHUNK B — DS 시간-투-프리 원천 교체 (보이는 트럭도 무브로그 앵커 우선)

근거는 코드 주석에 이미 있다(4555행 "RETIREMENT DECIDED, NOT YET DONE" + 4448~4462행
헤드투헤드). GPS 라벨로 학습된 duration을 무브로그 앵커로 바꾸되 **DS만** — LD는 GPS가
2.7배 우세라 그대로 둔다.

### B1. 보이는 트럭 분기에서 DS 앵커 우선
파일: `crates/api/src/livemap.rs`, 4563~4584행 — `s @ ("soon_idle" | "approaching" | "wait_rtg") => {` 팔.
현재 구조: `jt` 계산 → 정차 앵커(`st_free`) → `fi_bias` 사슬. 이를:
```rust
let jt = p.jobtype.clone().or_else(|| p.latched_jobtype.clone()).unwrap_or_default();
// DS 는 무브로그 앵커가 우세하다(실측 |err| 311s vs GPS 424s — 위 헤드투헤드 주석).
// 이 duration 들(st_free/fi_bias)은 GPS 라벨(dropped_at·33% 누락)로 학습된 값이라
// DS 에서 앵커가 있으면 앵커가 이긴다. LD 는 GPS 가 우세(240s vs 659s)라 종전 그대로.
if jt == "DS" {
    if let Some(&rem) = inflight.get(id) {
        v.push((id.clone(), p.lat, p.lon, rem.clamp(0, 3600), c.state));
        continue;
    }
}
// …이하 기존 정차 앵커 → fi_bias 사슬 그대로 (LD 전체 + 앵커 없는 DS 폴백)…
```
기존 사슬(정차 앵커·fi_bias·free_in 폴백)은 한 글자도 바꾸지 않는다.
⚠ 이 팔은 `match` 식으로 `base`를 반환하는 구조다 — `v.push + continue`가 구조상 안 맞으면
같은 의미가 되도록 `base` 계산 앞에 DS 앵커 분기를 두는 형태로 옮겨도 된다(의미 보존이 기준).
**검증**: `cargo build --release -p tt-api` 성공.

### B2. 관측 카운터
같은 파일, 후보 트럭 벡터 구성이 끝난 뒤(`if vehicles.is_empty()`(4593행) 직전)에:
```rust
if tick % 10 == 0 {
    let n_ds_anchor = vehicles.iter()
        .filter(|v| matches!(v.4, "soon_idle" | "approaching" | "wait_rtg") && inflight.contains_key(&v.0))
        .count();
    tracing::info!(n = n_ds_anchor, total = vehicles.len(), "DS 보이는 트럭 앵커 duration 적용");
}
```
(정밀 카운트가 아니어도 된다 — 앵커가 실제로 쓰이고 있다는 생존 신호가 목적이다.
B1에서 별도 카운터 변수를 쓰는 쪽이 자연스러우면 그렇게 해도 된다.)
**검증**: `cargo build --release -p tt-api` 성공 + `cargo test --workspace` 49개 통과.

### B3. 배포 + 확인
A5와 같은 배포 명령. `active` 확인 후 **10분 이상 기다렸다가**:
```
journalctl --user -u tt-api --since "-12 min" | grep "앵커 duration" | tail -3
```
**기대**: 로그 줄이 나온다. `n` 값을 그대로 보고서에 실을 것(0이어도 실패 아님 — 한산한
시간대일 수 있다. 줄 자체가 안 나오면 STOP AND REPORT).

## CHUNK C — KC 문서 갱신 (쉬운 한국어·내부 변수명 금지)

### C1. `kc/dispatch/dispatch-deadline.html`
"5. ③ 어떤 일을 후보로 담나" 절과 "8. 지금 얼마나 맞나 — 그리고 남은 문제" 절을 갱신:
- ③이 2026-08-06부터 **실제로 추천을 고르는 기준**이 됐다는 것(그 전에는 계산·기록만 했다).
- 새 풀의 규칙 한 문장: "모든 작업을 배차 마감이 이른 순으로 줄 세우고, 마감이 온 것부터
  트럭 수만큼 담는다. 담을 게 트럭보다 적으면 트럭을 남겨 둔다."
- "크레인이 지금 굶는가" 신호는 새 기준에서 **뺐다**는 것과 이유(그 신호를 100% 정확히
  판별하기 어렵고, 예측이 맞으면 굶는 크레인의 일은 마감이 이미 지나 어차피 맨 앞에 온다).
- 언제든 예전 방식으로 되돌릴 수 있는 스위치가 있다는 것(스위치의 내부 이름은 쓰지 말 것).
- 무브 하나의 시간을 "실제 벽시계 리듬"(대기·정지 포함)으로 재학습해 쓰게 됐다는 것(08-06).
갱신 날짜(2026-08-06)를 본문에 명시.

### C2. `kc/dispatch/stage2-rollout.html`
"지금 어디에 있나" 절 갱신: 여전히 A단계(그림자·무간섭)이며, 추천을 고르는 기준이
마감 기반으로 바뀌었다는 것. B단계(참고 표시)의 첫 도구(라이브 맵 오버레이)는 이미 있고,
TOS/관제와의 연결은 협의 전이라는 것. 갱신 날짜 명시.

### C3. 검증
```
grep -c "2026-08-06" kc/dispatch/dispatch-deadline.html kc/dispatch/stage2-rollout.html
grep -n "pool_new\|STAGE2_POOL\|kept_new\|driving\|cap_by_oi" kc/dispatch/dispatch-deadline.html kc/dispatch/stage2-rollout.html
```
**기대**: 첫 grep 두 파일 다 1 이상. 둘째 grep **출력 0줄**(내부 이름 미노출).
브라우저 렌더 확인은 불필요(정적 HTML 부분 수정).

## REJECTED APPROACHES — 막히면 이쪽으로 가지 말 것

- **굶주림 신호를 새 풀에 이식**: 사용자 결정으로 폐기(2026-08-06). 종전 풀(킬스위치 경로)에는 남는다.
- **TOS/관제 소비 채널·자기 추천 이력 테이블**: TOS 협의 전 — 구현 금지.
- **POOL_MARGIN_S(300초) 조정**: 반나절 재측정 결과가 나오기 전에는 근거가 없다.
- **NEED_HORIZON/DEP_TIER 레버 제거·A/B**: 새 풀에서는 자연히 무력하고, 레거시 경로가 킬스위치로 남는 한 코드 제거는 별도 결정.
- **`order`(레거시 풀) 계산 삭제**: 킬스위치·겹침 로깅·stage2_pool_shadow 상세가 쓴다. 삭제 금지.
- **stage2_match_shadow에 별도 pool 컬럼 추가**: 같은 ts의 solver 행 pool_mode로 가를 수 있다 — 중복 판별자 불필요.
- **LD 시간-투-프리도 무브로그로 교체**: 실측 2.7배 악화(659s vs 240s). DS만.
- **pool_new의 슬롯 마감 걸음(DS_MOVE_S=90/LD_MOVE_S=132 상수)을 벽시계 학습값으로 교체**: 상자 단위 행(n=1)에는 무영향이고 범위 밖. 이번에 손대지 않는다.

## OUT OF SCOPE (이번 실행에서 하지 않는다)

- **반나절 재측정**(예측 치우침·새 풀 효과) — 시간 의존 수동 체크포인트, 오케스트레이터 몫.
- TOS/관제 채널(설계·구현 일체), 자기 추천 이력.
- `learn_work_eta_bias`/채점기/예측 생성 경로 일체.
- scengen·ETW·`web/public/livemap-roadgraph.geojson`.
- `lead_extra_s` 등 이름 정리(DB 컬럼 연동이라 별도 건).
- 커밋·푸시(오케스트레이터).
