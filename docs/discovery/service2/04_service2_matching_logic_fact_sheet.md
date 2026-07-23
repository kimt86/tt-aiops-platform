# 04. Service 2 매칭 로직 사실표

## 1. 문서 정보 및 요약

| 항목 | 내용 |
|---|---|
| 문서 목적 | Westports Service 2(TT Assignment) 사전조사 산출물 — **현재 구현된 매칭 로직의 사실 기록**. 재설계안·개선안·평가는 담지 않는다. |
| 조사 대상 | 저장소 `/home/tkadmin/projects/tt-aiops-platform` (단일 Git 저장소) |
| 기준 커밋 | 브랜치 `scengen-collector`, HEAD **10cc8c0**. 이하 모든 저장소 근거는 이 커밋 기준이며 개별 항목에 커밋을 반복 표기하지 않는다. |
| 작성일 | 2026-07-22 |
| 근거 유형 | 표기 없음 = 저장소 파일 직접 확인 / **[호스트]** = 2026-07-22 조사용 호스트의 읽기 전용 관찰 / **[문서]** = kc·docs 문서의 주장 |
| 상태 표기 | **확인 / 추정 / 미확인 / 상충** |
| 주의 | 조사 시점 워킹트리에 미커밋 변경 5건(`crates/api/src/cycles.rs`, `scripts/populate_tt_cycle_recon.sql`, `web/public/livemap-roadgraph.geojson`, `web/src/CyclesPage.tsx`, `web/src/api.ts`)이 있었다. 본 문서가 인용한 매칭 코드(`crates/api/src/livemap.rs`, `crates/api/src/workpool.rs`)는 포함되지 않는다. **확인** |

**요약.** 현재 TT×작업 매칭은 **2단계 구조**다. Stage 1은 위치를 보지 않고 "무엇이 급한가"만 결정한다 — TOS Oracle 스냅샷(`live_candidate`/`live_workqueue`)에서 미배차 수요를 버킷으로 만들고, 선박 출항예정(ESTDEP)·작업완료예정(ESTWKC) 역산과 학습된 QC move-time으로 버킷별 work-ETA(마감)를 산출한 뒤 "굶주린 QC 우선 → 마감 임박순"으로 정렬하고 크레인별 수요 상한을 씌워 작업풀을 후보 트럭 수 수준으로 절단한다. Stage 2는 위치만 보는 **순수 공차이동 효율 매칭**이다 — 엣지 비용 = (트럭이 빌 때까지 잔여초) + (픽업지까지 공차이동 p50초) + 전환 페널티이며, 자체 구현 min-cost max-flow(SPFA)로 트럭→버킷 배정을 푼다. 긴급도·기아·부하분산은 비용항이 아니라 전부 Stage 1에서 처리된다(`crates/api/src/livemap.rs:3823-3825`). 결과는 Postgres `stage2_match_shadow`/`stage2_solver_shadow`에만 기록되고 자체 대시보드에 표시되며, TOS로 배차를 되돌려 쓰는 코드 경로는 저장소에 없다. **확인**

---

## 2. 실행 트리거·주기

매칭 본체는 systemd 타이머가 아니라 **wp-api 프로세스 안의 tokio 백그라운드 태스크**다. 즉 매칭의 가동 여부는 그 프로세스의 수명에 묶여 있다.

| 태스크/유닛 | 주기 | 하는 일 | 게이트 조건 | 상태 | 근거 |
|---|---|---|---|---|---|
| `spawn_stage2_shadow` | 60초 | Stage 1+2 매칭 본체, `stage2_match_shadow`·`stage2_solver_shadow` 적재 | `lm.connected`(GPS 웹소켓 연결)가 false면 그 틱 `continue` | 확인 | `crates/api/src/livemap.rs:4172-4182`, 등록 `crates/api/src/main.rs:132` |
| `spawn_dispatch_compare` | 60초 | TOS가 방금 배차한 작업 1건 단위로 "우리 픽" 재계산 → `dispatch_compare_shadow` | 동일 `lm.connected` 게이트 | 확인 | `crates/api/src/livemap.rs:4497-4505`, INSERT `:4608`, 등록 `crates/api/src/main.rs:138` |
| `spawn_fair_compare` | 5분(300초) | 최근 15분 TOS 배차결정을 60초 버킷으로 묶어 1:1 완전매칭 비교 → `fair_compare_shadow`·`fair_compare_detail` | 동일 `lm.connected` 게이트 | 확인 | `crates/api/src/livemap.rs:4841-4850`, INSERT `:4983`·`:4996`, 등록 `crates/api/src/main.rs:139` |
| `spawn_selfcal_refresh` | 15분(900초) | 곧-유휴 학습 MV 갱신 + `SOON_IDLE_GATE_MM` 갱신 | 없음 | 확인 | `crates/api/src/livemap.rs:4129-4165`, 등록 `crates/api/src/main.rs:131` |
| `wp-workpool.timer`(extractor) | 90초 | Oracle에서 `live_workpool`/`live_candidate`/`live_workqueue`/`live_vessel_schedule`/`live_assigned_tt` 전량 교체 + ETW 게이트웨이 호출 | systemd 타이머 | 확인 | `deploy/systemd/wp-workpool.timer`, `crates/extractor/src/workpool.rs:92-170` |

보충 사실:

- 네 태스크 모두 `main.rs`에서 **조건 없이 spawn**된다 — 기능 플래그·설정 스위치가 없다. **확인** (`crates/api/src/main.rs:131-139`)
- 매칭 tick(60초)과 입력 갱신(90초)이 비동기라 **같은 작업 스냅샷으로 매칭이 여러 번 돌 수 있다**. 코드는 work-ETA를 `as_of` 스냅샷 시각에 앵커해 카운트다운 되감김만 막는다. **확인** (`crates/api/src/workpool.rs:370-373`)
- Stage 2는 GPS 연결만 게이트로 삼고 **Oracle 미러(`live_workpool`)의 신선도는 검사하지 않는다**. 프론트의 300초 FROZEN 판정은 화면 전용이다. **확인** (`crates/api/src/livemap.rs:4179-4182`, `crates/api/src/workpool.rs:183-187`, `web/src/TtPage.tsx:583`)
- 운영 활성: `wp-api.service`가 enabled + active(Restart=always, RestartSec=3) — 60초 매칭이 실제로 상시 가동 중. **확인 [호스트]**. 단 이 유닛 파일은 저장소 `deploy/systemd/`에 **없다**(형상관리 밖). **상충**

---

## 3. Stage 1 — 수요·마감 결정

Stage 1은 위치를 전혀 쓰지 않는다. "어떤 작업을 몇 대분 매칭 대상으로 올릴 것인가"만 정한다.

### 3.1 입력

| 입력 | 출처 | 갱신 | 상태 | 근거 |
|---|---|---|---|---|
| 미배차 수요 버킷 | Oracle `TOSADM.JOB_ORDER_LIST` → `live_candidate` | 90초 | 확인 | `crates/extractor/sql/workpool.sql:15-40`, `crates/extractor/src/workpool.rs:300-336` |
| QC 큐 계획·진행 | Oracle `TOSADM.JOB_QUEUE_SCHEDULE` → `live_workqueue` | 90초 | 확인 | `crates/extractor/src/workpool.rs:198-220` |
| 선박 ESTDEP/ESTWKC | Oracle `TOSADM.VSB_VOYAGE` → `live_vessel_schedule` | 90초 | 확인 | `crates/api/src/workpool.rs:408-420` |
| 기아(starving) QC 집합 | Postgres `qc_wait_qc_sample`의 최근 90초 `starving_real` | 매 틱 조회 | 확인 | `crates/api/src/livemap.rs:4193-4197` |
| 학습 QC move-time / work-ETA 잔차 보정 | Postgres 학습 테이블(`learn_qc_move_time`, `learn_work_eta_bias`) | 배치 | 확인 | `crates/api/src/workpool.rs:332-348`, `db/migrations/0083_work_eta_bias.sql` |

### 3.2 work-ETA(마감) 산출 방식 — **확인**

`crates/api/src/workpool.rs:370-489`

1. 앵커는 `now`가 아니라 데이터 스냅샷 시각 `as_of`(매 폴링마다 마감이 뒤로 밀리는 것을 방지).
2. 선박별 must-finish = `min(ESTDEP − FINISH_BUFFER_S, ESTWKC)`. 단 이 터미널의 ESTWKC는 신뢰도가 낮아 **출항 0~6시간 전 범위일 때만 채택**하고 아니면 `ESTDEP − 1800초`로 폴백한다(코드 주석에 "접안 며칠 전 작업완료" 같은 불가능한 값이 다수라고 명시).
3. 큐를 `seq` 순으로 정렬하고 베이별 처리시간을 누적해 각 큐의 `work_eta_ts`를 만든다. 처리시간 상수: `DS_MOVE_S=90`, `LD_MOVE_S=110`, 베이 이동 `BAY_CHANGE_S=180`, 해치커버 `HATCH_DS_S=340`/`HATCH_LD_S=390`.
4. 트윈 비율로 컨테이너→move 환산(`move_factor = 1 − twin/2`).
5. 정적 보정: `DS_WORK_ETA_BIAS_S=600`(진행 중 작업 미모델링분), 교대·식사 정지 `SHIFT_BREAK_S=500`을 MYT 00/08/16 경계마다 1회 가산.
6. 학습 잔차 계층(`learn_work_eta_bias`, mig 0083)이 크레인·작업유형별 (실제−예측) 중앙값을 자동 흡수.

코드 주석은 **먼 미래 예측의 한계가 큐 `seq` 신뢰도에 있다**고 명시한다. **확인** (`crates/api/src/workpool.rs:381-385`)

### 3.3 정렬 규칙 — **확인**

`crates/api/src/livemap.rs:4325-4333`

```
order.sort_by_key = (starving 집합에 QC가 있으면 0, 아니면 1,  work-ETA ms)
```

즉 ①최근 90초 내 실제 굶주린 QC 우선 → ②마감(work-ETA) 빠른 순. 그 외 가중치는 없다.
starving 신호원(`qc_wait_qc_sample` 로거)이 멈추면 **조용히 순수 마감순으로만 동작한다**(오류 없음).

### 3.4 크레인별 수요 상한 — **확인**

`crates/api/src/livemap.rs:3826-3831`, `:4334-4366`

```
move_s  = (jobtype == LD) ? LD_MOVE_S(110) : DS_MOVE_S(90)
qc_cap  = max(NEED_HORIZON_S(900) / move_s, 1)      // DS = 10, LD = 8
take    = min(버킷 수요 n, 해당 QC의 남은 room)
```

- 크레인 단위 room은 그 tick 안에서 여러 버킷에 걸쳐 소진된다(같은 QC의 두 번째 버킷은 남은 room만 받는다).
- 이 상한은 **버킷 수요(cap)에 미리 반영**되므로 Stage 2 그래프에는 별도의 QC 계층이 없다.
- `DS_MOVE_S`/`LD_MOVE_S`는 하드코딩 상수이며, work-ETA 산출에 쓰이는 학습 move-time(`learn_qc_move_time`)과 **별개의 기준**이다(이중 기준). **확인**
- 트윈/탠덤 특성은 Stage 2 수요 산정에 반영되지 않는다(`Stage2Work`에 twintandem 필드 없음) → 크레인 수요 상한이 과대 산정될 수 있다. **확인** (`crates/api/src/workpool.rs:796-822`)

### 3.5 작업풀 절단 규칙 — **확인 (정확한 표현 주의)**

`crates/api/src/livemap.rs:4336-4364`

정렬된 순서로 버킷을 훑으며 `take`를 누적(`acc`)하고, **`acc >= truck_n`이면 루프를 중단**한다. 검사가 가산 **전**에 이뤄지므로:

- 보장되는 것: **유지되는 버킷의 개수 ≤ 후보 트럭 수**.
- 보장되지 않는 것: 유지된 버킷들의 **슬롯 합계(Σtake)는 트럭 수를 초과할 수 있다** — 마지막으로 담은 버킷의 `take`만큼 초과 가능.
- QC room을 이미 다 쓴 버킷(`take <= 0`)은 건너뛴다(중단이 아니라 skip).

---

## 4. Candidate Pool

### 4.1 TT 후보 — 포함/제외 상태

TT 상태는 **TOS가 아니라 GPS 웹소켓 필드만으로** `classify_tt`가 판정한다. TOS 작업풀은 실시간 판정 경로에서 사용되지 않는다(`_aj` 인자 주석). **확인** (`crates/api/src/livemap.rs:915-1024`)

| 상태 | Stage 2 후보 | time-to-free(비용 base) | 상태 | 근거 |
|---|---|---|---|---|
| `idle` | 포함 | 0초 | 확인 | `crates/api/src/livemap.rs:4241-4242` |
| `soon_idle` | 포함 | 학습 잔여초(4.2 참조) | 확인 | `:4243-4271` |
| `wait_rtg` | 포함 | 학습 잔여초 | 확인 | `:4243-4271` |
| `approaching` | 분기는 있으나 **도달 불가** | (해당 없음) | 확인 | 분기 `:4243-4257`, `classify_tt`가 이 문자열을 반환하는 경로 없음 `:915-1024` |
| 침묵 홀드 트럭(`soon_idle_held`) | 포함 | `learn_free_in_stationary` 중앙값(기본 300초, 30~3600 클램프) | 확인 | `:4222-4240` |
| `delivering` | **제외** (`_ => continue`) | — | 확인 | `:4272` |
| `empty_travel` | **제외** | — | 확인 | `:4272` |
| `staging` | **제외** | — | 확인 | `:4272` |

운반 중(delivering) 트럭이 제외되므로 **선제 예약(pre-assign)의 사정거리는 사이클 최종단계 트럭으로 한정된다**. **확인**

### 4.2 soon-idle 잔여초 판정 — **확인**

`crates/api/src/livemap.rs:4243-4271`

우선순위 체인(위에서 아래로 폴백):

1. **정차 앵커** `learn_free_in_stationary[jobtype]` — 단 `state != approaching` 이고 `speed < IDLE_SPEED_KMH(3.0)`일 때만(= 실제로 멈춰 있음). mig 0091.
2. `learn_free_in_bias[(state, jobtype, RTG거리 bin)]`
3. `learn_free_in_bias[(state, jobtype, -99)]` (거리 무관 전역)
4. 상수표 `free_in(state, jobtype)` (`crates/api/src/livemap.rs:1026-1036`)

결과는 **30~3600초로 클램프**. jobtype은 `p.jobtype` 없으면 `latched_jobtype`으로 폴백해야 학습 버킷에 맞는다(코드 주석이 명시).

DS의 soon_idle 게이트(RTG 근접거리)는 전역 `SOON_IDLE_GATE_MM`(기본 50,000mm = 50m)이며, 15분마다 학습값으로 **30~90m 범위 내 갱신**된다. **확인** (`crates/api/src/livemap.rs:769-770`, `:4156-4161`, `db/migrations/0084_soon_idle_selfcal.sql`)

학습 MV 갱신이 멈추면 오류 없이 조용히 상수표로 되돌아간다. **확인**

### 4.3 침묵 홀드 규칙 — **확인**

`crates/api/src/livemap.rs:4166-4171`, `:4222-4240`

GPS 단말이 "움직일 때만 보고"하므로 곧 빌 트럭의 약 25%가 직전에 침묵한다(코드 주석). 이를 후보로 유지하는 조건 전부:

- `age > STALE_AFTER_S`(120초)이지만 `age <= SILENT_HOLD_S`(1200초 = 20분)
- jobtype이 LD 또는 DS(latched 폴백 허용)
- 적재 상태(`container1` 또는 latched container 비어있지 않음)
- 드롭 코드(`topos1` 또는 latched)가 있고, LD는 크레인 코드·DS는 블록 코드일 것(드롭 측만)
- 마지막 위치가 그 드롭 지점 **`HELD_NEAR_DROP_M`(120m) 이내**

이때 후보 좌표는 트럭의 낡은 GPS가 아니라 **드롭 지점 좌표**로 잡는다(빔이 발생할 위치 기준).

### 4.4 Job 후보

**추출 필터(Oracle 단)** — `crates/extractor/sql/workpool.sql:33-38`. **확인**

```
WHERE JOB_ODR_COMPDATE IS NULL
  AND JOB_ODR_JOBTYPE   IN ('DS','LD')
  AND JOB_ODR_JOBSTATUS IN ('A','Q')
  AND CRE_DT >= TRUNC(SYSDATE) - 2
```

→ `JOBSTATUS` P(Planned)·B(Blocked) 행과 생성 2일 창 밖 작업은 **통째로 제외**된다. A는 진행 중(배차됨) → `live_workpool`, Q는 미배차 → `live_candidate`로 Rust에서 분리한다.

**후보 선별(추출기 단)** — `crates/extractor/src/workpool.rs:300-336`. **확인**

- `JOBSTATUS='Q'` **AND `YTNO`가 빈 값**인 행만 후보 수요로 집계(= 진짜 미배차).
- 버킷 키 = `(queuename, vessel, jobtype, src_block)`.
  - **DS**: `src_block = None` — 픽업지가 QC이므로 (큐, 선박) 단위.
  - **LD**: `src_block = block_prefix(YT_TOPOS)` — 소스 야드 블록 단위.
- **트윈 합산**: 같은 `twinkey`를 가진 두 컨테이너는 **1대 수요**로 센다(수요 단위는 컨테이너가 아니라 truck-load). `twinkey` 없는 행은 개별 계산.
- 같은 tick에 개별 미배차 컨테이너 행도 `live_workpool`에 별도 적재된다(화면의 컨테이너 단위 순번 표시용) — 여기에는 `contno`·`msnseq`가 보존된다. 단 **Stage 2 매칭 입력(`live_candidate` 버킷)에는 넘어가지 않는다.**

**Stage 2 입력 구조체** `Stage2Work` = `(qc, vessel, queuename, jobtype, src_block, n, work_eta_ts)`. 컨테이너 ID·작업지시 ID·트윈 필드 없음. **확인** (`crates/api/src/workpool.rs:773-822`)

**픽업 좌표 해석** — LD는 `src_block`의 학습 centroid, DS는 해당 QC의 작업지점(크레인 GPS→centroid 폴백). 좌표를 못 구하거나 work-ETA가 없는 버킷은 매칭 대상에서 탈락한다. **확인** (`crates/api/src/livemap.rs:4288-4300`)

### 4.5 swappable — 계산되지만 매칭에 쓰이지 않음 **확인**

재배차 가능(`swappable`) 판정은 `classify_tt`에서 **`empty_travel` 트럭에 대해서만** 계산된다(픽업지까지 잔여거리 ≥ `SWAP_MIN_M`=500m). 그런데 `empty_travel`은 Stage 2 후보 상태가 아니다. 따라서 **Stage 2 매칭 코드는 이 값을 한 번도 읽지 않는다.** 화면의 '스왑 가능' 표시와 실제 매칭 후보군이 서로 다른 집합이다. (`crates/api/src/livemap.rs:948-970`, `:4241-4272`)

### 4.6 `approaching` 죽은 분기 **확인**

문자열 `approaching`은 (a) 후보 필터 분기, (b) `free_in` 상수표, (c) `free_in_bias` 학습 버킷 키, (d) `stage2_match_shadow.veh_state` 주석에 등장하지만, **`classify_tt`가 이 값을 반환하는 코드 경로는 없다**. 실시간 경로에서는 사실상 죽은 분기이며, 관련 학습 버킷도 실시간 매칭에서 조회되지 않는다. (`crates/api/src/livemap.rs:915-1024`, `:4248-4257`, `:1030`, `db/migrations/0052_stage2_match_shadow.sql:16`)

---

## 5. Feasibility Constraint

**핵심 사실: 실행가능성(feasibility)은 후보를 걸러내는 하드 제약이 아니라 사후 라벨이다.**

| 구분 | 항목 | 성격 | 상태 | 근거 |
|---|---|---|---|---|
| 하드 제약 ① | source→트럭 엣지 용량 **1** | 해의 구조로 강제. 한 TT는 최대 1개 버킷에만 배정 | 확인 | `crates/api/src/livemap.rs:3939-3941` |
| 하드 제약 ② | 버킷→sink 엣지 용량 = **Stage-1 capped 수요(take)** | 해의 구조로 강제. 한 버킷이 받을 수 있는 트럭 수 상한(크레인 상한 내포) | 확인 | `crates/api/src/livemap.rs:3949-3953` |
| 준-제약 | `arr >= 1800초` 엣지 사전 제거 | 그래프에 아예 넣지 않음(프루닝). 결과적으로 원거리 쌍은 배정 불가 | 확인 | `crates/api/src/livemap.rs:4405-4412` |
| 사후 라벨 ① | `feasible` (boolean) | 계산만 하고 매칭은 그대로 채택·기록 | 확인 | `crates/api/src/livemap.rs:4452-4474` |
| 사후 라벨 ② | `deadline_slack_s` (음수 = 위험) | 동일 | 확인 | 동상 |

마감 판정식(**확인**, `crates/api/src/livemap.rs:4384-4386`, `:4452-4462`):

```
deadline    = max(work_eta_ms, now) + (cap_j / 2) * move_s * 1000   // 버킷 서비스 창의 중앙값
arrival_at  = now + (time-to-free + OD p90) * 1000                  // 보수적 도착
feasible    = arrival_at <= deadline
slack       = (deadline - arrival_at) / 1000
```

즉 마감을 넘긴 매칭도 `feasible=false` + 음수 slack으로 **기록되고 그대로 추천된다**. 필터·재시도·대체 후보 탐색은 없다. **확인**

**Crane Order(크레인 작업 순서)의 반영 방식** — 순서에 대한 별도 실행가능성 검사는 없다. `live_workqueue`의 `seq`와 진행률로 work-ETA를 만들고, 그 work-ETA가 위 마감식의 기준점이 되는 **간접 반영**이 전부다. 큐 `seq`가 재계획되면 work-ETA 전체가 이동한다. **확인** (`crates/api/src/workpool.rs:408-489`)

---

## 6. Cost 구성

### 6.1 엣지 비용 식 — **확인** (`crates/api/src/livemap.rs:4389-4412`)

```
arr        = time_to_free(초)  +  OD_p50(초)          // 픽업지까지의 공차 도착시간
switch_pen = 0                        (직전 tick 추천과 동일 버킷)
           | COMMIT_LOCK_S   (1200)   (다르고, 직전 추천의 work-ETA가 지금부터 10분 이내)
           | SWITCH_PENALTY_S (180)   (그 외 전환)
eff        = arr + switch_pen                          // 그래프 엣지 비용
if arr < 1800 → 엣지 추가                              // 그 외 제거
```

| 항 | 출처 | 상태 |
|---|---|---|
| `time_to_free` | idle=0 / 그 외 §4.2 학습 체인(30~3600 클램프) | 확인 |
| `OD_p50` | §6.2 비용 tier | 확인 |
| `switch_pen` | 직전 150초 내 `stage2_match_shadow` 최신행(`DISTINCT ON (ytno)`)과 버킷키 비교 | 확인 (`crates/api/src/livemap.rs:4184-4191`) |
| `p90` | 비용에는 안 들어감. 마감 판정(§5)과 `od_p90_s` 기록에만 사용 | 확인 |

### 6.2 비용 tier 3단과 좌표계 — **확인** (`crates/api/src/livemap.rs:4302-4324`, `crates/api/src/roadgraph.rs:304-420`)

| tier | 산출 | 기록값 | 조건 |
|---|---|---|---|
| **R** | 추론 도로망(`road_node`/`road_edge`) 위 **방향성 Dijkstra 경로시간** → `road_route_eval` 실측 학습곡선(DS/LD 분리)에 매핑 | `"R"` | 양 끝점이 그래프에 스냅되고 경로가 있을 때 |
| **L3** | 안벽축 회전 **맨해튼 거리**(GRID_COS/SIN = cos/sin 29.8°) → **같은 학습곡선**에 매핑 | `"L3"` | 라우팅 실패 시 |
| 상수 폴백 | `m / SEG_SPEED_MS(3.30 m/s)`, p90 = p50×1.5 | `"L3"`(구분되지 않음) | 학습곡선 자체가 아직 없을 때 |

- 좌표계: 입력·저장·API·프론트 전부 **WGS84 lat/lon**. 미터 환산은 맨해튼 계산 내부에서만 수행(`quay_manhattan_m`, 111,320 m/도 + 위도 코사인). **확인**
- 225m 격자(L2)는 mig 0082에서 **폐기**됨. 다만 `db/migrations/0052_stage2_match_shadow.sql:19`의 `cost_tier` 주석은 여전히 "L2 (225m grid) | L3 (haversine fallback)"으로 남아 있어 현행 코드와 **상충**한다.
- 도로망 그래프의 재구성은 저장소의 타이머가 아니라 **호스트 crontab(매시 11분 `scripts/reinfer_roadgraph.sh`)**에 의존한다. **확인 [호스트]**

### 6.3 비용에 **들어가지 않는 것** — **확인**

`crates/api/src/livemap.rs:3823-3825` 주석이 명시: *"urgency / starvation / load-balance are NOT cost-matrix terms."*

| 항목 | 처리 위치 |
|---|---|
| 긴급도·마감 임박도 | Stage 1 정렬(§3.3) |
| QC 기아(starvation) | Stage 1 정렬 1순위 키 |
| 크레인 간 부하분산 | Stage 1 크레인별 수요 상한(§3.4) |
| **적재 후 납품 구간**(LD의 블록→QC 주행) | 비용에 없음. 코드 주석: 생산적 작업이므로 벌점을 주면 유휴 야드 트럭이 안벽으로 장거리 공차 이동하게 되어 오히려 나빠진다 (`crates/api/src/livemap.rs:4392-4395`) |
| 연료·거리·차량 상태·운전자 | 비용항 없음(검색 근거: 비용 클로저 `:4304-4324`와 엣지 조립 `:4389-4412` 전체에 해당 변수 미등장) |

---

## 7. Weight·Threshold 상수표

**전부 Rust 소스 하드코딩이다.** 설정파일·DB·환경변수로 노출된 항목은 없으며, 변경하려면 **`cargo build --release` 재빌드 후 `wp-api` 재시작**이 필요하다. **확인**

| 상수명 | 값 | 의미 | 위치 | 변경 방법 |
|---|---|---|---|---|
| `SWITCH_PENALTY_S` | 180초 | 일반 전환 페널티(GPS/OD 잡음에 의한 재배정 감쇠) | `crates/api/src/livemap.rs:3817` | 재빌드 |
| `COMMIT_WINDOW_MS` | 600,000ms(10분) | 직전 추천의 work-ETA가 이 안이면 "임박"으로 판정 | `crates/api/src/livemap.rs:3821` | 재빌드 |
| `COMMIT_LOCK_S` | 1200초 | 임박 상태에서의 전환 비용(사실상 잠금) | `crates/api/src/livemap.rs:3822` | 재빌드 |
| (프루닝 임계) | `arr < 1800`초 | 이보다 먼 쌍은 엣지 미생성 | `crates/api/src/livemap.rs:4407` | 재빌드 |
| `DS_MOVE_S` / `LD_MOVE_S` | 90 / 110초 | 컨테이너당 QC 처리시간(수요 상한·서비스 창) | `crates/api/src/livemap.rs:3829-3830` | 재빌드 |
| `NEED_HORIZON_S` | 900초 | 크레인별 수요 상한 지평 | `crates/api/src/livemap.rs:3831` | 재빌드 |
| `SILENT_HOLD_S` | 1200초 | 침묵 트럭 후보 유지 한도 | `crates/api/src/livemap.rs:4169` | 재빌드 |
| `HELD_NEAR_DROP_M` | 120m | 침묵 트럭이 드롭지 근처로 인정되는 반경 | `crates/api/src/livemap.rs:4170` | 재빌드 |
| `STALE_AFTER_S` | 120초 | GPS 노후 판정(초과 시 침묵 홀드 경로로) | `crates/api/src/livemap.rs`(전역 상수) | 재빌드 |
| `IDLE_SPEED_KMH` | 3.0 km/h | 정차 앵커 적용 기준 속도 | `crates/api/src/livemap.rs:789-800` | 재빌드 |
| `SEG_SPEED_MS` | 3.30 m/s | 학습곡선 미형성 시 상수 속도 폴백 | `crates/api/src/livemap.rs:3852` | 재빌드 |
| `GRID_COS` / `GRID_SIN` | 0.86777 / 0.49697 | 안벽축 29.8° 회전 맨해튼 | `crates/api/src/livemap.rs:3838-3839` | 재빌드 |
| `SWAP_MIN_M` | 500m | swappable 판정(§4.5, 매칭 미사용) | `crates/api/src/livemap.rs:789-800` | 재빌드 |
| `GEOFENCE_DROP_M` | 70m | 상태 판정용 지오펜스 | `crates/api/src/livemap.rs:789-800` | 재빌드 |
| `SOON_IDLE_GATE_MM` | 기본 50,000mm | DS soon_idle RTG 근접 게이트. **유일하게 런타임 학습으로 30,000~90,000 범위 갱신** | `crates/api/src/livemap.rs:769-770`, 갱신 `:4156-4161` | 학습(15분) 또는 재빌드 |
| `LEAD_DS_S` / `LEAD_LD_S` | 450 / 1180초 | 배차 리드(Stage-1 마감 계열 지표용) | `crates/api/src/workpool.rs:793-794` | 재빌드 |
| `DS_WORK_ETA_BIAS_S` | 600초 | DS work-ETA 정적 보정 | `crates/api/src/workpool.rs:386` | 재빌드(학습층이 별도 보정) |
| `SHIFT_BREAK_S` | 500초 | MYT 00/08/16 교대·식사 정지 1회분 | `crates/api/src/workpool.rs:393` | 재빌드 |
| `FINISH_BUFFER_S` | 1800초 | 출항 전 작업완료 목표 버퍼 | `crates/api/src/workpool.rs:379` | 재빌드 |
| `MAX_N` | 120 | **fair_compare 전용** 문제 크기 상한(Stage 2 본체에는 없음) | `crates/api/src/livemap.rs:4843` | 재빌드 |

---

## 8. 해 선택 알고리즘

### 8.1 구조 — **확인**

- **자체 구현 min-cost max-flow(SPFA 연속최단경로)**, 구조체 `Mcmf` (`crates/api/src/livemap.rs:3858-3928`), 배정 추출 `optimal_assign` (`:3930-3966`), 호출 `:4448`.
- **외부 솔버 크레이트 없음** — `crates/api/Cargo.toml:11-26`에 LP/MIP/그래프 솔버 의존성 없음.
- **헝가리안 알고리즘이 아니다.** 버킷 용량이 1보다 클 수 있는 **일반화 배정**이므로 유량망으로 푼다.
- 3층 그래프 (`crates/api/src/livemap.rs:3930-3953`):

| 층 | 엣지 | 용량 | 비용 |
|---|---|---|---|
| source → 트럭 | 후보 트럭 1개당 1개 | 1 | 0 |
| 트럭 → 버킷 | 프루닝 통과 쌍만 | 1 | `eff = arr + switch_pen` |
| 버킷 → sink | 유지된 버킷 1개당 1개 | Stage-1 capped 수요(`take`) | 0 |

- 크레인 계층은 없다 — 크레인 상한이 이미 버킷 용량에 반영되어 있기 때문(코드 주석 명시).
- `Mcmf::run`은 증가경로가 없어질 때까지 SPFA를 반복 → **최대유량 조건 하의 최소비용**. 즉 비용 최소화가 유량 최대화보다 우선하지 않는다.

### 8.2 프루닝 — **확인**

`arr >= 1800`초인 (트럭, 버킷) 쌍은 엣지 자체가 생성되지 않는다(`crates/api/src/livemap.rs:4405-4412`, 주석 "prune the far tail"). Stage 2 본체에는 이 외의 크기 제한이 없다(§11).

### 8.3 greedy 베이스라인의 역할 — **확인**

`crates/api/src/livemap.rs:4415-4446`

동일 tick에서 greedy(정렬된 작업 순서대로 각 버킷이 남은 트럭 중 최저비용 n대를 가져감)를 계산하지만 **추천으로 기록하지 않는다**. 용도는 `stage2_solver_shadow`의 `greedy_cost_s`·`greedy_n`·`greedy_miss` 기록과 `gap_pct` 산출뿐이다(코드 주석: "computed only to measure what we'd lose; NOT logged as the recommendation anymore"). 채택되는 추천은 항상 MCMF 해다(`db/migrations/0056_stage2_solver_miss.sql:1-3` "Phase-2 adoption").

### 8.4 tie-break 부재 — **확인**

`optimal_assign`에 명시적 tie-break 규칙이 없다. 동일 최소비용 해가 복수일 때 어떤 트럭이 선택되는지는 **엣지 삽입 순서(= `vehicles` 벡터의 HashMap 반복 순서)에 의존**한다. 결과적으로 동점 상황에서 추천이 tick마다 달라 보일 수 있다.

### 8.5 "전역 최적"의 정확한 범위 — **확인**

이 해는 다음 세 조건 **안에서의** 최적이다. 문서·대외 설명에서 무조건적 "전역 최적"으로 쓰면 과장이 된다.

1. **프루닝된 부분그래프** 위의 최적 — `arr >= 1800`초 쌍은 애초에 후보가 아니다.
2. **Stage 1에서 이미 절단된 작업풀** 위의 최적 — 유지 버킷 개수 ≤ 트럭 수, 크레인별 상한 적용 후(§3.4~3.5).
3. **최대유량 조건 하의 최소비용** — 더 적은 대수를 보내 총비용을 낮추는 해는 선택되지 않는다.

추가로 후보 자체가 GPS 기반 상태 판정으로 정해지므로, GPS 침묵·필드 결측은 곧 후보 누락이다(§4.1).

---

## 9. Assignment / Reservation / Swap

**핵심 사실: Assignment·Reservation·Swap을 관리하는 영속 상태 머신이 없다.** **확인**

| 항목 | 사실 | 근거 |
|---|---|---|
| 상태 저장 | 없음. 예약 테이블·상태 전이 테이블·잠금 레코드 없음 | 검색 근거: `stage2_match_shadow`/`stage2_solver_shadow` 외에 Stage-2가 쓰는 테이블 없음 (`crates/api/src/livemap.rs:4465-4484`) |
| 매 tick 동작 | **무상태 전량 재매칭** — 후보 차량·작업 버킷·비용을 처음부터 다시 만들어 다시 푼다 | `crates/api/src/livemap.rs:4198-4448` |
| 이전 tick 참조 | `stage2_match_shadow`에서 **직전 150초 내 트럭별 최신 1행**을 읽어 "직전 추천 버킷키"로만 사용 | `crates/api/src/livemap.rs:4184-4191` |
| 기존 배차 보호 | **소프트 비용뿐** — 전환 페널티 180초, 임박 시 커밋 잠금 1200초 | `crates/api/src/livemap.rs:4396-4406` |
| 하드 보호 | 없음. 비용 차이가 페널티보다 크면 추천은 뒤집힌다 | 동상 |
| Swap 처리 | 별도 Swap 절차 없음. 재매칭 결과가 달라지면 `switched=true` 라벨이 붙어 기록될 뿐 | `crates/api/src/livemap.rs:4457`, `:4468-4472` |
| 운영자 수락/거부 | 수집하는 코드·테이블 없음 | 검색 근거: 전 라우트 GET, 비-GET 서버 라우트는 scengen `POST /api/scenario/config` 하나 |

프로세스 재시작 내성: 직전 추천을 메모리가 아니라 DB(`ts` 기준)에서 읽으므로 재시작해도 anti-thrash는 이어진다(코드 주석 "ts-based (restart-safe)"). **확인**

---

## 10. 중복 방지

| 무엇을 | 어떻게 | 보장 수준 | 상태 | 근거 |
|---|---|---|---|---|
| 동일 TT의 중복 배정(한 트럭에 두 작업) | source→트럭 엣지 용량 = 1 | **구조적 보장**(해에서 불가능) | 확인 | `crates/api/src/livemap.rs:3939-3941` |
| 동일 버킷 초과 배정 | 버킷→sink 용량 = Stage-1 capped 수요 | **구조적 보장** | 확인 | `crates/api/src/livemap.rs:3949-3953` |
| 한 QC에 트럭 몰림 | 크레인별 room(`NEED_HORIZON_S/move_s`)을 버킷 수요에 사전 반영 | 구조적(단, 상한값 자체는 하드코딩 상수) | 확인 | `crates/api/src/livemap.rs:4334-4366` |
| 같은 tick 결과의 중복 행 | `stage2_match_shadow` PK `(ts, ytno)` + `ON CONFLICT DO NOTHING` | DB 제약 | 확인 | `db/migrations/0052_stage2_match_shadow.sql:20`, `crates/api/src/livemap.rs:4463-4467` |
| solver 요약 행 중복 | `stage2_solver_shadow` PK `ts` + `ON CONFLICT DO NOTHING` | DB 제약 | 확인 | `crates/api/src/livemap.rs:4476-4481` |
| 트윈 컨테이너 2개를 2대 수요로 세는 것 | 추출기에서 `twinkey` 중복 제거(1 truck-load) | 코드 보장 | 확인 | `crates/extractor/src/workpool.rs:311-316` |
| **tick 간 중복(직전 tick 추천의 유효성)** | 없음 — 매 tick 무상태 재매칭, 소프트 페널티만 | **보장 없음** | 확인 | §9 |
| **프로세스 다중 기동 시 중복 계산** | DB advisory lock·파일 락·leader election 전무 | **보장 없음** | 확인 | 검색 근거: 저장소 전체에 advisory lock/leader election 코드 0건 |

---

## 11. Timeout·불완전 결과 처리

**결론: Stage 2 매칭에는 최적화 타임아웃·반복 상한·부분해 처리 코드가 없다.** **확인**

- 근거(부재 확인): `spawn_stage2_shadow` 본문 전체(`crates/api/src/livemap.rs:4172-4490`)에 `timeout`/`deadline`/반복 상한 관련 호출이 없다. `Mcmf::run`(`:3873-3925`)은 증가경로가 없어질 때까지 무제한 반복한다.
- 규모를 제한하는 것은 **간접적 두 가지뿐**: `arr < 1800` 프루닝(§8.2)과 Stage-1 작업풀 절단(§3.5). **후보 차량 pool 자체에는 상한이 없다.**
- 예외적으로 `spawn_fair_compare`만 `MAX_N=120`으로 배치 크기를 제한한다(주석: "keeps the solve sub-second"). **확인** (`crates/api/src/livemap.rs:4843`)
- 부분 결과 처리: 입력이 비면(`vehicles.is_empty()`, `works.is_empty()`, `stage2_work_candidates` 실패) 그 tick을 **조용히 건너뛴다** — 경고·지표 기록 없음. **확인** (`crates/api/src/livemap.rs:4274-4300`)
- DB INSERT 실패도 `let _ = ...`로 결과를 버린다(오류 전파·재시도 없음). **확인** (`crates/api/src/livemap.rs:4463`, `:4476`)
- 태스크가 오래 걸릴 때 이를 감지·중단하는 장치는 없다. tokio interval은 지연된 tick을 즉시 이어 실행한다. **확인**

---

## 12. 출력 스키마와 실배차 연동의 결정적 제약

### 12.1 출력 스키마 — **확인**

`db/migrations/0052_stage2_match_shadow.sql:5-22` + 이후 마이그레이션의 컬럼 추가. 추천 1건(=트럭 1대)당 1행.

| 컬럼 | 의미 |
|---|---|
| `ts`, `tick` | 매칭 시각·틱 번호 (PK = `(ts, ytno)`) |
| `ytno` | 추천 차량 |
| `qc`, `vessel`, `queuename`, `jobtype`, `src_block` | 매칭된 **작업 버킷** 식별자 |
| `veh_state` | idle / soon_idle / soon_idle_held / wait_rtg |
| `arrival_s` | time-to-free + OD p50 |
| `od_p90_s` | 보수적 도착(마감 판정용) |
| `deadline_slack_s`, `feasible` | 사후 라벨(§5) |
| `cost_tier` | R / L3 |
| `switched` | 직전 tick 대비 전환 여부 |
| `dest_lat/lon`, `src_lat/lon` | 픽업지·트럭 좌표(맵 오버레이용) |

보존은 30틱(약 30분)마다 인라인 `DELETE ... < now() - interval '21 days'`. **확인** (`crates/api/src/livemap.rs:4485-4488`)

`stage2_solver_shadow`: `(ts, tick, n_trucks, n_works, greedy_n, greedy_cost_s, optimal_n, optimal_cost_s, gap_pct, greedy_miss, optimal_miss)`, 보존 21일. **확인** (`crates/api/src/livemap.rs:4475-4482`)

소비 경로: `GET /api/stage2/advisory`(최신 tick 행만), `GET /api/stage2/shadow` 등 `/api/stage2/*` 라우트 → `web/src/Stage2Page.tsx`, `web/src/TtPage.tsx`의 "우리 픽" 표시. **전부 GET·표시 전용.** **확인** (`crates/api/src/workpool.rs:935-961`, `crates/api/src/main.rs:57-64`)

### 12.2 결정적 제약 — 실배차 연동 시 이 두 가지가 먼저 걸린다

> **① `stage2_match_shadow`에는 컨테이너번호도 작업지시 ID(MSNSEQ)도 없다. ② 매칭의 최소 단위가 개별 작업지시가 아니라 (QC·선박·큐) 또는 (소스 블록) 버킷이다.**
> 따라서 **현재 산출물만으로는 TOS에 "어느 작업지시를 어느 트럭에"를 지목할 수 없다.** **확인**
> 근거: `db/migrations/0052_stage2_match_shadow.sql:5-22`(contno/msnseq 컬럼 없음), `crates/api/src/workpool.rs:796-822`(`Stage2Work`가 버킷 + 수량 `n`만 보유)

부연 사실:

- 원천 데이터에는 식별자가 존재한다 — `live_workpool`에 `contno`·`msnseq`가 미배차 컨테이너 단위로 적재된다(`crates/extractor/src/workpool.rs:326-336`). **손실 지점은 추출이 아니라 매칭 입력(`live_candidate` 버킷 집계)과 출력 스키마다.** **확인**
- 저장소 전체에 TOS(TOSADM) 대상 INSERT/UPDATE/DELETE/MERGE·PL-SQL 호출 **0건**. API 크레이트는 Oracle 접근 자체가 없다고 주석에 명시(`crates/api/src/main.rs:1-3`). **확인**
- 배차 지시용 커맨드 ID·Ack·재전송·멱등성 키 개념 없음(멱등성은 Postgres upsert 수준). **확인**
- API 계약 산출물(OpenAPI/AsyncAPI/JSON Schema) **0건**. `utoipa` 의존성은 선언만 되고 코드 사용 0건. 계약은 Rust 구조체와 `web/src` 타입에만 존재한다. **확인** (`Cargo.toml:32`)
- TOS 측 소비 인터페이스의 존재·형식·인증·승인 절차는 저장소로 확인 불가 → **Service 2 담당 PM 확인 필요**(P0-1, P0-2).

---

## 13. KPI·평가 지표

### 13.1 지표 4종

| 지표 | 산출 주체·주기 | 무엇을 재는가 | 알려진 편향·한계 | 상태 | 근거 |
|---|---|---|---|---|---|
| `stage2_solver_shadow` (`gap_pct`) | `spawn_stage2_shadow`, 60초 | 같은 tick의 greedy 대비 MCMF 최적해의 총 도착시간 차 | 비교 대상이 **우리 내부 greedy**다. TOS 대비가 아니다 | 확인 | `crates/api/src/livemap.rs:4475-4482` |
| `dispatch_compare_shadow` | `spawn_dispatch_compare`, 60초 | TOS가 방금 배차한 작업 1건에 대해, 배차 시각 T1의 위치 스냅샷으로 재구성한 "우리 최근접 가용 트럭" vs TOS 트럭의 도착시간 | **낙관 편향** — 작업별로 독립 계산이라 같은 트럭을 여러 작업에 중복 선택할 수 있다. 마이그레이션 주석이 "overstates our advantage"라고 명시 | 확인 | `crates/api/src/livemap.rs:4493-4620`, `db/migrations/0058_dispatch_compare_shadow.sql:1-4`, `db/migrations/0061_fair_compare_shadow.sql:1-4` |
| `fair_compare_shadow` / `fair_compare_detail` (`savings_pct`) | `spawn_fair_compare`, 5분 | 최근 15분 TOS 배차결정을 60초 버킷으로 묶고, 각 버킷에서 **1:1 완전매칭**(예약 준수, 트럭 중복 불가)을 풀어 TOS 대각합 대비 절감률 | 화면 자체가 "절감은 거의 전부 **원거리** 교정에서 나오고 단거리에서는 우리가 더 나쁠 수 있다"고 한계를 표시. `MAX_N=120` 배치 상한. `n < 4`면 기록 생략 | 확인 | `crates/api/src/livemap.rs:4836-5005`, `web/src/Stage2Page.tsx:90-131`, [문서] `kc/start/launch-plan.html:55` |
| `dispatch_pred_sample` | `extractor workpool` 90초 틱에서 로깅/해소 | Stage-1 **예측 정확도** — 컨테이너별 예측 work-ETA vs 실제 작업시각(`resolved_at`). 배차 마감·트럭 보유 여부·QC slack도 함께 기록 | 매칭 품질이 아니라 마감 예측 품질 지표. 보존 21일 | 확인 | `db/migrations/0046_dispatch_pred_sample.sql:1-12`, `crates/api/src/workpool.rs:626-757` |

부수 지표: `/api/health/dispatch`가 최근 배차 틱 age<120초로 up 판정하며 thrash·feasible·savings를 함께 노출한다. **확인**

### 13.2 지표 정의가 코드 내에서 둘로 갈린다 — **상충(확인된 사실)**

같은 "greedy 대비 이득"을 두 곳이 **분모를 다르게** 계산한다.

| 위치 | 식 | 노출 |
|---|---|---|
| `crates/api/src/livemap.rs:4475` | `gap_pct = 100 × (greedy_cost − optimal_cost) / **optimal_cost**` | `stage2_solver_shadow.gap_pct` 컬럼 |
| `crates/api/src/workpool.rs:921-928` | `savings_pct = 100 × Σ(greedy − optimal) / **Σgreedy**` | `/api/stage2/shadow`의 solver 블록 |

동일 데이터라도 두 값은 항상 다르다. 대외 인용 시 어느 정의인지 반드시 병기해야 한다.

### 13.3 문서 간 수치 상충 — **상충**

| 출처 | 수치 | 표현 |
|---|---|---|
| [문서] `kc/dispatch/stage2-journey.html:40` | 약 **40% 감소** | "총 도착시간" |
| [문서] `kc/data/tos-verification.html:59` | **38~43% 절감** | — |
| [문서] `kc/start/launch-plan.html:32` | **−5.1%** | "단순 그리디 대비 아낀 공차시간" |

세 페이지 모두 **정적 HTML에 하드코딩**된 값이며 지표 정의·측정창·기준선이 서로 다르다(앞 둘은 총 도착시간, 뒤는 공차시간). 여기에 §13.2의 코드 내 이원화가 겹친다. **효과 수치의 공식 정의·측정창을 하나로 확정하는 것은 Service 2 담당 PM 확인 필요**(P0-7).

또한 [문서] 일부 kc 페이지의 "급한 작업에는 가산점" 서술은 코드와 **상충**한다 — 긴급도는 비용행렬에서 명시적으로 제외되어 있다(§6.3).

---

## 14. 구현·시험·배포 상태

| 구성요소 | 구현 | 단위시험 | 통합시험 | 배포(저장소 자산) | 운영 활성 | 근거 |
|---|---|---|---|---|---|---|
| Stage 1 수요·마감(`build_workpool`/`stage2_work_candidates`) | 있음 | **없음** | 없음 | wp-api 바이너리에 포함 | 활성 | `crates/api/src/workpool.rs:370-489`, `:793-822` |
| Stage 2 매칭(`spawn_stage2_shadow`) | 있음 | **없음** | 없음 | wp-api 바이너리에 포함 | 활성(60초, GPS 연결 틱만) | `crates/api/src/livemap.rs:4172-4490` |
| MCMF 솔버(`Mcmf`/`optimal_assign`) | 있음 | **없음** | 없음 | 동상 | 활성 | `crates/api/src/livemap.rs:3858-3966` |
| 상태 분류기(`classify_tt`) | 있음 | **없음** | 없음 | 동상 | 활성 | `crates/api/src/livemap.rs:915-1024` |
| OD 비용(`roadgraph::RouteCost`) | 있음 | **없음** | 없음 | 동상. 그래프 재구성은 호스트 crontab | 활성 | `crates/api/src/roadgraph.rs:304-420` |
| 곧-유휴 자가보정(`spawn_selfcal_refresh`) | 있음 | 없음 | 없음 | 동상 | 활성(15분) | `crates/api/src/livemap.rs:4129-4165` |
| 작업풀 추출(`extractor workpool`) | 있음 | 파싱 테스트만 | Postgres 필요 테스트 3건(별건) | `deploy/systemd/wp-workpool.{service,timer}` | 활성(90초) | `crates/extractor/src/workpool.rs:366-392` |
| 그림자 KPI 3종(solver/dispatch_compare/fair_compare) | 있음 | **없음** | 없음 | wp-api 바이너리에 포함 | 활성 | `crates/api/src/main.rs:132,138,139` |
| 추천 표시 UI(`/api/stage2/*` + Stage2Page/TtPage) | 있음 | 없음 | 없음 | 정적 빌드 | 활성 | `crates/api/src/main.rs:57-64`, `web/src/Stage2Page.tsx` |
| **TOS 실배차 연동(권고 출력 채널·TOS 소비 계약·운영자 UI)** | **없음** | — | — | — | — | 검색 근거: TOSADM 대상 DML 0건, 비-GET 서버 라우트 0건(scengen 킬스위치 제외). [문서] `kc/start/launch-plan.html` A1/A2/D2 미착수 |
| `wp-api` 서비스 유닛 | 호스트에만 존재 | — | — | **저장소에 없음** | enabled+active(Restart=always/3s) **[호스트]** | `deploy/systemd/` 목록에 부재, `kc/reference/references.html:32`가 유일한 문서 언급 |
| 도로망 재추론(`reinfer_roadgraph.sh`) | 있음 | 없음 | 없음 | **스케줄 정의 저장소에 없음** | crontab 매시 11분 **[호스트]** | `scripts/reinfer_roadgraph.sh:1-6` |

**매칭 로직 단위·회귀 테스트는 0건이다.** API 크레이트의 유일한 `#[cfg(test)]` 모듈은 `crates/api/src/periods.rs:132`이고, extractor 테스트는 ETW·행 파싱뿐이다. **확인**

배포 방식: 수동 `cargo build --release` → 유닛 복사 → `systemctl --user enable`. CI/CD·컨테이너·IaC 산출물 0건, 롤백 절차 문서 없음. **확인** (`deploy/systemd/README.md:19-38`)

---

## 15. 운영 위험 요약

| # | 위험 | 발생 조건 | 관측 가능성 | 상태 | 근거 |
|---|---|---|---|---|---|
| R-01 | **낡은 작업목록으로 매칭이 계속 산출·기록됨** | `wp-workpool` 타이머 중단·Oracle 장애 | 낮음 — Stage 2에 신선도 게이트 없음, 300초 FROZEN은 프론트 전용 | 확인 | `crates/api/src/livemap.rs:4179-4182`, `web/src/TtPage.tsx:583` |
| R-02 | **GPS 피드 단절 구간에 해가 아예 산출되지 않음**(무음 정지) | `lm.connected=false` | 중간 — OutageBanner·`/api/livemap/health`로 표시되나 매칭 스킵 자체의 지표는 없음 | 확인 | `crates/api/src/livemap.rs:4179-4182` |
| R-03 | 실배차 연동 시 **마감 위반 매칭이 필터 없이 나감** | feasible=false 상황 | 사후 라벨로만 관측 | 확인 | `crates/api/src/livemap.rs:4452-4474` |
| R-04 | 학습 MV(`spawn_selfcal_refresh`·free_in) 갱신 정지 시 **조용히 상수 폴백**되어 비용이 편향됨 | 태스크 실패·DB 이슈 | 낮음 — 오류 없이 값만 바뀜 | 확인 | `crates/api/src/livemap.rs:4243-4271`, `:4129-4165` |
| R-05 | 도로망 그래프가 낡거나 비면 **모든 OD 비용이 L3/상수 폴백으로 하락** | crontab 재추론 중단(형상관리 밖) | 중간 — `cost_tier` 분포로 사후 확인 가능 | 확인 | `scripts/reinfer_roadgraph.sh:1-6`, `crates/api/src/livemap.rs:4304-4324` |
| R-06 | starving 신호원 중단 시 **긴급도 반영이 조용히 사라짐**(순수 마감순으로 동작) | `qc_wait_qc_sample` 로거 중단 | 낮음 | 확인 | `crates/api/src/livemap.rs:4193-4197` |
| R-07 | 대형 tick에서 SPFA MCMF가 길어져도 **타임아웃·중단 장치가 없음** | 후보 차량 급증(후보 pool 상한 없음) | 낮음 — 소요시간 지표 없음 | 확인 | `crates/api/src/livemap.rs:4172-4490`, `:3873-3925` |
| R-08 | 동점 해에서 **추천이 tick마다 흔들려 보일 수 있음** | 동일 최소비용 해 복수 | 낮음 | 확인 | `crates/api/src/livemap.rs:3930-3966` |
| R-09 | 트윈/탠덤 미반영으로 **크레인 수요 상한 과대 산정** 가능 | 트윈 비중이 높은 큐 | 낮음 | 확인 | `crates/api/src/workpool.rs:796-822` |
| R-10 | 추출 필터(JOBSTATUS P/B 제외, CRE_DT 2일)로 **수요가 통째로 누락**될 수 있음 | 해당 상태·기간의 작업 존재 시 | 낮음 — 누락 카운터 없음 | 확인 | `crates/extractor/sql/workpool.sql:33-38` |
| R-11 | 화면의 '스왑 가능' 트럭이 **실제 매칭 후보가 아님**(지표·후보군 불일치) | 상시 | 화면 해석 오류로 나타남 | 확인 | `crates/api/src/livemap.rs:948-970`, `:4241-4272` |
| R-12 | 매칭 엔진 호스트 유닛(`wp-api.service`)이 **형상관리 밖**에 있어 이관·재해복구 절차가 성립하지 않음 | 이관·호스트 교체 시 | — | 상충 | `deploy/systemd/` 목록, `kc/reference/references.html:32` |
| R-13 | **매칭 로직 회귀 테스트 0건** — 상수·로직 변경 시 자동 안전망 없음 | 코드 변경 시 | — | 확인 | `crates/api/src/periods.rs:132`(유일 테스트 모듈) |
| R-14 | 상수 튜닝에 **재빌드+재시작 필요**(런타임 조정 불가) | 현장 캘리브레이션 시 | — | 확인 | §7 |
| R-15 | 대시보드 API 31개 전부 GET이나 **인증·인가 계층 없음**, `CorsLayer::permissive()` | 상시 | — | 확인 | `crates/api/src/main.rs:45-75` |
| R-16 | 효과 수치가 **문서 간(40% vs −5.1%)·코드 간(gap_pct vs savings_pct)** 갈려 있어 대외 인용 시 신뢰 훼손 | 인용 시 | — | 상충 | §13.2~13.3 |
| R-17 | 프로세스 중복 기동 시 **동시 매칭을 막는 장치 없음**(advisory lock·leader election 전무) | 운영 실수·이중 배포 | 낮음 | 확인 | 검색 근거: 저장소 전체 lock/leader election 코드 0건 |

---

## 16. 본 문서 범위에서 확인되지 않은 항목

아래는 저장소·호스트 관찰로 결론 내릴 수 없어 **미확인**으로 남긴 항목이다. 전부 **Service 2 담당 PM 확인 필요**.

| # | 항목 | 왜 미확인인가 |
|---|---|---|
| U-1 | TOS 측에 배차 권고를 소비할 인터페이스(테이블/API/메시지)가 존재하는가 | 저장소에 쓰기 코드·계약 문서 0건 |
| U-2 | TOS가 요구하는 배차 대상 식별자(작업지시 ID/MSNSEQ/컨테이너번호) | §12.2 제약의 해소 조건 |
| U-3 | Stage-2 권고를 실제 배차 담당자가 참고하는 절차가 있는가(= Recommendation 운영인가, 미사용 Shadow인가) | 코드는 표시까지만 보장. 수락/거부 수집 코드 없음 |
| U-4 | `NEED_HORIZON_S`·`DS/LD_MOVE_S` 상한 상수가 현장 실측과 맞는가 | 저장소에 검증 기록 없음(work-ETA는 학습값을 쓰는 이중 기준) |
| U-5 | 효과 판정의 공식 지표 정의·측정창·기준선 | §13.2~13.3 상충 |
| U-6 | 마감 위반(feasible=false) 허용 정책 | 현재는 라벨만 남기고 채택 |
| U-7 | 동점(tie) 상황의 운영 규칙 필요 여부 | 코드에 tie-break 없음 |
| U-8 | 매칭 tick의 실제 가동률(피드 단절로 스킵된 비율) | 스킵 카운터·로그 없음 |
| U-9 | Stage-2 솔버 1회 소요시간·부하 특성 | 부하시험·벤치마크 0건, 소요시간 지표 없음 |
| U-10 | 일정·범위·비용 | 본 문서는 확정하지 않는다. [문서] 2026-06-08 고객 세션 자료의 UAT 8/3·1단계 실투입 8/17·2단계 10/30은 현재 상태(Phase 0 그림자, 출력 채널 미착수)와 상충 |
