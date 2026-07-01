---
title: 용어집 & FAQ
description: 플랫폼 문서와 대시보드에 등장하는 용어를 주제별로 한두 줄씩 정리한 용어집과 자주 묻는 질문.
sidebar:
  order: 1
---

플랫폼 문서와 대시보드에 등장하는 용어를 주제별로 한두 줄씩 정리했습니다. 모든 용어와 질문에 고유 앵커(`#id`)가 있어 다른 문서에서 바로 딥링크할 수 있고, 브라우저 검색(Ctrl+F)으로 찾으셔도 됩니다. 구체적인 궁금증은 [자주 묻는 질문](#faq)에서 먼저 확인해 보시기 바랍니다.

빠른 이동: [① 장비·작업](#g-equip) · [② TOS 데이터](#g-tos) · [③ websocket 필드](#g-ws) · [④ 지표·개념](#g-metrics) · [⑤ 내부 시스템](#g-internal) · [FAQ](#faq)

## ① 장비 · 작업

터미널 현장의 장비와 작업 유형입니다. 작업 유형(jobtype)은 TOS·websocket·사이클 로그 모두에서 같은 코드를 씁니다.

**TT · 야드 트럭** — `Yard Truck / TT####`
: 안벽(QC)과 야드 블록 사이에서 컨테이너를 운반하는 트럭. 이 플랫폼의 관리 대상입니다. GPS device id(예 `TT1045`)와 TOS의 [ytno](#ytno)가 같은 번호 체계를 씁니다.

**QC · 안벽 크레인** — `Quay Crane / C##`
: 안벽에서 선박↔TT 사이 컨테이너를 옮기는 크레인(예 `C39`). QC가 멈추지 않게 TT를 제때 보내는 것이 배차 문제의 핵심이며, QC는 [PLC(ctab)](#plc-load)를 송신합니다.

**RTG · 야드 크레인** — `RTG / ARMGC`
: 야드 블록에서 TT↔적치장 사이를 처리하는 크레인. TOS에서는 ARMGC로 표기합니다. PLC가 없어 핸드오버 관여 여부는 **TT GPS와의 근접(같은 bay ≈ 30m)**으로 판정합니다.

**DS · 양하** — `Discharge`
: 선박에서 내려 야드로 옮기는 작업. TT 기준 **픽업=안벽(QC), 드롭=블록(RTG)**입니다.

**LD · 적하** — `Load`
: 야드에서 꺼내 선박에 싣는 작업. TT 기준 **픽업=블록, 드롭=안벽(QC)** — 양하(DS)와 방향이 반대입니다.

**MI / MO / LC · 야드 이동** — `yard moves`
: 야드 안에서 컨테이너를 옮기는 이적 계열 작업 유형. 픽업·드롭이 모두 야드 쪽이라, 사이클 복원에서는 **첫 ARRIVED를 픽업으로 간주**합니다.

**트윈 · 탠덤** — `twin / tandem`
: 컨테이너 2개를 한 번에 함께 운반·처리하는 방식. websocket에서는 [container1·container2](#container1)가 모두 채워지고, 작업 풀과 [tt_cycle_log](#tt-cycle-log)의 `twintandem` 필드로 기록됩니다.

## ② TOS 데이터

작업 계획·배정의 권위 원천. 운영 Oracle은 추출기만 읽고, 대시보드는 Postgres 복제본만 봅니다([FAQ](#faq-oracle)). 상세는 [TOS DB 레퍼런스](/kc/architecture/tos-db-reference/)를 참고하시기 바랍니다.

**TOS** — `Terminal Operating System`
: 터미널 운영 시스템(Oracle). 작업 지시·이력의 권위 원천이며, API가 직접 조회하지 않고 [추출기](#extractor)가 주기적으로 PostgreSQL로 복제합니다.

**JOB_ORDER_LIST** — `live job orders`
: TOS 라이브 작업지시 테이블 — 개별 컨테이너 move 단위로 배정 트럭([ytno](#ytno))·컨테이너([contno](#contno))·[ETW](#etw)·RTG를 담습니다. 작업 풀의 두 원천 중 하나.

**JOB_QUEUE_SCHEDULE** — `QC queue plan`
: QC별 작업 큐 계획 테이블 — 크레인(C##)·순서(SEQ)·양/적·진행률(잔여 = TOTAL−COMP). `JOB_ORDER_LIST`와 Postgres에서 조인해 QC별 작업 시퀀스를 만듭니다.

**JOBSTATUS** — `A / B / Q (P, C)`
: 작업 상태 코드: **A** 활성(배차됨·진행중) · **Q** 대기(백로그) · **B** 차단 · P 계획 · C 완료. [가동률](#utilization) 정의에서 "작업중"=A, "투입"=A·B·Q입니다.

**ytno** — `YTNO — assigned truck`
: 작업에 배정된 TT 번호(예 `TT1153`). GPS device id와 일치해 **TOS 계획 ↔ websocket 실측을 잇는 연결 고리**입니다.

**contno** — `CONTNO — container no.`
: 컨테이너 번호(예 `EGSU2064058`). 작업과 사이클을 식별하는 기본 요소입니다.

**topos / from_pos / to_pos** — `position codes`
: 작업의 위치 필드 — 출발(from_pos)·도착(to_pos)과 목적지(topos) 코드. 블록-베이(예 `03U-21`) 또는 크레인(`C39`) 형식입니다.

**ETW · 크레인 준비시각** — `Estimated Time of Work`
: 크레인이 해당 작업을 처리할 준비가 되는 예정 시각. TOS DB 컬럼(`JOB_ODR_ETW_DT`)은 수 시간 오차가 확인되어, 정확값은 [tos_etw_gateway](#etw-gateway)에서 받습니다([FAQ](#faq-etw)).

## ③ websocket 필드

GPS(wpt_gps zone)·크레인 PLC(ctab zone) 실시간 스트림의 주요 필드입니다. TOS와는 별개 시스템 — 구조와 함정은 [실시간 websocket 데이터](/kc/architecture/websocket-data/) 문서를 참고하시기 바랍니다.

**container1 / container2** — `wpt_gps`
: **물리 적재가 아니라 TOS "배정" 필드입니다.** 직전 drop 시점에 다음 컨테이너가 사전배정([c2c](#c2c))되므로 값이 차 있어도 트럭은 빈 차일 수 있습니다([FAQ](#faq-container1)). 2개 모두 채워지면 [트윈·탠덤](#twin-tandem).

**topos1 · 목적지 코드** — `wpt_gps`
: 현재 미션의 목적지 — 블록 베이(예 `03U-21`) 또는 크레인(`C39`). 픽업↔드롭 진행에 따라 동적으로 바뀝니다. 공차의 잔여 거리(스왑 판정)에도 사용됩니다.

**arrival / ARRIVED** — `wpt_gps`
: 목적지 도착 플래그. 픽업/드롭 구분은 **도착한 측면으로 복원**합니다 — LD는 블록 도착=픽업·크레인 도착=드롭, DS는 그 반대, MI/MO는 첫 도착=픽업.

**cur_loc · 현재 위치** — `wpt_gps`
: 트럭이 지금 있는 위치 라벨(예 `01U`). 좌표(lat/lon)와 함께 들어오는 부가 표시용 필드입니다.

**PLC 훅로드** — `ctab — load (t)`
: 크레인 후크 하중(톤). 적재 ≥ 1.0t·빈 후크 ~0이라 **하중 전이가 곧 PICKUP/DROP 순간**입니다. 시간당 처리량(move/hr)의 교차검증에도 씁니다. QC만 송신 — RTG에는 PLC가 없습니다.

## ④ 지표 · 개념

KPI와 사이클 검증에 쓰이는 핵심 개념입니다. 산출식 전체는 [KPI 산출](/kc/architecture/kpi-computation/) 문서를 참고하시기 바랍니다.

**사이클타임** — `cycle time`
: TT 작업 1건이 도는 데 걸린 시간. 대시보드 KPI는 **중위값(median)**을 쓰고 **양하(DS)/적하(LD)를 분리**해 표시합니다. 실측 TT 사이클은 약 13–14분 수준으로 TOS 기반 추정과 합치합니다.

**4단계 사이클** — `4-phase decomposition`
: **공차이동**(배정→픽업 도착) → **받기**(픽업 도착→출발) → **부하이동**(출발→드롭측 도착) → **주기**(도착→drop). 공차이동 시작은 배정 시각(assigned_at)을 프록시로 쓰며, ARRIVED 의존이라 관측 못 한 단계는 추정 없이 NULL로 둡니다([FAQ](#faq-phases)).

**공차 레그 분해 · drive_s** — `leg decomposition`
: 각 공차 레그를 30초 GPS 이동 세그먼트로 나눠 **주행(drive_s·움직인 시간·레그의 ~53%)** + **정지(stop_s·~47%·대부분 목적지 최종 접근/대기)** + **접근(arrived−gps_arrived·중앙값 ~67초)**으로 쪼갠 것. `learn_leg_decomp`에 적재. **실주행 속도는 ≈ 22.8km/h**이며, 한때 표시하던 "중앙값 6.9km/h"는 직선÷정지포함시간÷짧은레그 가중이 겹친 착시였습니다. drive_s는 같은 OD에서도 변동계수 0.758(±76%)로 대기 노이즈가 지배해, 어떤 거리 모델도 이 정확도 바닥을 넘지 못합니다.

**크레인 접근 대기** — `learn_crane_approach`
: 픽업 지점별 핸드오버/접근 대기 = arrived − gps_arrived (dest_topos: DS 픽업=안벽 크레인·LD 픽업=블록). 중앙값 67초, 크레인별 22–194초(~9배), 앞 트럭이 줄 서면 ~2.5배. **트럭별이 아니라 목적지 속성**이라, 2단계 트럭별 이동비용에서 빼내 1단계(데드라인) 신호로 다룹니다(현재 매트뷰만 구축·1단계 미연결). ARRIVED가 실제 도착과 거의 같으므로(중앙값 −4초) **큰 크레인 대기는 이동시간 안이 아니라 도착 이후**에 있습니다.

**2단계 이동비용 · 순수주행** — `pure-drive travel cost`
: 배차 2단계(최소비용 매칭)의 트럭→작업 이동비용 = free_in + 이동시간. 이동시간은 **현재 순수주행 격자 조회가 잠정**: 매트뷰 `learn_travel_zone225_drive`(225m OD 격자·주행만 p50/p90), L3 폴백 = quay_manhattan_m ÷ 6.33 m/s(22.8km/h). 실현시간 `learn_travel_zone225`는 계속 갱신하나 비용엔 미사용(참조용). OD 이동시간 학습 모델이 이 조회를 대체하는 것이 목표입니다.

**이동필터** — `150 m / 30 s`
: "진짜 인도" 검증 규칙 — [container1](#container1)이 비-빈 값에서 바뀌는 엣지 + **보유 ≥ 30초** + **운반 ≥ 150m**일 때만 인도로 인정합니다. 미달 엣지는 TOS 재배정 artifact로 기각합니다([FAQ](#faq-cycle-verify)).

**c2c · 연속적재** — `consecutive assignment`
: drop 시점에 다음 컨테이너가 이미 사전배정되어 작업이 끊김 없이 이어지는 패턴. websocket에서는 [container1](#container1)이 drop 직후 곧바로 다음 값으로 바뀌는 것으로 관측됩니다.

**staging · 배차·대기** — `assigned & waiting`
: **공차 + 정지 + 작업 배정됨** — 다음 작업을 받고 대기 중인 상태. 미배정 유휴([idle](#idle))와 구분하기 위해 신설했습니다([FAQ](#faq-idle), [staging vs soon_idle](#faq-staging-soonidle)).

**idle · 유휴** — `truly unassigned`
: **공차 + 정지 + 미배정** — 즉시 배차 가능한 진짜 유휴. staging 분리 후 idle은 미배정만 남았습니다(약 21대 수준).

**TT 가동률** — `utilization`
: 작업 풀 **배정 기반** 정의 — 작업중 = [JOBSTATUS](#jobstatus) A로 배정된 TT, 투입 = A·B·Q. 60초마다 찍는 [util_tt_sample](#util-tt-sample)을 시간 평균해 산출합니다.

**Little's law** — `W = L / λ`
: 안정 상태에서 시스템 내 평균 개체 수 L = 도착률 λ × 평균 체류시간 W라는 대기행렬 법칙. W = L/λ 형태로 대기시간을 추정·검증할 때 씁니다.

**운영일 · 교대** — `business date / shift`
: 지표가 귀속되는 터미널 운영일과 3교대 — Night 00–07 · Day 08–15 · Evening 16–23. 경계는 모두 **터미널 시간 MYT(UTC+8)** 기준이며 서버(KST)와 1시간 다릅니다.

## ⑤ 내부 시스템

우리가 만든 파이프라인의 구성 요소 — Rust(axum) API + PostgreSQL + React/Vite 위에서 돌아갑니다.

**extractor · 추출기** — `Oracle → Postgres`
: 운영 Oracle을 읽는 **유일한** 컴포넌트. 타이머로 정해진 주기에만 조회해 Postgres에 멱등 적재합니다. 작업 풀 틱은 ~90초이며, API 크레이트에는 Oracle 클라이언트가 아예 없습니다([FAQ](#faq-oracle)).

**live_workpool / live_assigned_tt** — `work-pool snapshot`
: 작업 풀 스냅샷 테이블(추출기 ~90초 틱). 원천은 [JOB_QUEUE_SCHEDULE](#job-queue-schedule) + [JOB_ORDER_LIST](#job-order-list) — QC별 시퀀스·[ETW](#etw)·배정 트럭([ytno](#ytno))을 담아 TT 페이지가 읽습니다.

**util_tt_sample** — `60 s sampler`
: 60초 주기로 TT별 배정 상태(작업중 A / 투입 A·B·Q)를 기록하는 샘플 테이블. 시간 기반 [가동률](#utilization) 평균의 원천입니다.

**tt_cycle_log** — `cycle log (ML data)`
: 검증된 사이클 1건 = 1행 — [ytno](#ytno) + 작업 메타(jobtype·vessel·voyage·container·qc·twintandem) + 단계 타임스탬프(assigned_at·pickup_arrived_at·pickup_left_at·arrived_at·dropped_at) + leg 거리/시간. UNIQUE(ytno, dropped_at)로 30초 주기 멱등 flush, CYCLES 페이지(`/api/tt-cycles`)가 읽습니다.

**tos_etw_gateway / wp-etw-bridge** — `accurate ETW path`
: 정확 [ETW](#etw) 공급 경로 — Azure의 FastAPI 게이트웨이가 TOS RPC `/RPC/yard/etw`를 폴링하고, SSH 터널 wp-etw-bridge(`127.0.0.1:18080`)로 받아 `tos_etw_cntr` 테이블에 upsert합니다(TTL 30분).

**learn_leg_decomp** — `empty-leg decomposition`
: 공차 레그별 1행 — [drive_s/stop_s/접근](#leg-decomp) + oz/dz(225m 격자)·출발/도착 좌표·dest_topos. ①(이동시간) 학습의 라벨 원천(mig 0075/0076/0078).

**learn_travel_zone225_drive** — `pure-drive OD travel cost`
: 225m OD 격자별 주행만(drive_s) p50/p90 매트뷰. [2단계 이동비용](#pure-drive-cost)의 **현재 소스**. 실현시간판 `learn_travel_zone225`는 참조용으로만 갱신됩니다.

**learn_crane_approach** — `pickup approach/queue wait`
: 픽업 지점(dest_topos)별 [접근·핸드오버 대기](#crane-approach) 매트뷰. 1단계 신호 후보(현재 미연결).

**road_node / road_edge** — `inferred road graph`
: GPS 궤적에서 매시 추론한 방향 도로망(~264노드/353방향엣지·mig 0077). Rust 방향 Dijkstra 라우터 `crates/api/src/roadgraph.rs`가 사용. **게이트에서 맨해튼보다 나쁘고 스냅 커버리지 30%라 핫패스 비용엔 미채택**, 향후 이동시간 모델의 경로거리 피처 공급용으로 보존합니다.

## 자주 묻는 질문

실제로 자주 받은 질문과, 데이터로 확인된 답변입니다.

### Q. 왜 운영 Oracle(TOS)을 직접 조회하지 않나요?

운영 TOS는 터미널 운영의 심장이라 대시보드 조회가 부하·장애 위험이 되기 때문입니다. 그래서 [추출기](#extractor)만 정해진 타이머에 Oracle을 읽어 PostgreSQL로 복제하고, 대시보드/API는 Postgres만 읽습니다. API 크레이트에는 Oracle 클라이언트 자체가 없어 **구조적으로도 직접 조회가 불가능**합니다.

### Q. 화면의 ETW가 TOS DB 값과 왜 다른가요?

TOS DB의 ETW 컬럼(`JOB_ODR_ETW_DT`)은 실제와 **수 시간** 어긋나는 것이 확인됐습니다. 그래서 TOS RPC(`/RPC/yard/etw`)를 폴링하는 Azure의 [tos_etw_gateway](#etw-gateway)에서 정확값을 받아 SSH 터널(wp-etw-bridge)로 수신하고 `tos_etw_cntr`에 적재합니다(TTL 30분). 터널이 끊기면 DB 값으로 폴백되어 정밀도가 떨어질 수 있습니다.

### Q. 차량 풀의 idle 수가 갑자기 줄었습니다. 버그인가요?

아니요, 분류 교정입니다. 기존 분류가 배정 여부를 무시해 **idle 102대 중 51대가 실제로는 배정된 상태**였습니다. 그래서 공차+정지+배정 = [staging](#staging)(배차·대기) 상태를 신설했고, idle은 진짜 미배정(약 21대 수준)만 남았습니다.

### Q. 사이클은 어떻게 "진짜"로 검증하나요?

[이동필터](#movement-filter)를 통과해야 합니다 — [container1](#container1)이 비-빈 값에서 바뀌는 엣지가 있고, 그 컨테이너를 **30초 이상 보유**했으며 **150m 이상 운반**했을 때만 진짜 인도로 인정합니다. 미달 엣지는 TOS 재배정 artifact로 보고 기각합니다.

### Q. container1이 차 있는데 트럭은 왜 빈 차인가요?

container1은 물리 적재가 아니라 **TOS의 배정 필드**이기 때문입니다. 직전 drop 시점에 다음 컨테이너가 사전배정([c2c](#c2c))됩니다. 실측 검증: LD 트럭 34대가 container1이 세팅된 채 **픽업 블록**에 ARRIVED했습니다. 그래서 픽업 시점은 container1이 아니라 [ARRIVED 신호의 측면 분류](#arrived)(LD: 블록=픽업/크레인=드롭, DS: 반대, MI/MO: 첫 도착=픽업)로 복원합니다.

### Q. 사이클 4단계가 일부만 채워진 행이 있는데요?

[4단계 분해](#four-phase)는 ARRIVED 신호에 의존해, 신호를 놓친 단계는 관측되지 않습니다. 이때 **추정하지 않고 NULL**로 둡니다. 같은 원칙으로, 수집기 재시작 등 관측 경계에서 생긴 artifact 56건도 원인을 규명한 뒤 NULL로 정리했습니다 — 처음에 "픽업도착<배정"을 '미리 가서 대기'로 오독했다가 데이터(전수 일치, 83%가 재시작 후 첫 사이클)로 정정한 사례입니다.

### Q. KPI 옆 "잠정" 표시는 무엇인가요?

낮 동안의 "오늘" 값은 교대 틱이 채운 잠정치라는 뜻입니다. 매일 야간(01:30 MYT) 권위 런이 어제 하루를 다시 추출해 확정하며, 한 번 확정된 날은 재스캔하지 않습니다. 상세는 [KPI 산출](/kc/architecture/kpi-computation/) 문서를 참고하시기 바랍니다.

### Q. ML 학습 데이터는 어디에 쌓이나요?

[tt_cycle_log](#tt-cycle-log)입니다. 검증된 사이클 1건당 1행 — 작업 메타 + 4단계 타임스탬프 + leg 거리/시간 — 으로 30초마다 멱등 flush되며, [2단계 예측 모형 연구](/kc/research/tt-prediction/)의 학습 데이터로 축적되고 있습니다. 대시보드 CYCLES 페이지에서 열람할 수 있습니다.

### Q. 화면에 데이터가 안 보이면 어디부터 봐야 하나요?

**FEED 페이지(WS 데이터 헬스)**부터 보시기 바랍니다. websocket 데이터는 메모리에만 살아 있어 SSH 터널이 끊기면 라이브 맵·차량 풀이 즉시 빕니다 — 연결·신선도·수신율을 먼저 확인합니다. KPI·작업 풀처럼 Postgres 기반 화면이라면 HEALTH 페이지에서 추출기 동작을 확인합니다.

### Q. staging과 soon_idle은 무엇이 다른가요?

[staging](#staging)은 **공차** 상태 — 빈 차가 정지해 있는데 다음 작업이 이미 배정되어 대기 중인 경우입니다. soon_idle은 **적재** 상태 — 짐을 실은 채 드롭 측에 도착해 크레인이 관여 중(마지막 핸드오버 진행)이라 곧 풀릴 트럭입니다. 즉 "이미 배정돼 기다림" vs "곧 비워짐"의 차이입니다. 분류 로직은 [배차 풀](/kc/architecture/dispatch-pools/) 문서 §2를 참고하시기 바랍니다.
