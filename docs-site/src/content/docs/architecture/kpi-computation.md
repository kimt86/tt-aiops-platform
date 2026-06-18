---
title: KPI 산출 — 7개 지표가 어떻게 계산되는가
description: 대시보드의 모든 KPI는 TOS Oracle에서 추출해 PostgreSQL에 누적·집계한다. 운영 Oracle 부하 최소화와 정확한 기간 합산이라는 두 원칙을 다룬다.
sidebar:
  order: 4
---

대시보드의 모든 KPI는 **TOS Oracle**에서 추출해 **PostgreSQL에 누적**하고, 거기서 집계합니다. 핵심 원칙은 둘 — **운영 Oracle 부하 최소화**(정해진 시각에만, 한 번 가져온 날은 영구 보관)와 **정확한 기간 합산**(분자/분모를 보존해 어떤 구간도 정확히 결합). 이 문서는 그 파이프라인을 설명합니다.

기준 **2026-06-09** · 원천 **TOS Oracle**(websocket 아님) · 저장 **PostgreSQL 3-tier** · 대시보드/API는 **Postgres만** 읽음

## 30초 요약

- **어디서** — KPI는 **전부 TOS Oracle**에서. 추출기(extractor)만 Oracle을 읽고, 그 결과를 Postgres에 누적.
- **어떻게** — 원천 행을 **분자/분모로 보존**(L0) → 일 단위 권위 값(L1) → 기준선·유의성(L2). 대시보드는 Postgres만 읽음.
- **왜 정확** — 평균/비율을 단순 평균하지 않고 **분자합/분모합**으로 결합. 그래서 일·주·월 어떤 구간도 정확.

:::note[핵심 트릭]
"오늘"은 아직 진행 중인데도 **Oracle을 다시 스캔하지 않습니다**. 교대 틱이 이미 가져온 현재-교대 누적(`kpi_shift`)을 과거일 값에 폴드해서 합산합니다(§3). 야간에 권위 값으로 확정.
:::

## 7개 KPI 정의

대시보드는 6개 표시(K_RTG_Q 숨김). 모두 **intensive**(평균·비율)라 기간 합산 시 분자/분모로 정확 결합됩니다. 각 KPI의 **가중치** = 진짜 분모.

| KPI | 단위·방향 | 정의 (TOS 소스) | 가중치 |
|---|---|---|---|
| **K_EMPTY** 공차거리 | km/Job · 낮을수록↑ | Σ공차m / Σjobs / 1000 (JOB_ORDER_HISTORY) | jobs |
| **K_EMPTY_R** 공차비율 | % · 낮을수록↑ | Σ공차m / Σ(공차+적재)m (JOB_ORDER_HISTORY) | 미터 |
| **K_CYCLE** 사이클타임 | s · 낮을수록↑ | 작업 첫~마지막 이벤트 간격의 jobs-가중평균 (JOB_ORDER_HISTORY) | jobs |
| **K_UTIL** TT 가동률 | % · 높을수록↑ | **TT별 min(1, 가동분/경과분)의 평균**·100 — avg-of-ratios (MCH_WORKTIME/STOP) | (평균) |
| **K_QC_NOMOVE** QC 대기 | s · 낮을수록↑ | QC 병합 active 구간 사이 유휴 갭 평균 (MCH_OPERATION C##) | idle_periods |
| **K_MPH** QC 처리량 | move/hr · 높을수록↑ | QC move/active-hour의 active_hours-가중평균 (MCH_OPERATION) | active_hours |
| **K_RTG_Q** 야드 핸드오버 대기 (숨김) | s · 낮을수록↑ | (ACTV_DT − YT_DIS_DT) — TT 하차→야드 핸드오버 (JOB_ORDER_HISTORY) | in_range |

:::caution[K_QC_NOMOVE ≠ K_RTG_Q]
**K_QC_NOMOVE**(표시) = "QC가 트럭을 기다림"(안벽 크레인 유휴 갭). **K_RTG_Q**(숨김) = "TT가 야드에서 핸드오버를 기다림"(ARMGC=RTG). 둘은 다른 대기입니다. 자세한 TOS 컬럼은 [TOS DB 레퍼런스](/kc/architecture/tos-db-reference/).
:::

## KPI별 추출·가공·적재 상세

KPI 하나하나가 **TOS Oracle의 어떤 테이블·컬럼**에서 나와, **추출 SQL이 무엇을 왜** 계산하고, **Postgres의 어느 `raw_*` 테이블·컬럼**에 멱등 적재되며, **transform.rs가 어떻게 `kpi_daily`로 롤업**하는지를 코드 그대로 추적합니다. 각 KPI는 네 단계 — **① 소스 · ② 가공+왜 · ③ 적재 · ④ 롤업** — 로 나눕니다.

:::note[공통 규약]
모든 일 단위 SQL은 `{{DAY_STR}}`(=YYYYMMDD)을 **인덱스-세이프 문자열 리터럴**로 주입(`params.rs`). DAY 경로는 `{{TIME_PREDICATE}}`를 **빈 문자열**로 채워 검증본 SQL과 **바이트 동일**; SHIFT(라이브) 경로는 `AND (시각열) BETWEEN '시작' AND '끝'`을 끼워 교대 창으로 좁힙니다. `/*+ NO_PARALLEL */` + `FETCH FIRST`로 운영 Oracle 부하를 억제합니다. 모든 `raw_*` upsert는 `ON CONFLICT (PK) DO UPDATE` — 같은 날을 다시 돌려도 덮어쓰기만(중복 없음).
:::

### K_EMPTY · K_EMPTY_R — 공차거리 / 공차비율

두 KPI는 같은 추출 SQL(`e4_k_empty_decomposition.sql`) 한 번으로 같은 `raw_k_empty` 행에서 나옵니다(분자만 다름).

#### ① 소스 (TOS 테이블·컬럼)

| TOSADM 컬럼 | 역할 |
|---|---|
| `JOB_ORDER_HISTORY` | 완료 작업 이력 (소스 테이블, FROM) |
| `JOB_HIST_DATE` | 인덱스 범위 = `'{{DAY_STR}}'` (PK 범위 스캔) |
| `JOB_HIST_JOBTYPE` | 작업 유형 (그룹·출력 키) |
| `JOB_HIST_CONTNO · _POINT · _SEQNO` | 작업 1건의 자연 식별자 (GROUP BY 키) |
| `JOB_HIST_TIME` | 교대 산정용 (SUBSTR 앞 2자리 = 시각) |
| `LNDN_TRV_RNG` | 적재(laden) 주행거리 m → `lndn` |
| `UN_LNDN_TRV_RNG` | 공차(empty) 주행거리 m → `un_lndn` |
| `CRNT_PSN_IDX_NO1` | 블록 ID (MAX → `block_id`, distinct 카운트) |

인덱스/필터 술어: `JOB_HIST_DATE = '{{DAY_STR}}'` · `LNDN_TRV_RNG BETWEEN 0 AND 5000` · `UN_LNDN_TRV_RNG BETWEEN 0 AND 5000` · `HAVING COUNT(*) >= 50` · `FETCH FIRST 50 ROWS ONLY`.

#### ② 가공 + 왜

```sql
-- 작업 1건 = (CONTNO,POINT,SEQNO,JOBTYPE,DATE) 단위로 먼저 접고(GROUP BY),
--   교대는 그 작업의 첫 이벤트 시각(MIN(TIME))으로 결정
CASE WHEN SUBSTR(MIN(JOB_HIST_TIME),1,2) BETWEEN '00' AND '07' THEN 'Night'
     WHEN SUBSTR(MIN(JOB_HIST_TIME),1,2) BETWEEN '08' AND '15' THEN 'Day'
     WHEN SUBSTR(MIN(JOB_HIST_TIME),1,2) BETWEEN '16' AND '23' THEN 'Evening' END
-- 그 다음 (jobtype × shift)로 다시 집계
ROUND(SUM(un_lndn) / NULLIF(SUM(lndn + un_lndn),0), 4)  AS k_empty_ratio
SUM(un_lndn) AS total_empty_m,  SUM(lndn) AS total_laden_m,  COUNT(*) AS jobs
```

:::note[왜 0~5000m로 거르나]
**한 작업의 정상 주행거리 범위**를 벗어난 값(센서 오류·미터값 결측·창고 간 비정상 이동)을 분모/분자에서 제외하기 위함. 음수나 5km 초과는 GPS/주행 산정 오류로 보고 버립니다. 그래야 `SUM(un_lndn)/SUM(lndn+un_lndn)` 비율이 한두 개의 이상치에 오염되지 않습니다.
:::

:::note[왜 비율 대신 합을 저장하나]
SQL이 행마다 `k_empty_ratio`를 계산하긴 하지만, 적재되는 핵심은 **분자 `total_empty_m`·분모 `total_laden_m`·`jobs`의 합**입니다. 기간 합산 시 비율을 단순 평균하면(작업량이 다른 날을 동등 취급) 틀리고, 합을 보존해야 어떤 구간도 `Σ공차 / Σ(공차+적재)`로 정확히 재계산됩니다.
:::

#### ③ 적재 (Postgres)

대상: `raw_k_empty` · PK `(snapshot_date, jobtype, shift)` (`k_empty.rs`).

| raw_k_empty 컬럼 | 출처 (SQL 출력) |
|---|---|
| `jobtype, shift` | JOB_HIST_JOBTYPE, 교대 CASE (PK 일부) |
| `jobs` | COUNT(*) — K_EMPTY 가중치 |
| `total_empty_m` | SUM(un_lndn) — **K_EMPTY 분자 / K_EMPTY_R 분자** |
| `total_laden_m` | SUM(lndn) — K_EMPTY_R 분모의 적재 성분 |
| `k_empty_ratio, avg_empty_m, avg_laden_m, distinct_blocks` | 참고용(롤업엔 합 컬럼만 사용) |

```sql
INSERT INTO raw_k_empty (snapshot_date, jobtype, shift, jobs, …)
ON CONFLICT (snapshot_date, jobtype, shift) DO UPDATE SET
  total_empty_m=EXCLUDED.total_empty_m, total_laden_m=EXCLUDED.total_laden_m, … extracted_at=now()
```

#### ④ 롤업 (→ kpi_daily)

```
-- K_EMPTY (km/Job)
value = round(sum(total_empty_m)/nullif(sum(jobs),0)/1000, 4)   sample_n = sum(jobs)
-- K_EMPTY_R (%)
value = round(sum(total_empty_m)/nullif(sum(total_empty_m+total_laden_m),0)*100, 4)
sample_n = sum(jobs)
```

`transform.rs`가 그날 `raw_k_empty`의 (jobtype×shift) 행 전부를 합쳐 위 식으로 산출. `HAVING sum(jobs)>0`(또는 `sum(empty+laden)>0`)이라 데이터 없는 날은 아무것도 쓰지 않음. 라이브(`shift.rs`)에서는 K_EMPTY 가중치=`jobs`, **K_EMPTY_R 가중치=`Σ(empty+laden)` 미터**(jobs 아님)로 `kpi_shift.agg_weight`에 따로 저장합니다.

### K_CYCLE — 작업 사이클 타임

#### ① 소스

| TOSADM 컬럼 | 역할 |
|---|---|
| `JOB_ORDER_HISTORY` | 소스 테이블 (FROM) |
| `JOB_HIST_DATE` | 인덱스 범위 `= '{{DAY_STR}}'` |
| `JOB_HIST_JOBTYPE` | 유형 (그룹·출력 키) |
| `JOB_HIST_CONTNO · _POINT · _SEQNO` | 작업 1건 식별자 (GROUP BY) |
| `JOB_HIST_TIME` | 이벤트 시각 — `DATE||TIME`의 MIN/MAX로 사이클 양끝 |

#### ② 가공 + 왜 (2-pass, 평균+2σ 이상치)

```sql
job_events: 작업 1건 = 첫 이벤트 ~ 마지막 이벤트
cycle_sec = (MAX(DATE||TIME) − MIN(DATE||TIME)) * 86400     -- 초 단위
WHERE transitions > 1                -- 이벤트 1개뿐이면 간격 0 → 제외
stats(jobtype별): 평균·중앙·표준편차·P25/75/95, 이상치 임계치
thr = AVG(cycle_sec) + 2*STDDEV(cycle_sec)
outlier_n = (SELECT COUNT(*) FROM cycles WHERE cycle_sec > thr)
```

:::note[왜 transitions>1 / 평균+2σ인가]
이벤트가 하나뿐인 작업은 시작=끝이라 사이클이 0초 — 의미 없는 값이라 거릅니다. 그리고 사이클 타임은 꼬리가 긴 분포(장비 고장·대기로 비정상 장시간)라, **평균+2σ를 이상치 임계로** 두어 정상 작업의 평균이 극단값에 끌려가지 않도록 `outlier_n`을 따로 셉니다. 중앙값·P95도 함께 보존해 분포를 진단합니다.
:::

#### ③ 적재

대상: `raw_k_cycle` · PK `(snapshot_date, jobtype)` (`k_cycle.rs`). 핵심 컬럼 `jobs`(=COUNT), `avg_sec`(=ROUND(avg_cyc,1)); 그 외 `med_sec·std_sec·p25/75/95_sec·outlier_threshold_sec·outlier_n·avg_transitions`은 진단용. `ON CONFLICT (snapshot_date, jobtype) DO UPDATE`.

#### ④ 롤업

```
value = round(sum(avg_sec*jobs)/nullif(sum(jobs),0), 1)   -- jobs-가중 평균 초
sample_n = sum(jobs)         HAVING sum(jobs) > 0
```

가중치=`jobs`. 라이브도 동일(`Σ(avg_sec·jobs)/Σjobs`), `agg_weight=jobs`.

### K_UTIL — TT(야드 트랙터) 가동률

#### ① 소스

| TOSADM 컬럼 | 역할 |
|---|---|
| `CDY_MACHINE` | `CDY_MCHN_TYPE='YT'`인 `CDY_MCHN_CODE` = TT 장비 목록 |
| `MCH_WORKTIME` | 로그인 세션 — `MCH_WORK_MACHNO · _START_DT · _END_DT · _STARTDATE · _ENDDATE` |
| `MCH_WORKSTOP` | 정지 구간 — `MCH_STOP_MACHNO · _START_DT · _END_DT · _STARTDATE · _ENDDATE` |

인덱스 술어: `MCH_WORK_STARTDATE = day_str OR MCH_WORK_ENDDATE = day_str`(세션이 자정을 걸쳐도 포착), `LENGTH(...DT) >= 14`. 창은 `params` CTE의 `{{START_TS}}/{{END_TS}}`로 클립. `FETCH FIRST 50 ROWS ONLY`.

#### ② 가공 + 왜 (구간 병합 → 캡)

```sql
세션 시작/끝을 창[START_TS,END_TS]으로 GREATEST/LEAST 클립
flagged→grouped→merged: 겹치거나 잇닿은 세션을 하나의 구간으로 병합
active_min_merged = SUM((grp_end − grp_start) * 1440)
stop_min          = SUM((e_dt − s_dt) * 1440)        -- MCH_WORKSTOP
productive_min    = active_min_merged − stop_min
k_util_capped = LEAST(1.0, (productive_min) / {{ELAPSED_DENOM}})
k_util_raw    =            (productive_min) / {{ELAPSED_DENOM}}
```

:::note[왜 세션을 병합하나]
운영자가 **로그아웃을 빼먹으면** 세션이 겹치거나 비정상적으로 길어집니다. 겹치는 세션을 병합(`MAX(end) OVER … 1 PRECEDING` 누적 플래그)하면 같은 시간을 이중 계산하지 않고 **실제 점유 구간**만 남습니다. `has_overlap → logout_anomaly`로 그 이상을 표시합니다.
:::

:::caution[왜 캡(≤1)과 avg-of-ratios인가]
분모 `{{ELAPSED_DENOM}}`는 DAY 경로에서 **1440분**(하루), SHIFT 경로에서 **교대 경과분**. 병합 오차로 가동분이 경과분을 살짝 넘을 수 있어 `LEAST(1.0,…)`로 캡합니다. K_UTIL은 **TT별 비율을 먼저 구한 뒤 그것들을 평균**(avg-of-ratios)하므로, 분자/분모 단순합으로는 결합되지 않습니다 — 이 점이 다른 6개와 근본적으로 다릅니다.
:::

#### ③ 적재

대상: `raw_k_util_tt` · PK `(snapshot_date, machno)` (`k_util_tt.rs`). 컬럼: `active_min · stop_min · productive_min · k_util_capped · k_util_raw · sessions_total · interval_groups · logout_anomaly`. `ON CONFLICT (snapshot_date, machno) DO UPDATE`. (참고: QC/YC용 `e1c` → `raw_k_util_crane`도 같은 병합 로직, machine_type QC/YC.)

#### ④ 롤업

```
value = round(avg(k_util_capped)*100, 4)    -- TT별 캡 비율의 단순 평균 ×100
sample_n = count(*)                          -- 그날 TT 수
```

:::note[K_UTIL은 특별 취급]
주/월·"오늘"은 분자/분모 합으로 결합할 수 없어, 오늘분은 `util_tt_shift`(TT별 `productive_min` + 교대 `elapsed_min`)에서 **TT별 day_util = min(1, Σ가동분/Σ경과분)**을 만들고 그 평균×100으로 **정확 재조합**합니다(`agg.rs`). 그래서 `kpi_shift.agg_weight=NULL`이고, 야간 권위 런이 최종 채웁니다.
:::

### K_QC_NOMOVE — QC(안벽 크레인) 대기 (표시)

#### ① 소스

| TOSADM 컬럼 | 역할 |
|---|---|
| `MCH_OPERATION` | 장비 move 이력 (FROM) |
| `MCH_OPER_COMPDATE` | 인덱스 `= '{{DAY_STR}}'` |
| `MCH_OPER_MACHNO` | 장비번호 — `REGEXP_LIKE(…,'^C[0-9]+$')` 안벽 크레인만 |
| `ST_DT` | move 시작 시각 → `s` (`IS NOT NULL AND LENGTH>=14`) |
| `MCH_OPER_COMPDATE||_COMPTIME` | move 완료 시각 → `e` |
| `MCH_OPER_JOBTYPE` | `IN ('LD','DS')` 적/양하만 |
| `MCH_OPER_VESSEL · _VOYAGE` | 크레인-선박 배정 분리(파티션 키) |

#### ② 가공 + 왜 (구간 병합 → 갭)

```sql
moves → flagged → grouped → merged: 잇닿은 move를 active 구간으로 병합
gaps: 한 구간의 끝(ge) ~ 다음 구간의 시작(LEAD(gs)) 사이 = 유휴(idle)
idle_sec = (LEAD(gs) OVER(… ORDER BY gs) − ge) * 86400
버킷팅: <1m / 1-5m / 5-10m / 10-30m / >30m
avg_idle_sec = AVG(idle_sec WHERE 0..1800)   -- 30분 이하만
HAVING COUNT(*) >= {{QCQ_HAVING}}             -- day=10, shift=2
```

:::note[왜 move를 병합해 갭을 보나]
한 컨테이너 작업은 여러 move 행으로 쪼개져 있어, 인접 move를 **하나의 active 구간으로 병합**해야 "크레인이 실제로 멈춰 있던 시간"이 드러납니다. 그 구간 사이의 갭이 곧 **QC가 다음 트럭을 기다린 유휴**(K_QC_NOMOVE)입니다. 30분(1800초) 초과 갭은 작업 종료/교대 공백으로 보고 평균에서 제외합니다.
:::

:::note[왜 HAVING ≥ N 갭인가]
갭이 한두 개뿐인 크레인은 평균이 통계적으로 불안정합니다. DAY 경로는 `≥10`(신뢰), SHIFT 경로는 `≥2`(부분 교대라도 근사값이라도 나오게) — `params.rs`의 `{{QCQ_HAVING}}` 토큰으로 전환합니다.
:::

#### ③ 적재

대상: `raw_k_qc_q` · PK `(snapshot_date, qc)` (`k_qc_q.rs`). 컬럼: `idle_periods · avg_idle_sec · med_idle_sec · quick_under_1m · normal_1_5m · delayed_5_10m · extended_10_30m · over_30m · total_tt_wait_sec · total_idle_30m_sec`. `ON CONFLICT (snapshot_date, qc) DO UPDATE`.

#### ④ 롤업

```
value = round(sum(avg_idle_sec*idle_periods)/nullif(sum(idle_periods),0), 1)
sample_n = sum(idle_periods)          HAVING sum(idle_periods) > 0
```

가중치=`idle_periods`(=sample_n과 동일). 라이브도 동일 식, `agg_weight=Σidle_periods`.

### K_MPH — QC 시간당 처리량 (move/hr)

#### ① 소스

| TOSADM 컬럼 | 역할 |
|---|---|
| `MCH_OPERATION` | 장비 move 이력 (FROM) |
| `MCH_OPER_COMPDATE` | 인덱스 `= '{{DAY_STR}}'` |
| `MCH_OPER_MACHNO` | `REGEXP_LIKE(…,'^C[0-9]+$')` 안벽 크레인만 → `qc_machno` |
| `MCH_OPER_JOBTYPE` | `IN ('LD','DS')` 적/양하만 |
| `MCH_OPER_COMPTIME` | `SUBSTR(...,1,2)` = 시각 → `active_hours` |
| `MCH_OPER_VESSEL · _VOYAGE` | 그룹 키 (선박/항차별 QC) |
| `TRK_ID · MCH_OPER_CONTNO` | distinct 트럭/컨테이너(진단) |

`GROUP BY vessel, voyage, qc_machno` · `ORDER BY moves DESC FETCH FIRST 30 ROWS ONLY`.

#### ② 가공 + 왜

```sql
moves = COUNT(*)
active_hours = COUNT(DISTINCT SUBSTR(MCH_OPER_COMPTIME,1,2))   -- 실작업 시각 수
k_mph_per_active_hour = ROUND(COUNT(*) / NULLIF(active_hours,0), 2)
```

:::note[왜 active_hours로 나누나(달력 24h 아님)]
크레인은 하루 24시간 내내 작업하지 않습니다. 실제 move가 발생한 **distinct 시각 수**로만 나눠야 "작업 중일 때의 처리율"이 됩니다. 정박 대기·유휴 시간이 분모에 들어가 처리량을 왜곡하는 걸 막습니다. 그래서 기간 가중치도 **active_hours**입니다(voyage 수가 아님).
:::

#### ③ 적재

대상: `raw_k_mph_realtime` · PK `(snapshot_date, vessel, voyage, qc_machno)` (`k_mph_realtime.rs`). 핵심: `k_mph_per_active_hour · active_hours · moves · load_moves · discharge_moves`. `ON CONFLICT (…4키…) DO UPDATE`. (공식 항차 집계 `c06` → `raw_k_mph_voyage`는 `VSS_STATISTICS`의 `VSS_STT_GQCR/NQCR`를 30일 창으로 별도 보관.)

#### ④ 롤업

```
value = round(sum(k_mph_per_active_hour*active_hours)/nullif(sum(active_hours),0), 2)
sample_n = count(distinct vessel||'/'||voyage)   -- 표시 N = 항차 수
HAVING sum(active_hours) > 0
```

:::caution[N과 가중치가 다름]
화면의 `sample_n`은 **항차 수**지만, 실제 기간 결합 가중치는 `Σactive_hours`입니다. `shift.rs`는 `kpi_shift.agg_weight=Σactive_hours`를 따로 저장해(`0009` 마이그레이션) "오늘" 결합을 정확히 합니다 — sample_n으로 폴백하면 K_MPH는 근사가 됩니다.
:::

### K_RTG_Q — 야드 핸드오버 대기 (숨김)

#### ① 소스 · ② 가공

```sql
FROM TOSADM.JOB_ORDER_HISTORY   WHERE JOB_HIST_DATE = '{{DAY_STR}}'
  AND YT_DIS_DT IS NOT NULL AND JOB_HIST_ACTV_DT IS NOT NULL
crane_q_sec = (ACTV_DT − YT_DIS_DT) * 86400        -- TT 하차 → 야드 핸드오버
in_range = COUNT(WHERE crane_q_sec BETWEEN 0 AND 1800)
k_crane_q_avg_sec = AVG(crane_q_sec WHERE 0..1800)  -- ARMGC=RTG, 음수/30분초과는 이상
```

컬럼 `YT_DIS_DT`(TT 하차 시각) · `JOB_HIST_ACTV_DT`(야드 크레인 활성) · `JOB_HIST_ARMGC` · `JOB_HIST_JOBTYPE`. 0~1800초로 거르는 이유는 K_QC_NOMOVE와 같음(음수=시각 역전 오류, 30분 초과=핸드오버 아닌 공백).

#### ③ 적재 · ④ 롤업

대상: `raw_k_crane_q_daily` · PK `(work_date, jobtype)` (`k_crane_q_daily.rs`; 시간별은 `e5`→`raw_k_crane_q_hour`). 롤업: `value = round(sum(k_crane_q_avg_sec*in_range)/nullif(sum(in_range),0), 1)`, `sample_n = sum(in_range)`, 가중치=`in_range`.

:::caution[K_QC_NOMOVE ≠ K_RTG_Q]
**K_QC_NOMOVE**(표시) = MCH_OPERATION의 안벽 크레인(C##) 유휴 갭 = "QC가 트럭을 기다림". **K_RTG_Q**(숨김) = JOB_ORDER_HISTORY의 `ACTV_DT−YT_DIS_DT` = "TT가 야드 핸드오버를 기다림"(ARMGC=RTG). 서로 다른 대기입니다.
:::

:::note[정리 — 분자·분모 보존의 일관성]
6개 표시 KPI는 모두 **가중치(=진짜 분모)**를 `raw_*`와 `kpi_shift.agg_weight`에 보존합니다: K_EMPTY=jobs, K_EMPTY_R=Σ미터, K_CYCLE=jobs, K_MPH=active_hours, K_QC_NOMOVE=idle_periods, K_RTG_Q=in_range. 덕분에 일·주·월 어떤 구간도 `Σ(value·weight)/Σweight`로 정확히 결합되고(§4), K_UTIL만 avg-of-ratios라 `util_tt_shift`로 재조합합니다.
:::

## 3계층 — 원천에서 화면까지

```
TOS Oracle (SOURCE)            완료 작업 이력. 추출기만 읽음(인덱스 범위·FETCH FIRST).
  ▶ raw_k_* (L0 raw)           일 스냅샷 — 분자/분모를 보존(예 공차m·적재m·jobs·active_hours). 멱등 upsert.
  ▶ kpi_daily (L1 rollup)      KPI별 일 권위 값(트렌드·이력의 소스). 야간 확정 / 틱 잠정.
  ▶ kpi_baseline (L2 stats)    4주 롤링 기준선 + Welch t-test(유의성).
```

대시보드/API는 **L1·L2와 LIVE 테이블만** 읽습니다. Oracle 미접근(구조적 보장).

| 계층 | 대표 테이블 | 역할 |
|---|---|---|
| L0 raw | `raw_k_empty · raw_k_cycle · raw_k_qc_q · raw_k_mph_realtime · raw_k_util_tt …` | 분자/분모 보존, PK=(date,…) 멱등. |
| L1 rollup | `kpi_daily` · `kpi_breakdown_qc` | KPI별 일 값(잠정/권위 플래그). |
| L2 stats | `kpi_baseline` | 기준선·델타·p-value. |
| LIVE | `kpi_shift · kpi_shift_history · vessel_shift · util_tt_shift` | 현재 교대 누적(틱이 채움) → "오늘"의 입력(§3). |

:::note[왜 분자/분모를 보존하나]
"공차비율 60%"를 그냥 평균하면 틀립니다(작업량이 다른 날을 동등 취급). L0가 **Σ공차m·Σ적재m**을 들고 있으면, 어떤 구간이든 `Σ공차 / Σ(공차+적재)`로 정확히 재계산됩니다.
:::

## "오늘"을 Oracle 재스캔 없이

오늘은 아직 진행 중이라 권위 값이 없습니다. 그렇다고 매번 Oracle을 다시 긁으면 부하가 큽니다. 그래서:

```
기간 [from, to] 집계 (agg.rs)
값 = 과거일(raw_*에서 분자/분모, 정확)  ＋  터미널-오늘(kpi_shift에서 폴드)
                                                  └ 교대 틱이 이미 가져온 현재-교대 누적
        ⇒ 제2 Oracle 스캔 0. 분자합/분모합으로 결합 → today·this_week·last7 모두 정확
```

범위를 `raw_to = min(to, 오늘−1)`에서 나누고, 오늘분은 `kpi_shift`의 (값·가중치) 합을 더합니다. "오늘"이 포함된 어떤 기간(this_week/this_month/last7)도 라이브로 반영되되 추가 Oracle 스캔이 없습니다.

:::caution[K_UTIL만 예외]
K_UTIL은 avg-of-ratios(TT별 비율의 평균)라 단순 합산이 안 됩니다. 오늘분은 `util_tt_shift`(TT별 productive_min + 교대 elapsed_min)에서 **정확 재조합**(Σ가동분/Σ경과분, 캡, 평균)합니다. 다른 6개는 분자/분모로 깔끔히 결합.
:::

## 추출 스케줄 (systemd 타이머)

| 작업 | 주기 · 내용 | Oracle 부하 |
|---|---|---|
| **야간** `wp-nightly` | 01:30 MYT — 어제 **권위** 전체 추출 + transform(L1) + baseline(L2). 1회 풀스캔(~32s). | 1회/일 |
| **교대 틱 T1** `wp-shift-t1` | ~3분 — MPH·QC대기·가동률 + 선박 패널. MCH_OPERATION 1회. | LOW |
| **교대 틱 T2** `wp-shift-t2` | ~15분 — 공차·사이클·크레인대기. JOB_ORDER_HISTORY. | MEDIUM |

:::tip[잠정 → 권위]
낮 동안 "오늘"은 교대 틱이 채운 **잠정** 값(provisional 배지). 밤 01:30에 야간 런이 어제를 다시 권위 값으로 확정하고 기준선을 갱신합니다. 한 번 권위로 확정된 날은 다시 스캔하지 않습니다 — 그래서 이력 깊이는 "추출을 시작한 시점부터 앞으로" 영구 누적됩니다(보존 한계는 [TOS 레퍼런스 §2](/kc/architecture/tos-db-reference/)).
:::

## 일 / 주 / 월 이력

`GET /api/kpis/history?gran=day|week|month` — 버킷×6KPI 매트릭스. **Postgres 전용·Oracle 0**.

| 단위 | 계산 |
|---|---|
| 일별 | `kpi_daily`를 단일 쿼리로 피벗 — 트렌드 값과 동일 보장. |
| 주/월별 | 버킷마다 `agg.aggregate(from,to)` 재사용 — 과거 raw + 오늘 kpi_shift를 분자/분모로 정확 결합(K_UTIL 포함). |

버킷은 터미널 시간(MYT) 기준(ISO 월~일 주, 달력 월), 미래는 오늘로 클램프. 데이터 없는 버킷은 `—`로 표시(에러 아님). 화면은 표 + 열(KPI) 클릭 시 추이 차트.

## 주의 사항

| 항목 | 내용 |
|---|---|
| K_UTIL avg-of-ratios | 단순 평균 금지 — 주/월은 일 단위 raw에서 재계산, 오늘은 util_tt_shift에서 재조합(§3). |
| 시간대 MYT vs KST | 서버=KST(UTC+9)지만 터미널/Oracle=**MYT(UTC+8)**. 교대·기간 경계는 반드시 터미널 시간 사용. 안 그러면 1h 오판·창이 데이터를 놓쳐 0행. |
| 잠정(provisional) | 오늘/현재 기간은 잠정 — 야간 확정 전까지. K_QC_NOMOVE는 교대 중 과소(임계 미달)일 수 있음. |
| 보존 한계 | 깊은 과거(예 1월)는 Oracle에서 삭제되어 백필 불가. [TOS 레퍼런스 §2](/kc/architecture/tos-db-reference/). |
