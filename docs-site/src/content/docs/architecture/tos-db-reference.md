---
title: TOS DB 종합 레퍼런스
description: Westports TOS(Oracle) — 제품이 쓰는 권위적 데이터 원천의 테이블·컬럼·보존특성·코드를 총망라한 단일 진실 소스.
sidebar:
  order: 1
---

Westports **TOS(Oracle)** — 제품이 쓰는 권위적 데이터 원천의 **테이블·컬럼·보존특성·코드**를 총망라. "어디에서 무엇이 나오고, 얼마나 오래 남고, 무엇을 조심해야 하는가"의 단일 진실 소스. **실시간 websocket 데이터는 TOS가 아니며 [별도 문서](/kc/architecture/websocket-data/)로 분리**했습니다.

기준 **2026-06-09** · 원천 = **TOS Oracle**(websocket 제외) · 서빙 = PostgreSQL 3-tier · 절대제약 **prod Oracle은 추출기만 접근**

## 개요 · 데이터 아키텍처

데이터는 두 원천에서 나와 **Postgres 3-tier**로 흐릅니다. 대시보드/API는 **Postgres만** 읽고, **prod Oracle은 추출기(extractor) 크레이트만** 건드립니다(읽기전용·인덱스레인지·`FETCH FIRST`·`NO_PARALLEL`, 승인된 `remote-toolbox-sql` 경유).

```
TOS Oracle  ──(추출기: 일/교대 틱, 캡·인덱스레인지)──▶  Postgres L0 raw_*  ──▶ L1 kpi_daily ──▶ L2 baseline
TOS Oracle  ──(추출기: 작업풀 90s 틱)────────────────▶  Postgres live_workqueue/live_workpool  ※라이브 작업 풀
websocket   ──(SSH 터널 → API 인제스트, ctab/wpt_gps)──▶ 메모리(라이브맵·배차)  ※DB 미적재
대시보드/API ──읽기──▶ Postgres only          ※Oracle 직접 접근 절대 금지
```

:::note[왜 이렇게]
source Oracle은 **운영 라이브** DB라 부하를 최소화해야 합니다. 추출기는 정해진 시각(야간 01:30 MYT 권위·교대 틱 경량)에만, 날짜 인덱스로 좁혀, 출력만 `FETCH FIRST`로 잘라 가져옵니다. 한 번 가져온 날은 Postgres에 영구 누적 → 같은 과거 날짜를 다시 스캔하지 않습니다.
:::

## 두 원천 한눈에

**TOS Oracle** (`tos-db-research`): 완료된 작업의 권위적 기록. 컨테이너 이벤트·크레인 작업·항차 계획·장비 가동. 타임스탬프 **초** 단위. **보존 기간 제한**(보존 기간 섹션). 스키마 `TOSADM.*`.

**websocket** (실시간 2 zone): 장비 ~450대의 GPS/PLC(`wpt_gps`·`ctab`). **TOS가 아닌 별도 실시간 시스템** — 이 문서 범위 밖. → [websocket 데이터 & 활용](/kc/architecture/websocket-data/) 문서.

| 구분 | TOS Oracle | websocket |
|---|---|---|
| 성격 | 이벤트·결과(무슨 작업이 있었나) | 연속 관측(공간·시간에서 어떻게) |
| 시간 해상도 | 초 단위 timestamp | GPS 장비당 ~3s 불규칙 / PLC ~1s |
| 접근 | 추출기만(remote-toolbox-sql) | API 인제스트(SSH 터널 127.0.0.1:9986) |
| 저장 | Postgres raw_*/kpi_*(영구 누적) | 메모리(라이브 스냅샷) |
| 깊이 | 보존 기간만(보존 기간 섹션) | 현재 순간만 |

:::note[이 문서의 범위]
아래 모든 내용(테이블·컬럼·코드·KPI·서빙)은 **TOS Oracle** 데이터입니다. 실시간 websocket(GPS/PLC)은 출처도 시스템도 달라 [별도 문서](/kc/architecture/websocket-data/)로 분리했습니다.
:::

## 보존 기간 — 가장 중요한 특성

TOS Oracle은 **롤링(rolling) 보존**이라 오래된 날짜가 자동 삭제됩니다. 이게 "과거 데이터를 얼마나 끌어올 수 있나"를 결정합니다.

| 소스 테이블 | 보존 | 영향받는 KPI | 실증 |
|---|---|---|---|
| `JOB_ORDER_HISTORY` | 약 15일 | 공차·공차비율·사이클·핸드오버대기(K_RTG_Q) | raw_k_cycle이 정확히 ~15일 창에서 끊김 |
| `MCH_OPERATION` / `VSS_STATISTICS` | ≥ 35일 | MPH·QC대기·가동률·항차 통계 | MCH 기반 raw_*가 더 깊게 남음 |
| `YT_DIS_DT`/`ACTV_DT` (크레인대기 입력) | 오래된 날 희소 | K_RTG_Q | 오래된 날은 0행 → 0 산출 |

:::caution[실무 결론 — 깊은 백필 불가]
예: **2026년 1월 데이터(약 160일 전)는 원천에 남아있지 않아 가져올 수 없습니다.** 백필로 끌어올 수 있는 최대 깊이는 공차/사이클 **~15일**, MPH/QC대기/가동률 **~35일** 뿐. 그 이전(특히 1~4월)은 Oracle에서 이미 삭제됨. **과거 깊이는 "추출을 시작한 시점"부터 앞으로만 영구 누적**됩니다(현재 Postgres 보유 2026-05-01~). 빈 결과셋에 toolbox는 `{"result":"null"}` 반환(0행 처리).
:::

## TOS Oracle 테이블

wp-tt가 실제로 읽는 핵심 테이블들. 컬럼은 추출 SQL(`crates/extractor/sql/*.sql`)에서 실제 사용하는 것 기준. 스키마는 `TOSADM`.

### 3.1 `JOB_ORDER_HISTORY` — 컨테이너 작업 이력 (컨테이너 단위 이벤트)

한 컨테이너 작업의 생애를 이벤트(transition) 단위로 기록. **이동거리·사이클·크레인 큐 대기의 원천**. 한 작업 = (CONTNO, POINT, SEQNO)로 식별, 여러 이벤트로 구성.

| 컬럼 | 의미 · 단위 · 예 |
|---|---|
| `JOB_HIST_DATE` / `JOB_HIST_TIME` | 이벤트 날짜(YYYYMMDD, **인덱스 술어**) / 시각(HH24MISS). 교대 분류·정렬에 사용. |
| `JOB_HIST_JOBTYPE` | 작업 종류 — LD 적하 · DS 양하 · MO/MI 이적 · RH 재취급 · GI/GO 게이트 in/out 등(코드 사전 참조). |
| `JOB_HIST_CONTNO` | 컨테이너 번호(ISO, 예 `EGSU2064058`). 작업 식별 키. |
| `JOB_HIST_POINT` / `JOB_HIST_SEQNO` | 포인트 코드 / 시퀀스 번호. (CONTNO·POINT·SEQNO) = 한 작업. |
| `JOB_HIST_VESSEL` / `JOB_HIST_VOYAGE` | 선박명 / 항차. 항차별 집계·맥락. |
| `JOB_HIST_ARMGC` | 작업 크레인 ID. **주의:** probe 결과 RTG·ES·XXX(미배정)만 — **C##(안벽)은 없음**. 즉 야드측 크레인. |
| `YT_DIS_DT` → `JOB_HIST_ACTV_DT` | YT(TT) 하차 시각 → 크레인 활성 시각. 차이 = **크레인 큐 대기(K_RTG_Q)** = `(ACTV_DT − YT_DIS_DT)·86400`초. 오래된 날 희소. |
| `CRNT_PSN_IDX_NO1` | 현재 위치 인덱스 = 블록 ID. |
| `LNDN_TRV_RNG` / `UN_LNDN_TRV_RNG` | **적재거리 / 공차거리**(미터, 0~5000 필터). 공차비율 = `Σ공차 / Σ(적재+공차)`. |

:::note
로드 **MEDIUM**(~240K행 인덱스 범위). 보존 **~15일**(보존 기간 섹션). 추출 SQL: `e4_k_empty_decomposition`(공차), `e3b_k_cycle_refined_v2`(사이클), `c08/e5_k_crane_q`(큐 대기).
:::

### 3.2 `MCH_OPERATION` — 크레인 작업 기록 (move 단위)

크레인 move 1건 = 1행. **QC 생산성(MPH)·QC 유휴(K_QC_NOMOVE)·크레인 가동률**의 원천. JOB_ORDER_HISTORY보다 가볍고(LOW~MEDIUM) 깊게 보존(≥35일).

| 컬럼 | 의미 · 예 |
|---|---|
| `MCH_OPER_MACHNO` | 장비 번호. `^C[0-9]+$` = QC(안벽), RTG·M·Z 등(코드 사전 참조). |
| `ST_DT` | move 시작(YYYYMMDDHH24MISS, 앞 14자). |
| `MCH_OPER_COMPDATE` + `MCH_OPER_COMPTIME` | 완료 날짜(YYYYMMDD) + 시각(HH24MISS). 인덱스 술어 = COMPDATE. |
| `MCH_OPER_JOBTYPE` | LD/DS 등. MPH는 LD/DS만 집계. |
| `MCH_OPER_VESSEL` / `MCH_OPER_VOYAGE` / `MCH_OPER_CONTNO` | 선박 / 항차 / 컨테이너. |
| `TRK_ID` | 이 move를 처리한 트럭(TT) ID — 항차당 distinct TT 수 집계. |

:::note
인터벌 병합(연속 move 간 짧은 갭)으로 크레인 가동 구간을 만들고, **병합 구간 사이의 갭 = QC 유휴(K_QC_NOMOVE)**. 추출 SQL: `c07_k_mph_realtime`, `f2_k_qc_q`, `e1c_k_util_crane_merged_intervals`.
:::

### 3.3 `VSS_STATISTICS` — 항차 계획·실적 (선박/항차 단위)

(선박, 항차)당 1행(최신 업데이트 우선). **계획 물량·규격 혼합·공식 생산성** — 진행률·맥락 피처. 30일 윈도우.

| 컬럼 | 의미 |
|---|---|
| `VSS_STT_VESSEL` / `VSS_STT_VOYAGE` / `VSS_STT_UP_DT` | 선박 / 항차 / 업데이트 시각. |
| `VSS_STT_VAN` / `VSS_STT_MOVES` / `VSS_STT_TEU` | 계획 컨테이너 수 / 계획 move / TEU. (진행중 항차는 MOVES가 NULL인 경우 많음.) |
| `VSS_STT_SIN_MOV` / `TWN_MOV` / `TND_MOV` | 단/트윈/탠덤 move 수(규격 혼합). |
| `VSS_STT_GROSSTIME` / `NETTIME` / `ABERTHTIME` | 총시간 / 순시간 / 실접안시간(분). |
| `VSS_STT_WORKQC` | 작업 QC 수. |
| `VSS_STT_GQCR` / `NQCR` | Gross / Net QC Rate(move/hr) = 공식 MPH(전체시간 / 유휴제외). |
| `VSS_STT_GBP` / `NBP` | Gross / Net Berth Productivity. |

:::note
로드 **TRIVIAL**(~41K, FTS). 추출 SQL: `voyage_plan`(계획 물량), `c06_k_mph_voyage`(공식 rate). 진행률 = moves/VAN(VAN 있을 때만, 약 1/11 항차).
:::

### 3.4 작업시간·장비 마스터 (가동률 입력)

| 테이블 · 컬럼 | 의미 |
|---|---|
| `MCH_WORKTIME`: MACHNO, START_DT, END_DT, STARTDATE/ENDDATE | TT(YT) 작업/로그인 세션. Postgres에서 병합(로그아웃 이상 플래그)해 가동률(K_UTIL_TT) 산출. |
| `MCH_WORKSTOP`: MACHNO, START_DT, END_DT, STARTDATE/ENDDATE | TT 정지/휴식 구간. 가동시간에서 차감. |
| `CDY_MACHINE`: CDY_MCHN_CODE, CDY_MCHN_TYPE | 장비 마스터. TYPE='YT'(야드트럭)로 TT 필터, QC/YC 등. |

:::note
추출 SQL: `e3a_k_util_tt_merged`(YT 가동률, 세션-정지 병합). 로드 LOW(~10K).
:::

### 3.5 라이브 작업지시 — 작업 풀 (현재 진행/대기)

위 3.1~3.4는 **완료된** 작업의 이력입니다. 반면 **지금 이 순간 처리해야 할 작업**(라이브 작업 풀)은 두 테이블에 살아있습니다 — 계속 갱신(`UPD_DT`=현재)되는 운영 테이블. 90초 주기로 추출해 대시보드 TT 페이지의 QC 시퀀스/배차 화면을 채웁니다. 자세한 갱신·융합은 [차량·작업 풀 갱신](/kc/architecture/dispatch-pools/) 문서 참조.

**`JOB_ORDER_LIST`** — 개별 컨테이너 move (HISTORY의 라이브 판; 단, 완료행도 보관되므로 라이브만 필터 필수)

| 컬럼 | 의미 · 예 |
|---|---|
| `JOB_ODR_JOBSTATUS` | **작업 상태** — A 활성(배차됨·진행중) · Q 대기(백로그) · P 계획 · B 차단 · C 완료. 라이브 = A/Q/P/B + `COMPDATE IS NULL`. |
| `JOB_ODR_QUEUENAME` | 큐 이름(예 `34H-D`=베이34 선창 양하, `42D-L`=베이42 갑판 적하). 큐 스케줄과의 **조인 키**. 주의: 항차마다 재사용됨(과거판 다수). |
| `JOB_ODR_YTNO` | **배정된 TT**(예 `TT1153`). GPS device id와 일치 → 라이브 위치/상태 머지. 활성 작업은 전부 배정됨. |
| `JOB_ODR_ETW_DT` | **ETW = 크레인 준비 시각**(YYYYMMDDHH24MISS[mmm], MYT). `ETW − 현재` = 작업 시급도. **예측 연구의 ②목표를 TOS가 라이브로 제공**. |
| `JOB_ODR_ARMGC` / `JOB_ODR_CONTNO` | 야드크레인(RTG) / 컨테이너 번호. |
| `JOB_ODR_YT_TOPOS` · `CRNT_PSN_IDX_NO1` · `YT_TO_PSN_IDX_NO1` | 야드 블록-베이(예 `10Q-0405`) · 출발 위치 · 도착 위치. |
| `JOB_ODR_TWINTANDEM` · `SWAP_FLG` · `CRE_DT` | 트윈/탠덤 · 스왑 플래그 · 생성시각(스캔 경계 `≥ SYSDATE−2`로 stale 고아 제거). |

**`JOB_QUEUE_SCHEDULE`** — QC별 작업 큐 계획 (작고 깨끗, 한 큐당 1행)

| 컬럼 | 의미 · 예 |
|---|---|
| `JOB_QUE_CRANENO` | **QC(안벽크레인)** — 실제 id `C11..C55 · M·Z · DC##`. `C##`는 websocket GPS/PLC 크레인 id와 직접 매칭. |
| `JOB_QUE_QUEUENAME` / `JOB_QUE_VESSEL` | 큐 이름(=LIST의 조인 키) / 선박. (선박·큐이름) **유일** — 그래서 QC는 Postgres에서 이 깨끗한 스냅샷으로 붙임(Oracle 조인은 과거 큐와 fan-out). |
| `JOB_QUE_DISLOAD` / `JOB_QUE_SEQ` | 양하(D)/적하(L) / QC가 큐를 처리하는 순서. |
| `JOB_QUE_TOTALQTY` / `COMPQTY` / `PLANQTY` | 총 / 완료 / 계획 물량. **잔여(백로그) = TOTAL − COMP**. (개별 대기 move는 ETW/순서 없는 묶음이라, 깊이는 이 차이로 표시.) |

:::tip[왜 중요]
QC id(C##)가 TOS 계획 ↔ websocket PLC(크레인이 물리적으로 도는 중) ↔ GPS(배정 TT 실위치)를 한데 묶어, **"계획 + 실측"이 융합된 라이브 QC 시퀀스**가 됩니다. 로드 = 90초마다 bounded 2쿼리(큐 ~725행 + 활성 move ~130행, ~1–3s).
:::

## 코드 · 의미 사전

### 4.1 작업종류(jobtype) / 장비 프리픽스

**작업종류 (JOBTYPE)**: LD 적하(yard→vessel) · DS 양하(vessel→yard) · MO/MI 구내이적(out/in) · RH 재취급 · GI/GO 게이트 in/out · 그 외 AH·LC·GC 관측. 배차 핵심은 LD·DS·MO·MI.

**장비 프리픽스 (device id)**: TT 야드트럭(프라임무버) · RTG 야드 갠트리크레인 · C## 안벽크레인(QC) · M## 모바일하버크레인 · Z# 스트래들 · TC 트랜스퍼 · ES/RS 엠티/리치 스태커 · CR·PPM 등.

:::caution[중요 — PLC를 주는 크레인]
실시간 PLC(`ctab`)는 **동적 크레인 C/M/Z만** 송신. **RTG(야드크레인)는 PLC 미수신** → 양하 블록측 핸드오버는 GPS·근접도로만 추정(배차 연구 참조).
:::

### 4.2 위치 · 블록 · POINT_TYPE

위치는 여러 형태의 문자 코드로 표현됩니다.

| 코드 형태 | 의미 · 예 |
|---|---|
| 블록 베이 `NN[A-Z]-bay` | 야드 블록의 베이. 예 `07F-06`(블록 07F, 베이 06), `01PW-0405`, `08K-1213`. |
| 블록 라벨 `NN[A-Z](S\|W)?` | 예 `01U`, `01YS`(S=짝), `12XW`(W=홀). 내부 ID 매핑: `01Y→Y_B01`, `01YS→Y_EB02`, `01YW→Y_EB01`. |
| 크레인 `[CMZ][0-9]+` | 안벽/동적 크레인. `topos1`가 이 형태면 목적지가 크레인. |
| 특수 `WHARF_*·CT*·FUEL_*` | 예 `WHARF_23_B`, `CT4_WORKSHOP`. 내부 매핑 없음 → 근접점 매칭. |

**POINT_TYPE 코드 (레이아웃 매칭)**

- 1 안벽작업(quay)
- 2 블록 side
- 3 블록 end
- 4 buffer
- 5 게이트 IN
- 6 게이트 OUT
- 7 battery
- 10 handler · 11 pinning
- 0 extra · 8/9 new/del(제외)

레이아웃(블록/도로/포인트/노드/링크)은 mm CAD 좌표 → 등거리+6-파라미터 아핀 투영으로 lat/lon 변환(RMS ~70m). 라이브맵·매칭에서 사용.

## 7개 KPI 정의

대시보드는 6개 표시(K_RTG_Q 숨김). 모두 intensive(평균·비율)라 기간 합산 시 분자/분모로 정확 결합.

| KPI | 단위·방향 | 정의 (소스) | 가중치 |
|---|---|---|---|
| **K_EMPTY** 공차거리 | km/Job · 낮을수록↑ | Σ공차m / Σjobs / 1000 (JOB_ORDER_HISTORY) | jobs |
| **K_EMPTY_R** 공차비율 | % · 낮을수록↑ | Σ공차m / Σ(공차+적재)m (JOB_ORDER_HISTORY) | 미터 |
| **K_CYCLE** 사이클타임 | s · 낮을수록↑ | 작업 첫~마지막 이벤트 간격의 jobs-가중평균 (JOB_ORDER_HISTORY) | jobs |
| **K_UTIL** TT 가동률 | % · 높을수록↑ | **TT별 min(1, 가동분/경과분)의 평균**·100 — avg-of-ratios(단순 가중평균 아님) (MCH_WORKTIME/STOP) | (평균) |
| **K_QC_NOMOVE** QC 대기 | s · 낮을수록↑ | QC 병합 active 구간 사이 유휴 갭 평균 (MCH_OPERATION C##) | idle_periods |
| **K_MPH** QC 처리량 | move/hr · 높을수록↑ | QC move/active-hour의 active_hours-가중평균 (MCH_OPERATION) | active_hours |
| **K_RTG_Q** 야드 핸드오버 대기 (숨김) | s · 낮을수록↑ | (ACTV_DT − YT_DIS_DT) — TT 하차→야드 핸드오버 (JOB_ORDER_HISTORY). 안벽 QC 대기가 아님(ARMGC=RTG/ES). | in_range |

:::note[K_UTIL 특수]
avg-of-ratios라 주/월 버킷은 일 단위 raw에서 재계산해야 정확(단순 평균 불가). 오늘분은 `util_tt_shift`(TT별 productive_min + 쉬프트 elapsed_min)에서 정확 재조합.
:::

## 서빙 모델 (PostgreSQL 3-tier)

| 계층 | 테이블 | 역할 |
|---|---|---|
| L0 raw | `raw_k_empty · raw_k_cycle · raw_k_crane_q_daily · raw_k_qc_q · raw_k_mph_realtime · raw_k_util_tt · raw_k_util_crane · raw_voyage_plan` | Oracle 일 스냅샷(분자/분모 보존). PK=(date, …) 멱등 upsert. |
| L1 rollup | `kpi_daily`(kpi_key,snapshot_date,value,sample_n,…) · `kpi_breakdown_qc` | KPI별 일 권위 값(트렌드·이력의 소스). 야간 확정 / 틱 잠정. |
| L2 stats | `kpi_baseline`(4주 롤링 + Welch t-test) | 기준선·유의성. |
| LIVE | `kpi_shift · kpi_shift_history · vessel_shift · vessel_qc_shift · util_tt_shift` | 현재 교대 누적(틱이 채움) → "오늘"을 Oracle 재스캔 없이 합산. |
| 작업풀 | `live_workqueue · live_workpool` | 라이브 작업 풀 스냅샷(90초 full-replace). API `/api/workpool` → TT 페이지. [상세](/kc/architecture/dispatch-pools/). |

### 기간 합산 (agg.rs)

범위 [from,to] = **과거일 raw_***(분자/분모 정확) + **터미널-오늘은 kpi_shift**에서 폴드(제2 Oracle 스캔 0). 6개는 합/비율로 정확 결합, K_UTIL은 raw 재계산. 일/주/월 이력도 동일 원리(`/api/kpis/history`).

:::caution[시간대(필수)]
서버=KST(UTC+9)지만 터미널/Oracle=**MYT(UTC+8, Westports)**. 교대 감지·기간 경계는 반드시 `wp_core::shift::terminal_now()` 사용. 서버 `Local::now()`를 쓰면 교대 1h 오판·창이 데이터를 놓쳐 0행.
:::

### 추출 스케줄 (systemd user timer, opt-in)

- **야간(01:30 MYT)** `wp-nightly` — 어제 권위 전체 추출 + transform + baseline(~32s, 1회 풀스캔).
- **교대 틱** `wp-shift-t1`(~3분: MPH/QC대기/가동률+선박, LOW) · `wp-shift-t2`(~15분: 공차/사이클/크레인대기, JOB_ORDER_HISTORY).
- **작업풀 틱** `wp-workpool`(90초: 라이브 작업지시 2테이블 → live_workqueue/live_workpool, bounded).
- `enable-linger`로 로그아웃 후 생존.

## 함정 · 운영 노트 (반복 실수 방지)

| 함정 | 내용 · 대응 |
|---|---|
| SQL 주석의 아포스트로피 | `--` 주석에 `'`(예 `crane's`)가 있으면 toolbox/Oracle **HTTP 400**. em-dash는 OK. 곱슬따옴표도 금지. |
| `ORDER BY <집계 별칭>` | 금지 — 표현식이나 일반 컬럼 사용. |
| `include_str!` SQL 수정 | .sql 편집 후 .rs를 touch해 재임베드해야 반영. |
| 시간대 MYT vs KST | 위 서빙 모델 섹션. terminal_now() 필수. KST 00:00–01:00(=MYT 23:00–24:00)에 "오늘"이 아직 시작 안 한 터미널 날을 가리켜 0/7 blank. |
| 빈 결과셋 | toolbox가 `{"result":"null"}` 반환 → 0행 처리. |
| K_RTG_Q 라벨 오해 | "QC 크레인 대기"가 아님 — ARMGC가 RTG/ES라 **야드 핸드오버 대기**. 실제 "QC가 트럭을 기다림"은 별도 K_QC_NOMOVE. |
| RTG PLC 없음 | 양하 블록측 핸드오버는 직접 PLC 신호 없음 → GPS·RTG 근접도로 추정(배차 연구). |
| 보존 한계 | 보존 기간 섹션. 깊은 과거(예 1월)는 백필 불가. 깊이는 앞으로만 누적. |
| 직접 ssh+curl 금지 | Oracle 접근은 승인된 `remote-toolbox-sql`만(`SKILL_DIR=/home/aiadmin/.codex/skills/yard-db-ops`). |
