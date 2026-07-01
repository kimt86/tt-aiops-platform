---
title: 해측 배차 시뮬레이터 — 시나리오 구조 & 필요 요소 (기획)
description: 레이아웃·라우터가 준비된 시뮬레이터에 해측(QC↔야드, TT) 배차를 모델링하는 기획 — 에뮬레이터(선박·QC·YC·TT 장비 모듈) ↔ 정책 구조, 시나리오 구조, 동적 QC 작업지점·베이이동·해치커버, 정책 훅·보정. 지금은 해측만(게이트는 나중).
sidebar:
  order: 9
  label: 시뮬레이터 — 시나리오 구조
---

> **독자**: 시뮬레이터 구현자(다른 Claude Code/사람).
> **전제**: **레이아웃 + 라우터는 이미 준비됨.** 빠진 건 **에뮬레이터의 장비 모듈 + 시뮬레이션 시나리오**다.
> **범위(지금)**: **해측(quayside)만** — QC(안벽크레인) ↔ 야드 블록을 TT(내부 트럭)가 나르는 양하(DS)·적하(LD). **게이트(외부트럭·반출입)는 나중.**
> 모든 수치/공식/출처는 라이브 코드·DB로 검증됨(2026-07-01, 시간축 MYT=UTC+8).

---

## 0. 이 문서가 답하는 것

1. 시뮬레이터의 **아키텍처** — 에뮬레이터(장비 물리) ↔ 정책(배차) — §2
2. **장비 모듈 상세**: 선박·QC·YC·TT (베이이동·해치커버·동적 작업지점·STALL) — §3
3. **시나리오 구조** (핵심 산출물) — §4
4. 시나리오를 **실데이터/합성으로 만드는 법** — §5
5. **정책 훅·지표·보정** — §6, §7

---

## 1. "해측 작업"의 정확한 정의

해측 = QC가 선박을 작업하고, 그 컨테이너를 **내부 트럭(TT)** 이 QC ↔ 야드 블록으로 나르는 사이클. 두 종류:

```
양하 DS:  [TT 빈차]→QC 도착→QC가 배에서 내려 적재→[TT 적재]→야드 블록→YC(RTG) 적치→[TT 자유]
적하 LD:  [TT 빈차]→야드 블록 도착→YC(RTG) 적재→[TT 적재]→QC 도착→QC가 배에 적재→[TT 자유]
```
- **배차 결정 = 빈차 구간(TT→픽업)만.** 적재 구간은 작업이 정한 고정 경로(QC↔블록).
- **STALL(크레인 굶음)**: QC가 다음 컨테이너를 처리하려는데 트럭이 없으면 대기 — 해측 성능의 핵심.

| 포함(지금) | 제외(나중·게이트) |
|---|---|
| QC DS/LD, TT 내부운반, YC 야드 핸드오버, 베이이동·해치커버, STALL, 선박 마감 | 외부 도로트럭, 게이트 반입/반출, 야드 재정리, 철도 |

> 우리 배차 로직은 이미 `JOBTYPE IN ('DS','LD')` 전용(`workpool.sql`) → 범위 일치.

---

## 2. 아키텍처 — 에뮬레이터(장비) ↔ 정책(배차)

시뮬레이터 = **두 계층**. 레이아웃·라우터는 있으므로, **에뮬레이터의 장비 모듈 + 시나리오**가 채울 빈칸.

```
┌─────────────── 정책(Policy) ───────────────┐   "누구를 어디로" 결정
│  매 결정틱(60s): snapshot → 배차 주문        │   ← 우리 2단계 매처 | TOS baseline
└───────────────────┬────────────────────────┘
                    │ 주문(ytno→work)        ▲ 상태 snapshot
                    ▼                        │
┌─────────────── 에뮬레이터(장비 물리) ──────────────────┐  "주문을 물리적으로 실행"
│  Vessel ── QC ── YC(RTG) ── TT  모듈이 시각을 전진      │  ← 이벤트 생성
│  · 라우터로 이동시간, 레이아웃으로 위치                 │
│  · 이벤트: 컨테이너 ready / 트럭 free / QC STALL / 도착 │
└────────────────────────────────────────────────────────┘
          │ 종단 KPI(STALL·처리율·공차·사이클·마감)
          ▼  MetricsCollector + CalibrationHarness(§7)
```

- **에뮬레이터** = 장비가 주문을 **실행**하며 물리(이동·크레인 작업·핸드오버·타이밍)를 전개하고 이벤트를 낸다. (터미널 IT의 "장비 에뮬레이터" 개념 — TOS 없이도 장비 거동을 재현.)
- **정책** = 에뮬레이터 상태를 보고 **배차를 결정**. 우리 정책/TOS baseline 교체 가능.
- 둘의 경계가 곧 검증의 핵심: **같은 에뮬레이터(물리)에 정책만 갈아끼워** 성능을 비교.

> 이전 초안의 잘못: QC를 "cadence+큐", YC를 "flat handover"로 뭉개 **베이이동·해치커버·동적 작업지점·YC 큐**를 빠뜨림. 아래 §3가 이를 장비 모듈로 1급화.

---

## 3. 장비 에뮬레이터 상세 — 상태기계 + 이벤트

### 3.0 공통: 이벤트 구동 + 모듈 간 핸드셰이크
에뮬레이터는 **이산사건(DES)**. 각 모듈은 자기 다음 이벤트를 시각순 큐에 예약하고, 클럭은 다음 이벤트로 점프한다. **정책은 결정틱(60s)에서만** 호출되고, 그 사이 장비는 이벤트로 자율 전개한다. 모듈 결합은 **이벤트(핸드셰이크)로만**:

| 이벤트 | 생산 | 소비 | 효과 |
|---|---|---|---|
| `Dispatch(ytno, work)` | 정책(틱) | TT | 트럭 빈차이동 시작 |
| `TruckArriveQC(ytno, qc)` | TT | QC | DS=빈트럭 / LD=적재트럭 도착 → 핸드셰이크 |
| `TruckArriveBlock(ytno, blk, cont)` | TT | YC | YC 대기큐 진입 |
| `QcMoveDone(qc, cont)` | QC | TT | DS=트럭 적재완료 / LD=트럭 비움 |
| `YcServiceDone(blk, ytno)` | YC | TT | DS=트럭 비움(자유) / LD=트럭 적재완료 |
| `CraneStallStart/End(qc)` | QC | Metrics | 굶음(STALL) 누적 |
| `WorkPointMoved(qc, loc)` | QC | 좌표맵 | 동적 작업지점 갱신(§3.2) |
| `TruckFree(ytno)` | TT/YC/QC | 가용 풀 | 다음 틱 후보 복귀 |

**수요 노출**: QC는 자기 큐의 **앞쪽 horizon 미배차 컨테이너**를 "dispatchable work(=Q)"로 노출(실데이터 `live_candidate` Q-status에 대응). 정책이 트럭을 배정하면 'A', QC가 소비하면 제거.

### 3.1 선박(Vessel) 에뮬레이터
- 역할: QC가 walk할 **베이 플랜** 보유 + **출항 마감**.
- **적부구조는 데이터에 인코딩** — `live_workqueue.queuename` = `<베이><D|H>-<D|L>`: `02H-D`=베이02·**홀드**·양하, `08D-D`=베이08·**덱**·양하, `06H-L`=베이06·홀드·적하. `total_qty`=그 (베이,덱/홀드) 컨테이너 수, `seq`=QC 처리 순서.
- 상태: 베이별 잔여 컨테이너(`QcMoveDone`마다 감소). 전 베이 소진 → `VesselComplete`.
- **출항**: 선박은 estdep에 떠난다. 그 시각 미완 work는 **마감 미스**로 집계(1차: 정시 출항 가정; 후에 체류 연장=비용 모델). berth_loc → QC 안벽 기하(베이↔위치).

### 3.2 QC 에뮬레이터 — 상태기계 (★핵심)
**상태**: `IDLE → GANTRY → HATCH → AWAIT_TRUCK(STALL) → WORKING → … → DONE`. 보유: 현재 베이/덱홀드, 진행 work, 동적 작업지점.

```text
[다음 큐항목 (bay,dh,job,qty) 진입]
  bay ≠ 현재베이  → GANTRY  : clock += BAY_CHANGE_S(180); 현재베이=bay; emit WorkPointMoved(안벽위치)
  해치 전환       → HATCH   : 양하 덱→홀드(D→H) += HATCH_DS(340) | 적하 홀드→덱(H→D) += HATCH_LD(390)
  remaining = qty
[컨테이너 스텝] (remaining > 0)
  이 컨테이너를 dispatchable work로 노출(아직이면)
  필요 트럭이 작업지점에 있나?  (DS=빈트럭 도착 / LD=이 컨테이너 실은 트럭 도착)
    예  → WORKING : clock += move_s(learn_qc_move_time[qc,job,shift], 폴백 DS90/LD110); schedule QcMoveDone
    아니오 → AWAIT_TRUCK : emit CraneStallStart;  TruckArriveQC 이벤트 대기
on TruckArriveQC:  AWAIT_TRUCK였으면 emit CraneStallEnd → WORKING(clock += move_s)
on QcMoveDone(cont):
  DS: cont가 트럭 위 → 트럭 적재상태로 야드 블록 출발(TT)
  LD: cont가 배 위  → 트럭 비움 출발(TT)
  remaining -= 1   (트윈쌍이면 한 무브에 2개 → -= 2)
  remaining == 0 → 다음 큐항목 / else 컨테이너 스텝
```
- **★ 동적 작업지점**: `GANTRY`마다 갱신 = 현재 베이의 안벽 위치(`선석위치 + 베이오프셋`, 레이아웃 기하). TT의 DS 픽업/LD 드롭 끝점이 이걸 따라간다. (리플레이는 QC GPS로 검증.)
- **STALL** = `AWAIT_TRUCK` 누적시간 — 우리 정책 가치의 주 측정량. **QC 진행 = max(cadence, 트럭 공급).**
- (라이브 `workpool.rs:382-427`의 베이전환/해치 가산·move_factor와 동일.)
- **노브**: ① 에이프런 버퍼(1차 0=트럭 1:1 보수적, 옵션 k대 → STALL 완화). ② LD 적재 순서(1차 큐 엄격, 옵션 도착순 — 스토우 허용 시).

### 3.3 YC(RTG) 에뮬레이터 — 대기큐 있는 서비스
**상태**: `IDLE → SERVING → …` + 대기 트럭 **FIFO 큐**.
```text
on TruckArriveBlock(ytno, blk, cont):  큐에 추가;  IDLE이면 서비스 시작
[서비스 시작]  state=SERVING; clock += yc_service_s(블록내 갠트리 + 리프트, rtg_move_log 분포)
on YcServiceDone:
  DS: cont 적치 → 트럭 비움(자유, TruckFree)      LD: cont 트럭 적재 → QC로 출발(TT)
  큐 dequeue; 남았으면 다음 서비스 / else IDLE
```
- **큐 대기가 `K_RTG_Q(~544s)`의 큰 부분** — 블록 혼잡이 핸드오버 지연을 만든다(YC move 자체는 LD 84·DS 60s).
- 매핑 `yt_topos`(예 `04U-0809`)→블록→YC. 1차 블록당 YC 1대·서비스 분포; 옵션 2 RTG/블록·위치별 갠트리.

### 3.4 TT(트럭) 에뮬레이터 — 사이클 상태기계
**상태**: `IDLE → EMPTY_TRAVEL → AT_PICKUP → LADEN_TRAVEL → AT_DROP → (IDLE)`
```text
on Dispatch(work):  라우터(현위치→픽업) → EMPTY_TRAVEL; clock += travel; schedule TruckArrive(픽업)
on TruckArrive(픽업):
  DS: emit TruckArriveQC → QcMoveDone 대기      // QC가 STALL이었으면 즉시 해소
  LD: emit TruckArriveBlock → YcServiceDone 대기 // YC 큐 있으면 대기
on 픽업 핸드오버 done(적재): 라우터(픽업→드롭) → LADEN_TRAVEL; schedule TruckArrive(드롭)
on TruckArrive(드롭):
  DS: emit TruckArriveBlock(YC) → YcServiceDone에 자유
  LD: emit TruckArriveQC(QC)   → QcMoveDone에 자유
on free: IDLE → emit TruckFree(다음 결정틱 후보)
```
- 이동시간=라우터. 끝점: 현위치 / QC **동적** 작업지점(§3.2) / 블록(YC).
- **차량 주행속도 = GPS 실측 22.8 km/h**(움직이는 30초 구간 중앙, p90 41). ⚠ 점대점 "유효속도"(직선 ~6.9 km/h)는 **정지가 약 47% 섞이고 짧은 leg에 가중**돼 너무 느림 — 그대로 쓰면 안 됨. **순수 주행 추출 가능**: ① 모션분할(움직임≥8m/30s만) ② 이미 추적 중인 `empty_trip_m`(실경로길이)÷속도.
  - **leg 분해(`learn_leg_decomp`, 마이그0075/0076/0078)**: 빈차 leg을 30초 GPS 모션분할로 **주행(drive_s, leg의 ~53%·실주행 ≈22.8km/h) + 정지(stop_s, ~47%·대부분 도착지 최종접근/큐) + 접근(approach, 도착−GPS도착 중앙 ~67s)** 으로 쪼갠다. **핵심: `ARRIVED`가 물리 도착과 사실상 일치(중앙 −4s)** → 크레인 큰 **큐는 도착 *이후*이지 이동시간 안이 아니다**(이동 중 정지는 도착지 최종접근·자리잡기). drive_s는 큐 잡음 지배적(동일 OD 변동계수 0.758=±76%) → **어떤 거리모델도 이 바닥을 못 이긴다.**
  - **라우터에 넣는 법**: 라우터에 **주행속도 ~23 km/h**(실경로 기준)를 쓰고, **정지(큐·대기·신호)는 에뮬레이터(QC/YC 핸드오버·큐·STALL)에서 별도 모델** — 차량 속도에 넣으면 **이중계상**. 검증: 라우터(주행)+에뮬레이터(정지) 합 = 실측 leg시간·C1(TT 사이클 740s).
  - 적재/공차 분리는 `tt_cycle_v2`로(1차 단일 속도). 가속도는 30초 GPS로 추정 불가·불필요. 함대 수 `live_assigned_tt`, 적재량 1(트윈 2).
- **확률 모드**: 이동·핸드오버를 로그정규 샘플링(`mu=ln(p50), sigma=(ln(p90)−ln(p50))/1.2816`). 실측 트럭 leg p90/p50≈2.4(꼬리 두꺼움) → 결정론은 대기 과소평가.

### 3.5 워크드 예시 — DS 컨테이너 1개 (모듈 핸드셰이크 추적)
```text
t=0    [정책·결정틱] TT_42(idle, 안벽 인근) → C11 베이02 양하 work 배정 → Dispatch
t=0    [TT] EMPTY_TRAVEL: 라우터(현위치→C11 동적작업지점)=95s
t=80   [QC] C11이 다음 컨테이너 처리하려는데 트럭 없음 → AWAIT_TRUCK, CraneStallStart
t=95   [TT→QC] TruckArriveQC(TT_42,C11) → CraneStallEnd(굶음 15s) → WORKING move_s(DS)=90s
t=185  [QC] QcMoveDone → TT_42 적재 → LADEN_TRAVEL(C11→02J 블록)=210s
t=395  [TT→YC] TruckArriveBlock(02J). YC 큐 비어 즉시 SERVING=70s
t=465  [YC] YcServiceDone → TT_42 자유(TruckFree). → 다음 결정틱(t=480) 후보
지표 적립: C11 STALL +15s, TT_42 공차 95s·적재 210s·핸드오버(90+70)s.
```
LD는 대칭: 트럭이 **블록 먼저**(YC 적재) → **QC**(적재), QC STALL = 그 컨테이너 실은 트럭을 기다린 시간.

### 3.6 장비 작업시간 — 스펙 분해 vs 실측 (★ 시간 계산의 근거)
> §3.2~3.4 의사코드는 시간을 평탄 상수(`move_s`, `BAY_CHANGE_S` 180, `yc_service_s`)로 표기했지만, **실제 시간은 아래 분해모델로 계산**한다(상수는 스펙 미가용 시 폴백).

**두 방식:**
- **(A) 실측 집계** — `learn_qc_move_time` 중앙값 + 평탄 상수. 현실 보정은 자동이나 **거리·단(tier)·블록내 위치 의존을 뭉갠다.** 근거: 실측 YC 무브가 **p10 10s · p50 76s · p90 164s · max 592s**(24h, LD) — 16배 분산은 갠트리 거리·tier·재취급에서 온다. 단일 median이 이를 가린다.
- **(B) 스펙 분해** — 장비 사이클을 **물리 동작으로 분해**하고 **장비 스펙(속도)** 으로 계산. **레이아웃이 거리를 주므로** 거리/위치 의존을 정확히 반영.

> **결론(아래 ★ 검증됨): (B) 스펙 분해는 시도했으나 우리 운영로그로는 신뢰 불가 → (A) 실측 *분포*를 채택.** 아래 "분해 모델" 표는 *가능했다면* 이런 형태(거리/단 의존)였다는 참고일 뿐, **실제 채택값은 그 다음 "추정된 유효 작업시간" 표**다.

**분해 모델 (참고 — 채택 아님)**

| 시간 | 분해 = f(거리/단, 스펙) |
|---|---|
| QC 1무브 | 트롤리(배 셀→) + 권상(laden, **tier 높이**) + 트롤리(트럭레인/백리치) + 권하 + 스프레더 잠금 ≈ f(셀 위치, tier) |
| QC 갠트리(베이이동) | `(Δbay × 베이피치) / 갠트리속도 + 가감속` — **평탄 180 아님**(5베이 점프 ≫ 1베이) |
| 해치커버 | 커버 들어내기/덮기(베이당 1회) — 비교적 고정. 실측 양하 ~428·적하 ~496s로 보정 |
| YC 서비스 | 갠트리(블록내 Δbay) + 트롤리(행) + 권상(**tier**) + 스프레더 ≈ f(블록내 위치, tier) — **분산 10~590s의 원천** |
| TT 이동 | 라우터(레이아웃) — 이미 (B) |

**★ 실측에서 추정한 결과** (재현 스크립트: **`scripts/estimate_equipment_specs.sh`**). 데이터시트 없이 운영 로그로 추정했다.

> **핵심 발견 — 위치 회귀로 갠트리/권상 *속도*를 분해하는 건 신뢰 불가.** `MCH_OPERATION`에 위치 인덱스(`CRNT_PSN_IDX_NO1~3`=[베이,행,단], RTG `[69,14,1→4→5]`로 확인)가 있지만, ① 무브간 gap이 **truck 대기에 묻히고**(QC 갠트리 회귀 ≈0, 절편 120s=트럭 사이클), ② 단변량 회귀가 **교란**됨(YC 권상 회귀 **−0.68, 비물리**), ③ COMP−ST 무브시간이 권상 사이클을 못 잡는다. **→ 분해 대신 실측 유효시간 *분포*를 쓴다**(시뮬이 샘플). 정밀 분해는 PLC 모션로그 같은 별도 계측이 필요(현재 없음).

**추정된 유효 작업시간 (시뮬 입력값)**

| 장비/작업 | 유효시간 (실측) | 출처 |
|---|---|---|
| **QC 무브**(컨테이너 1개) | DS **90s**(주88·야93) · LD **121s**(주117·야125) | `learn_qc_move_time`, 58크레인 |
| **YC 서비스**(분포 샘플) | DS p10/p50/p90 **10/51/140s** · LD **10/76/164s** · 재취급RH 9/69/154 · GO 10/122/227 | `rtg_move_log` 24h |
| **해치커버**(베이당 1회) | 양하 **~428s** · 적하 **~496s** | research-log |
| **TT 주행속도** | **22.8 km/h**(GPS 실측: 움직이는 30초 구간 중앙, p90 41) — ⚠ 점대점 중앙 ~6.9는 정지 섞이고 짧은 leg 가중된 값 | `truck_pos_hist` state=empty_travel |
| **TT 정지(오버헤드)** | 빈차 leg 시간의 **~47%가 정지**(대부분 도착지 최종접근·큐) — 주행과 분리 추출됨(`learn_leg_decomp`) | `truck_pos_hist` 모션분할 |

> **★ 순수 주행 추출(정지 오버헤드 제외) — 구현 완료(배차에 적용됨).** **모션 분할**(채택): `truck_pos_hist`(state=empty_travel)의 30초 변위로 움직임(≥8m)/정지를 나눠 **주행시간만** 집계 → 매트뷰 **`learn_travel_zone225_drive`**(225m OD 격자, 움직임 구간만의 p50/p90). **배차 cost가 이미 이 순수-주행 OD 사용**(livemap.rs, L3 폴백 = `quay_manhattan_m ÷ PURE_DRIVE_SPEED_MS`, `PURE_DRIVE_SPEED_MS=6.33 m/s=22.8km/h`). Stage-2 cost = **빈차 도착시간 = `free_in`(곧빔 잔여) + TRAVEL**. **시뮬도 라우터 속도/정책 비용추정에 이 순수-주행 OD 차용** 가능. 미커버 쌍은 기하 폴백(quay_manhattan÷22.8km/h). (TT leg 시간 `tt_cycle_v2`: 공차 191s·적재 441s — 정지 포함.)
>
> ⚠ 이 travel-cost 소스는 여러 번 바뀌었다: 순수-OD(`zone225_pure`) → 실현(`zone225`) → **순수-주행(`zone225_drive`, 현재 채택)**. `learn_travel_zone225`(실현)은 여전히 갱신되지만 **cost엔 미사용**(참고용). **삭제됨(호출 시 STALE)**: `learn_travel_zone225_pure`, `learn_travel_drive_sample`, `learn_eval`.

- **베이 이동(갠트리)**: 깨끗한 분해 신호 없음 → 1차는 분포 안에 흡수. 향후 **레이아웃 거리 + TT와 동일 라우터 속도**로 별도 추정.
- **보정은 자동**: 이 값이 곧 실측이라, TOS-baseline 시뮬은 정의상 §7 C2/C5/C8을 재현(효율계수 불필요 — 명판이 아니라 관측이므로). 시뮬은 분포를 **로그정규로 샘플**(`mu=ln(p50), sigma=(ln(p90)−ln(p50))/1.2816`).

---

## 4. ★ 시나리오 구조

에뮬레이터를 초기화·구동하는 모든 것. 모듈 상태(§3) + 적부계획.

```jsonc
Scenario {
  meta: { id, start_ts, duration_s, decision_tick_s: 60, source: "replay:<win>"|"synthetic", seed },

  // 선박 — 적부구조(베이 플랜) + 선석  [출처: live_vessel_schedule, live_workqueue]
  vessels: [ {
    vessel_id, voyage, berth_id, berth_loc: LayoutLoc,   // 선석 위치 → 안벽 기하
    estwkc_ts, estdep_ts, cutoff_ts,                     // 마감(estwkc 가드, §4-주)
    bays: [ { bay, dh: "D"|"H", job: "D"|"L", qty } ]    // queuename 디코드 = 적부계획
  } ],

  // QC 모듈  [출처: live_workqueue(qc↔vessel↔seq), cranes GPS]
  cranes: [ {
    qc_id, vessel_id,
    start_loc: LayoutLoc,                  // 시작 안벽 위치(=현재 베이). 이후 동적(§3.2)
    queue: [ { bay, dh, job, qty, deadline_ts } ]        // seq 순서. work 단위
  } ],

  // YC 모듈  [출처: yt_topos→블록, rtg_move_log]
  yards: [ {
    block_id,
    loc: LayoutLoc, latlon: [lat,lon],
    dims: { rows, bays, tiers }, pitch: { row_m, bay_m }  // 블록 기하(YC 갠트리 거리)
  } ],

  // 장비 유효 작업시간 — 실측 분포(§3.6, scripts/estimate_equipment_specs.sh). 데이터시트·속도분해 불요.
  // 시뮬은 분포를 로그정규로 샘플. (분해 속도는 신뢰 불가로 미채택 — §3.6.)
  equipment_specs: {
    qc_move_s:  { DS: 90,  LD: 121 },                 // 컨테이너 1개 cadence(shift 보정 가능)
    yc_service: { DS: [10,51,140], LD: [10,76,164] }, // p10/p50/p90 → 로그정규 샘플
    hatch_s:    { ds: 428, ld: 496 },                 // 베이당 1회
    bay_change_s: 180                                  // 갠트리(분해 불가 → 상수, 향후 거리화)
  },

  // TT 함대(배차 대상)  [출처: truck_pos_hist(start_ts), classify_tt/free_in]
  fleet: [ { ytno, loc: LayoutLoc, latlon, state, free_at_offset_s } ],

  env: { travel: "router", policy_od: "router"|"learned", stochastic: bool },
  policy: { kind: "ours"|"tos_baseline", params: { SWITCH_PENALTY_S, NEED_HORIZON_S, fleet_size, ... } }
}
```

**개별 work(이동작업)는 시나리오에 정적으로 박지 않는다** — QC 모듈이 `queue`(베이 플랜)를 walk하며 **동적 생성**한다(베이이동·해치커버·STALL이 ready/작업지점을 시점마다 바꾸므로). 각 work가 생길 때:
`{ work_id, qc_id, queuename(bay+dh+job), container, twin_key, quay_loc=QC현재위치(동적), yard_loc=block, deadline_ts(정적) }`.

| 모듈 | 핵심 필드 | 출처 |
|---|---|---|
| 선박 | bays(베이·덱/홀드·수량), berth_loc, 마감 | `live_workqueue.queuename`/`total_qty`, `live_vessel_schedule` |
| QC | queue(seq), cadence, start_loc | `live_workqueue.seq`, `learn_qc_move_time`, cranes GPS |
| YC | block_loc, yc_service_s | `yt_topos`→블록, `rtg_move_log.dur_s` |
| TT | loc, state, free_at | `truck_pos_hist`(start_ts), `classify_tt`, `free_in` |

> **§4-주 마감**: `deadline = min(estdep−버퍼, estwkc[가드])`. estwkc는 대부분 쓰레기라 **출항 0~6h前 정상값일 때만**(라이브 `finish_by` 로직). 선박은 정시 출항 → deadline 정책무관 정적. ready는 §3.2 동적.

---

## 5. 시나리오 만들기 — 리플레이 vs 합성

**(A) 합성 — 1차 권장**: 선박 수·QC 수·**베이 플랜(베이수×덱/홀드 비율×수량)**·블록 분포(거리대)·트럭 수·마감여유·트윈비율 파라미터화. 부하/공급비/적부패턴을 의도대로 통제.

**(B) 리플레이**: 충실 윈도 = **최근 약 2일**(병목 GPS `truck_pos_hist` 2일). 구성: start_ts 트럭위치(`truck_pos_hist`), QC 큐·베이플랜(`live_workqueue`⨝`live_workpool`, 스냅샷 1장=사실상 "현재"), 마감(`live_vessel_schedule`), cadence(`learn_qc_move_time`). **반사실 불가** — 정책 바뀌면 궤적이 달라져 과거 GPS 재생 불가 → 초기조건+적부계획만 실데이터, 이후 전진 시뮬. TOS 실제배차(`dispatch_compare_shadow.tos_ytno`)는 baseline 교차검증용.

> `live_*`는 매 ETL 덮어쓰기(as_of_ts 1장) → 과거 큐·마감 추이 미보존. 장기 리플레이엔 append 이력테이블 적재 필요(별건).

---

## 6. 정책 훅 (코드 재사용 — 재구현 금지)

매 `decision_tick_s`(60s): `snapshot → 배차`. **에뮬레이터 상태에서 snapshot 합성** → 정책 호출 → 주문을 에뮬레이터가 실행.

**우리 정책 = 2단계 매처(`crates/api/src/livemap.rs:3457-3607` 추출):**
- 1단계(선택·위치무관): `(굶는 QC 먼저 → 마감 빠른 순)` + per-crane 수요캡 `NEED_HORIZON_S(900)/move_s`(DS10·LD8).
- 2단계(매칭·위치고려): edge=`빈차도착+anti-thrash`(SWITCH_PENALTY_S 180, 발진임박 COMMIT_LOCK_S 1200, `arr<1800`). 최적해=3층 MCMF(`optimal_assign`). 긴급도는 비용에 없음(전부 1단계).

추출 시그니처 + 이미 순수한 자산:
```rust
pub fn run_stage2(s: &Snapshot) -> Vec<(String /*ytno*/, WorkRef)>;
// 그대로 import: Mcmf(3252), optimal_assign(3324), grid225(3212), quay_manhattan_m(3240),
//               classify_tt(722), free_in(827), 상수(3217-3239)
// snapshot: vehicles, works, pickup좌표(QC 동적 작업지점·블록), od(추정), starving, prev_assign
```
공유코드는 `crates/core`로 → 라이브·시뮬 같은 경로. 함정: 1단계 후 `works[order[wpos]]` 복원, `free_in`은 DS만 grounded, `starving`·`prev_assign` 입력 필수.

**정책 비용추정 OD(`env.policy_od`)**: `"router"`(1차, 추정=실제로 배차결정 품질만 격리) | `"learned"`(**`learn_travel_zone225_drive`** grid225 p50/p90 — 라이브 배차가 쓰는 순수-주행 OD와 동일, 미커버는 quay_manhattan÷6.33; 추정오차 채널, 끝점 latlon 필요).

> **도로망 라우팅 주의**: 추론된 도로 그래프(`road_node`/`road_edge`, 마이그0077·Rust 방향 Dijkstra `roadgraph.rs`)는 **구축·검증됐으나 cost에 미연결**. 게이트 결과(585 leg): 도로경로 상관 **0.490 < 맨해튼 0.565**(도로가 더 나쁨) + leg의 **30%만 스냅**(작업지점이 도로망에서 중앙 62m 벗어남 — 도로가 블록/안벽 안까지 안 들어옴). → **cost는 순수-주행 격자 유지**(interim). 도로 그래프는 향후 OD 모델의 *경로거리 피처*로만 쓸 예정. 시뮬 `"router"`도 이 한계를 반영해 맨해튼/격자 기반이 현실적.

**TOS = 보정 baseline**(알고리즘 역공학 금지): 알려진 행동(*유휴 트럭만 배차*, 픽업 최단) 휴리스틱 → 같은 에뮬레이터에서 실측 KPI(§7) 재현하면 유효 baseline.

---

## 7. 지표 & 보정 (해측)

**종단 KPI(두 정책)**: **QC STALL(★)** · 처리율(move/hr) · 공차주행(시간/비율) · 트럭 사이클타임 · 마감준수 · 가동률 · **달성 가능분 F% = (시뮬 절감)/(fair_compare 상한 ~30%)**.

**보정(디지털 트윈) — TOS baseline 시뮬이 실측 재현 후 신뢰** (2026-06-26 야간확정, MYT):

| # | 시뮬 출력 | 실측 타깃 | 허용오차 |
|---|---|---|---|
| **C1** | TT 사이클타임 중앙값 | `raw_k_tt_cycle.med` **740s**(p25 419·p75 973) | ±15% |
| **C2** | QC move 서비스시간 | `learn_qc_move_time` DS **90**·LD **121s**(§3.6) | ±10% |
| **C3** | QC 처리율 move/hr | K_MPH **24.7~25.2** | ±10% |
| **C4** | 공차주행 비율 | K_EMPTY_R **46%**(1.24km/job) | ±3%p |
| C5 | 해치커버 작업시간 | 양하 ~428s·적하 ~496s(research-log) | ±20% |
| C6 | QC 굶음/트럭대기 | K_QC_TT_WAIT **177s**(**같은베이만**) | ±25% |
| C7 | **YC 서비스 분포**(스펙분해 검증) | `rtg_move_log` LD p10/p50/p90 = **10/76/164s**(분산 재현) | p50 ±15%·꼬리 형태 |
| C8 | 야드(YC) 핸드오버 대기 | K_RTG_Q **544s** | ±20% |

우선 **C1→C2→C3→C4**. median뿐 아니라 **분포(p25/p75)** 도. 추출기와 **같은 캡**(TT사이클[120,1200]s, RTG_Q[0,1800]s, QC move[1,300]s).

**★ 무효화 함정**: ① **K_CYCLE(~40분)는 트럭 사이클 아님**(컨테이너 생애 span) → **K_TT_CYCLE 740s** 사용. ② **K_QC_NOMOVE 1.68x 과대**(해치·베이이동 포함) → **같은베이(177s)** 만. ③ GPS `tt_cycle_v2`(~419s)=편도. ④ **K_RTG_Q(TT 야드대기) ≠ K_QC_NOMOVE(QC 트럭대기)**.

---

## 8. 단계 (모듈 → 시나리오 → 정책)

| Phase | 산출물 | 완료 기준 |
|---|---|---|
| **P0 — 장비 모듈** | 에뮬레이터: 선박(베이플랜)·QC(베이walk+해치커버+동적작업지점+STALL)·YC(블록서비스)·TT(라우터 사이클). 시나리오 스키마+합성 생성기. | 합성 시나리오 1개가 끝까지 전개(이벤트 정합) |
| **P1 — 보정** | TOS baseline + 결정론. | **C1–C4(+C5/C8) 통과** — 실측 재현 |
| **P2 — 우리 정책** | `run_stage2` 추출·연결, 종단 KPI 양 정책. | 우리 vs baseline 비교 + **F%** |
| **P3 — 확률+리플레이** | 로그정규, 2일 윈도 리플레이. | C6 + 분포 일치 |
| **P4 — 스윕+리포트** | 트럭대수·SWITCH_PENALTY·NEED_HORIZON A/B, fair_compare 윈도 백테스트(상한 vs 폐루프). | 헤드라인 + KC 문서 |

---

## 부록 A — 핵심 상수·인코딩

```
queuename = <bay><D|H>-<D|L>   // 02H-D = 베이02·홀드·양하 (parse_q, workpool.rs:367)
# QC 베이 walk 가산(workpool.rs:354-427)
BAY_CHANGE_S=180  HATCH_DS_S=340  HATCH_LD_S=390  DS_MOVE_S=90  LD_MOVE_S=110
proc = qty*(1−twin/2)*move_s + (베이바뀜?180 : 적하H→D?390 : 양하D→H?340 : 0)
# OD(policy_od="learned"일 때만) — 배차는 순수-주행 OD 사용
grid225(lat,lon)='G'||round(lat/0.00202)||'_'||round(lon/0.00202)   # ~225m
L2: learn_travel_zone225_drive[(oz,dz)](n>=10) ; L3: 안벽축 맨해튼 / PURE_DRIVE_SPEED_MS 6.33(22.8km/h), p90=p50*1.5
# (실현 learn_travel_zone225 는 정지/큐 포함이라 cost서 미사용·참고용; learn_travel_zone225_pure 삭제됨, §3.6)
# 배차(livemap.rs:3217-3239)
SWITCH_PENALTY_S=180  COMMIT_WINDOW_MS=600_000  COMMIT_LOCK_S=1200  NEED_HORIZON_S=900
```

## 부록 B — 구성/보정 쿼리 (PGPASSWORD=wp psql -h127.0.0.1 -p5433 -U wp -d wp_tt)
```sql
-- 선박 적부(베이 플랜) + QC 큐 순서
SELECT qc,vessel,queuename,disload,seq,total_qty,comp_qty FROM live_workqueue ORDER BY qc,seq;
-- work 끝점(야드 블록=yt_topos)
SELECT queuename,jobtype,yt_topos,twintandem FROM live_workpool;
-- 크레인 cadence / YC 서비스 / 트럭 사이클(C1) / 마감
SELECT qc,jobtype,shift,med_sec FROM learn_qc_move_time;
SELECT machno,jobtype,round(avg(dur_s)) FROM rtg_move_log GROUP BY 1,2;
SELECT med_sec FROM raw_k_tt_cycle ORDER BY as_of DESC LIMIT 1;            -- 740
SELECT vessel,estwkc_ts,estdep_ts,cutoff_ts FROM live_vessel_schedule;
SELECT ytno,lat,lon,state FROM truck_pos_hist WHERE ts BETWEEN $T AND $T+interval '30s';
```
