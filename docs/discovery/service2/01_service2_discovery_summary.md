# 01. Service 2 — TT Assignment 사전조사 종합 요약

> **⚠ 이 문서는 2026-07-22 시점의 스냅샷입니다.** 이후 두 가지가 바뀌어 일부 서술이 실제와 다릅니다:
> ① 프로젝트 개명(`wp-tt-dashboard` → `tt-aiops-platform`)으로 **systemd 유닛이 전부 `wp-*` → `tt-*`**, 크레이트가 `tt-*`가 됐습니다.
> ② 시나리오 서브시스템(`scengen`)이 재설계돼 출력 구조·수집 경로가 달라졌습니다.
> 현재 상태는 `deploy/systemd/README.md`, 루트 `README.md`, `/kc/data/equipment-deployment.html`을 기준으로 보세요.


| 항목 | 내용 |
|---|---|
| 문서 상태 | 내부 실행용 v1.0 |
| 조사일 | 2026-07-22 |
| 조사 방식 | 읽기 전용 Repository Discovery (코드·설정·마이그레이션·배포유닛·문서 정적 분석 + 로컬 호스트 systemd 상태 관찰) |
| 대상 | Service 2 — TT Assignment |
| 후속 활용 | AI 데이터 게이트웨이 3개월 진단·설계·안정화 컨설팅 사전자료 |

> 이 문서는 저장소 근거에 기반한 **현황 기술**이다. 업무 범위·일정·우선순위 결정은 Service 2 담당 PM의 소관이며,
> 이 문서에서 결정하지 않는다. 불명확한 항목은 **"Service 2 담당 PM 확인 필요"** 로 표시했다.

### 근거 표기 범례

| 표기 | 의미 |
|---|---|
| `경로:줄범위` | 저장소 파일에서 직접 확인한 근거 (기준 커밋 `10cc8c0`) |
| **[호스트]** | 2026-07-22 조사용 호스트에서 읽기 전용으로 관찰한 `systemctl --user` / `crontab -l` 결과. 저장소 밖 근거이므로 운영 담당자 재확인 권장 |
| **[문서]** | `kc/` 지식센터 또는 `docs/`의 서술. 코드로 확인된 사실과 구분 |
| 상태 | **확인**(직접 확인) / **추정**(정황 일치, 직접 확인 안 됨) / **미확인**(저장소에서 못 찾음) / **상충**(자료 간 불일치) |

---

## 1. 조사 저장소와 기준 커밋

| 항목 | 값 |
|---|---|
| 저장소 | `/home/tkadmin/projects/tt-aiops-platform` (단일 저장소) |
| 기준 브랜치 | `scengen-collector` |
| 기준 HEAD | `10cc8c0` — "싸이클: 트윈 컨테이너 ID 전체 표시(contnos·mig0099) + 적대검증 8건 반영" |
| 주 브랜치 | `main` |
| 워킹트리 | **미커밋 변경 5건** — `crates/api/src/cycles.rs`, `scripts/populate_tt_cycle_recon.sql`, `web/public/livemap-roadgraph.geojson`, `web/src/CyclesPage.tsx`, `web/src/api.ts` |
| 최상위 지침 파일 | `CLAUDE.md` / `AGENTS.md` **없음**. `README.md`는 초기 커밋 이후 미갱신 |

**주의 2건**

1. 조사 시작 시점에 미커밋 변경은 3건이었으나 조사 중 5건으로 늘었다. 이는 **외부(동시) 작업에 의한 변경**이며,
   본 조사는 저장소 파일을 일절 수정하지 않았다(조사 에이전트 41개 전원 파일 쓰기 0건).
   → 아래 인용한 줄 번호는 커밋 `10cc8c0` 기준이며, 미커밋 변경분(특히 `cycles.rs`, `api.ts`)은 인용과 달라질 수 있다.
2. `scripts/populate_tt_cycle_recon.sql`은 systemd 유닛이 **파일 경로 그대로 실행**한다
   (`deploy/systemd/wp-tt-cycle-recon.service:10-12`). 즉 미커밋 상태의 SQL이 이미 운영 타이머에서 돌고 있을 수 있다(상태: 추정).

---

## 2. 핵심 결론 (7줄)

1. **Service 2는 실제 TOS 배차를 수행하지 않는다.** 터미널 전체 TT×작업 매칭이 60초 주기로 실제 계산·기록되고 있으나,
   결과는 Postgres 그림자 테이블과 사내 대시보드 화면까지만 간다. TOS로 되돌아가는 쓰기 경로가 **코드에 존재하지 않는다.**
2. 그림자 매칭은 **운영에서 상시 가동 중**이다 — 배차 엔진을 품은 `wp-api.service`가 호스트에서 enabled + active다 **[호스트]**.
   따라서 "코드만 존재"가 아니라 **상시 Shadow 운영 + 사내 화면 Recommendation 표시** 단계다.
3. 매칭 알고리즘은 탐욕이 아니라 **자체 구현 min-cost max-flow(SPFA)** 로 전역 매칭을 푼다. 외부 솔버 라이브러리는 쓰지 않는다.
   다만 최적성은 프루닝·후보 절단 뒤의 부분그래프 기준이다.
4. **실배차 연동의 최대 장애물은 인터페이스가 아니라 식별자다.** 현재 매칭 산출물에는 컨테이너번호·작업지시 ID가 없고
   (QC, 선박, 큐, 작업유형, 출발블록) **버킷 단위**여서, 지금 형태로는 TOS에 "어느 작업지시를 어느 TT에"를 지목할 수 없다.
5. Oracle 접근은 CDC·Kafka·JDBC가 아니라 **저장소 밖 외부 CLI를 통한 주기 폴링**이며(TOSADM 13개 객체, 전부 SELECT),
   "읽기 전용"은 코드가 강제하는 것이 아니라 저장소 밖 스크립트와 DB 계정 권한에 의존한다(감사 관점 P0).
6. 비기능 요건(모니터링·알림·재시도·백업·롤백·인증)은 사실상 미착수이며, 프로젝트 스스로 **[문서]** 에서 미착수로 기록하고 있다.
7. **효과 수치가 자료마다 다르다**(40% vs −5.1%). 지표 정의·측정창·기준선이 서로 달라 외부 인용 전 단일화가 반드시 필요하다.

---

## 3. 현재 운영 모드

### 3.1 판정

| 후보 | 판정 |
|---|---|
| 문서상 개념만 존재 | 아니오 |
| 코드만 존재 | 아니오 (운영 활성 확인됨) |
| Offline Simulation | 아니오 (별도 시나리오 서브시스템은 존재하나 배차와 분리) |
| **Shadow Mode** | **예 — 상시 가동** |
| **Recommendation Mode** | **부분 — 사내 대시보드에 권고가 표시됨. 운영자가 이를 참고하는 절차의 실재 여부는 미확인(P0-4)** |
| 제한 시간대 Live Dispatch | 아니오 |
| 상시 Live Dispatch | 아니오 |
| 중단·대체됨 | 아니오 (최근 커밋까지 활발히 개발 중) |

**결론: 상시 Shadow 운영 + 사내 화면 Recommendation 표시.** 실제 배차 실행(Live)은 코드에 존재하지 않는다.

### 3.2 "코드에 있음"과 "운영에서 켜짐"의 분리

| 구분 | 사실 | 근거 | 상태 |
|---|---|---|---|
| TOS 명령을 만드는 코드 | **없음.** TOSADM 대상 INSERT/UPDATE/DELETE/MERGE·프로시저 호출 0건. 저장소의 모든 DML은 로컬 Postgres 대상 | `crates/extractor/sql/*.sql`(16개 전부 SELECT/WITH), `crates/api/src/main.rs:1-3` | 확인 |
| 매칭 결과의 종착지 | Postgres `stage2_match_shadow`(21일 보존) / `stage2_solver_shadow` | `crates/api/src/livemap.rs:4466`, `:4478`, `:4486` | 확인 |
| 코드가 스스로 밝힌 성격 | "SHADOW; never drives live dispatch" / "display/validation only" / "display only, never drives dispatch" | `crates/api/src/livemap.rs:3968-3972`, `db/migrations/0052_stage2_match_shadow.sql:1-2`, `crates/api/src/workpool.rs:935-936` | 확인 |
| API 표면 | 라우트 **31개 전부 GET**. 인증 계층 없음. 프론트에 POST/PUT/DELETE 호출 0건 | `crates/api/src/main.rs:45-75`, `web/src/api.ts:298` | 확인 |
| **운영 활성화** | `wp-api.service` = **enabled + active**(Restart=always, RestartSec=3) → 60초 그림자 매칭이 실제로 돌고 있음 | **[호스트]** | 확인(호스트) |
| 가동 조건 | 매 틱 GPS 웹소켓 연결(`lm.connected`)이 false면 그 틱은 건너뜀 → 피드 장애 구간에는 해가 산출되지 않음 | `crates/api/src/livemap.rs:4180-4182` | 확인 |
| 실배차 전환에 필요한 것 | 권고 출력 채널(A1) · TOS 소비 계약(A2) · 운영자 UI(D2) — **전부 미착수** | **[문서]** `kc/start/launch-plan.html` | 확인(문서) |

> **견적 시 반드시 분리할 것**: "배차 알고리즘"은 구현·가동되어 있으나, "배차 실행 연동"은 0에서 시작한다.

---

## 4. 현재 아키텍처와 TOS 연계

### 4.1 구성

- Rust 워크스페이스 4크레이트 — `core`(라이브러리), `extractor`(bin), `api`(bin), `scengen`(bin)
  + `web/`(React+Vite) + `kc/`(정적 HTML 지식센터) + `db/migrations/`(104개) + `deploy/systemd/`. 근거: `Cargo.toml:1-3`
- **배차 엔진은 별도 서비스가 아니다.** `wp-api` 프로세스 안의 백그라운드 tokio 태스크 24개 중 하나다
  (`crates/api/src/main.rs:115-139`). 같은 프로세스가 GPS 웹소켓 수집·학습 지속화·그림자 로깅을 함께 수행한다.
  → API 재시작은 곧 배차 계산·GPS 수집·학습 상태의 동시 중단이다.
- 배포는 쿠버네티스·컨테이너가 아니라 **단일 호스트의 systemd --user 유닛**이다. CI/CD·Dockerfile·k8s·IaC 산출물 **0건**.
  배포 = 수동 `cargo build --release` + 유닛 복사 (`deploy/systemd/README.md:19-38`).

### 4.2 데이터 흐름 (현재)

```
Oracle TOSADM ──(remote-toolbox-sql 폴링, 60초~4시간)──┐
Azure ETW 게이트웨이 ──(HTTP curl, 90초)──────────────┤
Open-Meteo / Tomorrow.io ──(HTTP, 1시간 / 3분)────────┼──> PostgreSQL ──> wp-api(31개 GET) ──> React 대시보드
GPS·PLC 웹소켓 ──(SSH 터널, 상시 스트림)──────────────┘         │
                                                    stage2_match_shadow 등 그림자 테이블
                                                              ╎
                                                   TOS로 되돌리는 경로 ── 미구현
```

### 4.3 TOS 연계 현황

| 항목 | 현황 | 상태 |
|---|---|---|
| 접근 방식 | **폴링**. 외부 CLI `$SKILL_DIR/scripts/remote-toolbox-sql`을 자식 프로세스로 실행, SQL을 임시파일로 전달 | 확인 |
| 게이트웨이 | **2개**(코드 공유 없음): `crates/extractor/src/runner.rs:41-72`(타임아웃 90초), `crates/scengen/src/toolbox.rs:44-73`(타임아웃 설정값, 기본 45초) | 확인 |
| CDC·Kafka·Debezium·JDBC | **사용하지 않음** | 확인 |
| 대상 | TOSADM 스키마 **13개 객체** | 확인 |
| SQL 문장 | 파일 16개 + 인라인 9개 = 25개, **전부 SELECT/WITH** | 확인 |
| 쓰기 | **없음** | 확인 |
| 읽기 전용의 강제 | **코드가 강제하지 않음.** `run_sql`은 임의 SQL을 그대로 외부 스크립트에 넘기며 DML 거부 가드가 없다. 실제 강제는 저장소 밖 스크립트와 Oracle 계정 권한 | **미확인 (P0-3)** |
| 별도 TOS 경로 | **Azure `tos_etw_gateway` HTTP REST** — `GET /v1/voyages/{vessel}/{voyage}/snapshot`, 90초마다, `wp-etw-bridge` SSH 터널 경유 (`crates/extractor/src/workpool.rs:111-170`) | 확인 |

---

## 5. 입력·출력 데이터 계약 (요약)

전체 인벤토리는 `03_service2_data_contract_inventory.md`와 `service2_data_inventory.csv` 참조.

### 5.1 입력

| 영역 | 소스 | 대표 객체 | 주기 |
|---|---|---|---|
| 미배차 수요 / 진행중 배차 | Oracle | `JOB_ORDER_LIST` → `live_candidate` / `live_workpool` | 90초 |
| QC 작업큐·진행률 | Oracle | `JOB_QUEUE_SCHEDULE` → `live_workqueue` | 90초 |
| 선박 일정(마감 원천) | Oracle | `VSB_VOYAGE` → `live_vessel_schedule` | 90초 |
| 완료 핸드오버(정답 라벨) | Oracle | `JOB_ORDER_HISTORY` → `tos_handover_label` | 60초 증분 |
| 크레인 무브 | Oracle | `MCH_OPERATION` → `qc_move_log` / `rtg_move_log` | 각 5분 증분 |
| 컨테이너별 ETW | **Azure HTTP 게이트웨이** | `tos_etw_cntr` | 90초 |
| 장비 위치·미션 | **GPS 웹소켓** | 인메모리 devices → `truck_pos_hist`/`hifreq` | 상시 스트림 |
| 크레인 PLC | **웹소켓 ctab** | 인메모리 plc | 상시(~1초) |
| 날씨 | Open-Meteo / Tomorrow.io | `weather_hourly` / `weather_1min` | 1시간 / 3분 |

**TT 위치의 유일한 좌표 소스는 GPS 웹소켓이다.** TOS 추출 SQL 16개에 lat/lon 컬럼이 0건이다
(`crates/extractor/sql/assigned_tt.sql` 주석 "pure TOS, no GPS"). 웹소켓이 끊기면 좌표 폴백이 없다.

### 5.2 출력

| 산출물 | 형태 | 소비처 |
|---|---|---|
| Stage-2 매칭 권고 | Postgres `stage2_match_shadow` (60초, 21일 보존) | `/api/stage2/advisory`·`/shadow` → 사내 화면 |
| 솔버 성능(그리디 대비) | `stage2_solver_shadow` | `/api/stage2/shadow`, `/api/health/dispatch` |
| TOS 대비 비교 | `dispatch_compare_shadow`(60초), `fair_compare_shadow`/`detail`(5분) | `/api/stage2/compare`, `/fair-compare` |
| 1단계 예측 검증 | `dispatch_pred_sample`(2분) | `/api/learn/dispatch-pred` |
| 시나리오/에뮬레이터 JSON | `scenario.*` → 온디맨드 조립 | 별도 포트 8899 브라우저 다운로드 |

**외부로 나가는 계약은 0건이다** — DigiPort·KPI 계층·webhook·Kafka·CSV export 어느 것도 코드에 없다.
**OpenAPI/AsyncAPI/JSON Schema 산출물도 0건**이다(`utoipa` 의존성은 선언만 되고 코드 사용 0).

### 5.3 실배차 연동의 결정적 제약

`stage2_match_shadow` 스키마에는 **컨테이너번호·작업지시 ID(MSNSEQ)가 없다**
(`db/migrations/0052_stage2_match_shadow.sql:5-22`). 매칭 입력 자체가 (QC, 선박, 큐, 작업유형, 출발블록, 수량 n) 버킷 집계다
(`crates/api/src/workpool.rs:796-822`). 상태: **확인**.

> ⇒ TOS에 배차를 전달하려면 스키마 변경과 **개별 작업 단위 매칭으로의 재설계**가 함께 필요하다.
> 이것이 A2(TOS 계약) 공수를 좌우하는 최대 변수다. → **P0-2**

---

## 6. 데이터량·지연·정합성 단서

### 6.1 근거 있는 값

| 항목 | 값 | 근거 |
|---|---|---|
| 최단 TOS 폴링 | **60초**(handover) | `deploy/systemd/wp-handover.timer` |
| 작업풀 갱신 | **90초** | `deploy/systemd/wp-workpool.timer` |
| 매칭 실행 | **60초**(GPS 연결 틱만) | `crates/api/src/livemap.rs:4173-4182` |
| 호스트 enabled 타이머 | **16개**(60초 ~ 4시간 + 야간 1회) | **[호스트]** |
| Oracle 동시성 | 프로세스 내 전역 Mutex로 **직렬화(동시성 1)**. 프로세스 간 직렬화는 외부 스크립트에 위임(미확인) | `crates/extractor/src/runner.rs:9-10` |
| 쿼리 캡 | 증분 스트림 FETCH 3,000~20,000행. **90초 작업풀 쿼리만 행수 캡 없음**(상태 필터 + 2일 범위) | `crates/extractor/sql/workpool.sql` |
| GPS 케이던스 | TT p50 3.0초 / p90 30초, RTG p50 60초 **[문서]** | `kc/data/websocket-coverage.html` |
| 신선도 임계 | FRESH 15초 / STALE 120초 / LOST 600초. 프론트는 배너 60초·맵 120초·작업풀 300초로 **다름** | `crates/api/src/livemap.rs:27-33` |
| 화면 지연 버퍼 | 기본 5초(라이브맵 재생 버퍼) | `web/src/LiveMapPage.tsx:453` |
| 커넥션 풀 | API 8 / extractor 4 / scengen 2 | 각 `db.rs` |
| 매칭 문제 크기 | 버킷 수 ≤ 트럭 수 ⇒ 쌍 수 ≲ 트럭수². 솔버 입력은 `arr<1800초` 프루닝. **후보 차량 pool 자체엔 상한 없음** | `crates/api/src/livemap.rs:4346-4364`, `:4406-4410` |
| 위치 이력 보존 | `truck_pos_hist` 2일 / `truck_pos_hifreq` 5일 / `rtg_pos_hist` 3일 (프로세스 내부 DELETE로만 정리) | `crates/api/src/livemap.rs:4673`, `:4829`, `:4726` |
| 원천 보존 | `JOB_ORDER_HISTORY` ~15일, `MCH_OPERATION`/`VSS` ≥35일 → **깊은 백필 불가** | `README.md:82-84` |

### 6.2 근거 없는 값 — 측정 필요 (기본값·샘플값을 운영 실측으로 쓰면 안 됨)

| 미지수 | 현재 자료 상태 | 견적 영향 |
|---|---|---|
| **TT 총 대수·가동 대수** | 코드 상수 없음. 문서 관측치가 TT~280 / TOS 495 / 웹소켓 539 / 엔진ON ~138로 제각각. RTG ~100 vs 187, QC ~28 vs 42~54 vs 61 | 규모 변수 전체 |
| **GPS 메시지율** | 문서 간 ~40건/초 vs ~965건/분(≈16건/초) **상충**. 코드엔 `rate_per_min` 카운터만 있고 저장 로그 없음 | 수집 CPU·대역이 2.5배 차이 |
| **현장 이벤트 → TOS Oracle 반영 지연** | **전혀 측정되지 않음.** 우리 폴링·표시 지연만 알 수 있음 | "실시간 배차" SLA 약속 불가 |
| **90초 작업풀 쿼리 반환 행 수 분포** | 캡이 없는 유일한 쿼리인데 실측 없음 | 물량 피크 시 틱 지연 |
| **Postgres 테이블 크기·증가율** | 용량 문서는 추정치. 코드엔 런타임 `reltuples` 조회만 | 스토리지 사이징 |
| **부하시험·벤치마크** | **0건**. 용량 문서 스스로 "부하 테스트 후 확정" 단서 | 성능 검증 공수 |

### 6.3 정합성 위험 (대표)

| 위험 | 내용 | 근거 |
|---|---|---|
| 워터마크 영구 누락 | 증분 워터마크가 문자열 사전순 `GREATEST` 전진만 하므로, 미래 날짜 이상치 1건이면 워터마크가 튀고 이후 정상 데이터가 영구 누락 | `crates/extractor/src/handover.rs:106-120` |
| 자정 경계 유실 | `qc_moves`/`rtg_moves`/scengen yard는 `COMPDATE = 오늘` 등가조건 → 자정 넘겨 정지하면 전날 미수집분 **영구 복구 불가** | `crates/extractor/src/qc_moves.rs:59-64` |
| 캡 무경보 | FETCH_CAP 도달을 scengen만 이벤트로 남기고 extractor는 알리지 않음 → 밀림이 조용히 누적 | `crates/extractor/src/rtg_moves.rs:19-20` |
| 이력 부재 | `live_*` 5개 테이블은 매 tick 전량 교체 → 과거 큐·마감 추이 재현 불가 | `crates/extractor/src/workpool.rs:266-268` |
| 중복/유실 미구분 | 무브·핸드오버가 `ON CONFLICT DO NOTHING` → 같은 키 재사용 시 후속 이벤트가 조용히 버려짐 | `crates/extractor/src/qc_moves.rs:82-87` |
| **신선도 게이트 결손** | Stage-2는 GPS 연결만 확인하고 **Oracle 미러(`live_workpool`)의 신선도는 검사하지 않음**. 300초 FROZEN 판정은 프론트 전용 → workpool 타이머가 죽어도 낡은 작업목록으로 매칭이 계속 산출·기록됨 | `crates/api/src/livemap.rs:4180-4182`, `web/src/TtPage.tsx:583` |
| 배차시각 근사 | `live_workpool.upd_ts`는 "행 최종수정 ≈ 배차시각"이라는 근사. TOS가 다른 이유로 행을 갱신하면 오염 | `crates/extractor/sql/workpool.sql:23-25` |

---

## 7. 운영·보안·Fallback

| 영역 | 현황 | 상태 |
|---|---|---|
| 헬스체크 | 3개 — `/api/health`(일 ETL 신선도), `/api/livemap/health`(GPS 피드), `/api/health/dispatch`(배차 틱 age<120초). readiness/liveness 구분 없음 | 확인 |
| 장애 알림 | **push 알림 0.** Prometheus·Grafana·OpenTelemetry·Sentry·PagerDuty·webhook·SMTP 전부 0건, systemd `OnFailure=` 0건. 감지는 사람이 화면을 봐야 하는 pull 방식만 | 확인 |
| 중복 실행 방지 | DB advisory lock·파일 락·leader election **전무**. 프로세스 내 Mutex와 oneshot 특성에만 의존 | 확인 |
| 재시도 | 추출 실패 시 재시도·백오프 **없음** — 로그만 남기고 다음 주기 대기. oneshot 유닛에 `Restart=` 없음 | 확인 |
| 자동 복구 | 상주 서비스만 Restart 설정(wp-api always/3s, ws-bridge·etw-bridge always/5s, scenario-web on-failure/5s) **[호스트]** | 확인 |
| 감사 로그 | `etl_run_log`(상태·행수·오류) + `data_freshness` + scengen `gen_run`/`gen_event`. **요청 단위 Trace ID·상관관계 ID 없음**, TraceLayer 미적용. etl_run_log 보존정책 없음 | 확인 |
| 인증·CORS | 대시보드 API **인증 계층 없음**, `CorsLayer::permissive()`. 바인드는 127.0.0.1. scengen 웹은 **0.0.0.0:8899에 무인증 킬스위치 POST** 노출 | 확인 |
| DB 권한 | `db/grants.sql`의 읽기전용 `wp_ro`는 **채택된 적 없는 잔존 계획** — API는 Postgres에 다수 INSERT/DELETE 수행. `README.md:75-76`은 미이행 체크리스트 | **상충** |
| 비밀 관리 | 시크릿 매니저 없음. `.env` 단일 파일(**권한 0644**) + systemd EnvironmentFile. **스크립트·crontab·`db/grants.sql`에 평문 DB 비밀번호가 커밋/등록되어 있음**(값은 본 문서에 옮기지 않음) | 확인 |
| 개인정보 | Oracle 쿼리에는 운전자 식별자 없음. 그러나 **GPS 피드의 `userid`(운전자 ID+전화번호)가 마스킹 없이 `/api/livemap/positions`로 나가고 화면에 표시됨**. DB 영속 저장은 미확인 | 확인 |
| 백업·복구 | **절차 없음**(`pg_dump` 0건). 학습 산출물 `data/travel_gbm.pkl`은 `.gitignore` 제외 → 호스트 손실 시 수개월 학습분 소멸 | 확인 |
| 형상관리 | 마이그레이션 적용기 없음(`sqlx::migrate!` 0건), 이력 테이블 없음, **0098 번호 중복 2건** | 확인 |
| **배포 자산 누락** | **`wp-api.service`·`wp-etw-bridge.service`가 호스트에만 존재하고 저장소에 없음**. crontab 2건(매시 도로망 재추론, 15분 GBM)도 저장소에 없음 **[호스트]** | **상충** |
| **문서-실제 불일치** | `deploy/systemd/README.md`가 enable 대상으로 안내하는 `wp-tick-t1/t2`는 **호스트에 설치조차 되어 있지 않고**, 실제 가동 중인 16개 타이머는 문서에 없음 **[호스트]** | **상충** |
| Fallback | 현재는 그림자라 시스템이 멈춰도 TOS가 그대로 배차. **코드 레벨 자동 강등·폴백 로직은 없음**(kc launch-plan C1 미착수) | 확인 |

---

## 8. 공통 데이터 게이트웨이 연계 Gap

상세는 `06_service2_handoff_for_data_gateway.md` 참조.

| # | Gap | 내용 |
|---|---|---|
| G1 | **직접 연결** | Service 2는 공통 CDC/Kafka/저장소를 쓰지 않고 Oracle을 **직접 폴링**한다. 게이트웨이 전환 시 25개 SQL 문장과 워터마크 로직 전체가 대상 |
| G2 | **저지연 경로 보존** | 위치 데이터는 폴링이 아니라 **상시 웹소켓 스트림**이고 STALE 120초 임계로 소비된다. 게이트웨이가 이 경로를 배치화하면 배차 기능이 성립하지 않는다 |
| G3 | **제3의 TOS 경로** | Azure `tos_etw_gateway` HTTP REST가 이미 존재한다. 발주서의 '공통 게이트웨이'와 동일 대상인지 **미확인 (P0-10)** |
| G4 | **스냅샷 vs 이벤트** | 현재 `live_*`는 전량 교체 스냅샷이라 이력이 없다. 게이트웨이가 이벤트를 준다면 재구성 방식이 바뀌고, 반대로 스냅샷만 준다면 지금 한계가 그대로 남는다 |
| G5 | **읽기 전용 증빙** | 읽기 전용이 저장소 밖 스크립트·계정 권한에 있어 감사 증빙을 제시할 수 없다 **(P0-3)** |
| G6 | **계약 산출물 부재** | OpenAPI/스키마 0건. 공유·분리할 계약을 정의하려면 Rust 구조체와 프론트 타입을 역공학해야 한다 |
| G7 | **출력 계약 미정** | Service 2 결과를 외부로 내보내는 계약이 없다. 대상 식별자·멱등성 키·Ack 개념이 전부 미정의 **(P0-2)** |
| G8 | **공통 키의 함정** | `queuename`이 선박·항차 간 재사용되어 조인 시 fan-out 위험이 있고(그래서 Oracle 조인을 회피), 시각 컬럼이 VARCHAR MYT 조합이다 |
| G9 | **원천 보존 한계** | TOS 보존이 ~15일/≥35일이라 게이트웨이가 생겨도 과거 백필은 불가능하다. 축적만이 유일한 방법 |

---

## 9. P0 질문 (견적·범위·안전 연계에 필수)

전체 목록과 P1·P2는 `05_service2_open_questions.md` 참조.

| # | 질문 | 확인 주체 | 미확인 시 영향 |
|---|---|---|---|
| P0-1 | TOS 측에 배차 권고를 소비할 인터페이스(테이블/API/메시지)가 존재하는가. 스키마·주기·인증·승인 절차는? | PM / 개발·운영 | Live 승격 공수의 최대 미지수 |
| P0-2 | TOS가 요구하는 배차 대상 식별자는 무엇인가(작업지시 ID/MSNSEQ/컨테이너번호)? | Service 2 담당 PM | 현재 산출물은 버킷 단위 → 매칭 입력·스키마 재설계 여부가 갈림 |
| P0-3 | `remote-toolbox-sql`(저장소 밖)의 접속 계정·권한이 읽기 전용으로 강제되는가. 이관 범위에 포함되는가? | 개발·운영 / 보안 | 보안 감사 증빙 불가, 운영 개시 자체가 불가능할 수 있음 |
| P0-4 | 운영에서 Stage-2 권고를 배차 담당자가 실제로 참고하는 절차가 있는가? | Service 2 담당 PM | "Recommendation 운영"인지 "미사용 Shadow"인지에 따라 다음 단계 범위가 달라짐 |
| P0-5 | 우리 시스템 정지 시 TOS 기본 배차로 자동 복귀됨이 TOS 측에서 보장되는가? | PM / 개발·운영 | 폴백 계약 없이는 부분 적용 안전 승인 불가 |
| P0-6 | 효과 판정 기준을 고객 세션의 수치 목표(공차거리 −8%/−15% 등)로 할 것인가, kc의 정성 게이트로 할 것인가? | Service 2 담당 PM | 판정 기준 이원화 상태로는 Go/No-Go 불가 |
| P0-7 | 최적매칭 이득의 공식 수치·정의·측정창을 하나로 확정(40% vs −5.1%) | 당사 | 외부 인용 시 신뢰도 리스크 |
| P0-8 | TT 총 대수·교대당 가동 대수의 권위 있는 값 | Service 2 담당 PM | 규모 변수 전체가 흔들림 |
| P0-9 | 현장 이벤트 → TOS Oracle 반영 지연 실측 | 개발·운영 | "실시간" SLA 약속 불가 |
| P0-10 | Azure `tos_etw_gateway`의 운영 주체·감싸는 TOS RPC·인증·SLA. 발주서의 '공통 게이트웨이'와 동일 대상인가? | Service 2 담당 PM | 게이트웨이 범위 산정의 전제 |
| P0-11 | `wp-api.service`·`wp-etw-bridge.service`와 crontab 2건을 저장소로 형상관리할 수 있는가? | 개발·운영 | 이관·재해복구 절차가 성립하지 않음 |
| P0-12 | GPS 피드 `userid`(운전자 ID+전화번호) 무마스킹 노출에 대한 개인정보 처리 정책 | PM / 보안 | 보존·마스킹 요건 추가 시 스키마·API 수정 필요 |

---

## 10. 수행 후보사 요청사항 대응

| 요청사항 | Service 2 조사 결과 | 상태 | 근거 | 추가 확인 주체 | 견적 영향 |
|---|---|---|---|---|---|
| 현재 PoC 아키텍처와 운영 상태 | 단일 호스트 systemd 기반. Rust 3바이너리 + React + Postgres. **상시 Shadow 운영**(wp-api enabled+active). CI/CD·컨테이너·k8s 없음 | 확인 | `Cargo.toml:1-3`, `deploy/systemd/README.md:19-38`, **[호스트]** | 개발·운영 | 컨테이너화·CI/CD·이관 자산 정비가 별도 범위 |
| 역할·데이터 입출력 | 입력 = Oracle 13객체 + ETW HTTP + GPS/PLC 웹소켓 + 날씨 2종. 출력 = Postgres 그림자 테이블 + 사내 GET API 31개. **외부 송출 0건** | 확인 | §5, `crates/api/src/main.rs:45-75` | — | 출력 채널이 전부 신규 |
| 적용 일정 | 고객 세션 자료의 UAT 8/3·1단계 8/17·2단계 10/30 **[문서]** 와 현재 준비도(Phase 0, A1/A2/D2 미착수)가 어긋남 | **상충** | `docs/2026-06-08-tt-assignment-customer-session-ko.html`, `kc/start/launch-plan.html` | **Service 2 담당 PM 확인 필요** | 일정 재협의 없이는 견적 불가 |
| Oracle/TOS 연계 | 외부 CLI 폴링(60초~4시간), TOSADM 13객체, 전부 SELECT, **쓰기 0건**. 읽기전용 강제는 저장소 밖 | 확인 / 일부 **미확인** | §4.3, `crates/extractor/src/runner.rs:41-72` | 개발·운영·보안 (P0-3) | 접속 계층이 이관 범위인지에 따라 크게 달라짐 |
| 기존 CDC·Kafka 사용 | **사용하지 않음.** Debezium/Kafka/커넥터 0건 | 확인 | 전수 검색 무히트 | — | 게이트웨이 전환은 신규 구축 |
| 대상 Table/View 수 | Oracle **13개 객체**(용도별로는 25개 SQL 문장). + HTTP 게이트웨이 1종 + 외부 API 2종 | 확인 | §4.3, 03번 문서 | — | 지식센터 추출 문서는 18개만 기재 → **문서 갱신 필요** |
| 위치·상태 Event 변경량 | 위치는 상시 스트림(TT p50 3초). **초당 메시지 수는 자료 간 2.5배 상충, 실측 없음** | **상충 / 미확인** | `kc/data/websocket-data.html`, `kc/data/websocket-coverage.html` | 당사(측정 가능) | 수집 사이징 근거 부재 |
| Match 처리량·지연 | 60초 주기, 쌍 수 ≲ 트럭수², 솔버 타임아웃 없음. **처리시간 실측·부하시험 0건** | 확인 / **미확인** | `crates/api/src/livemap.rs:4173-4182`, §6.2 | 당사 | 성능 검증 공수 별도 계상 |
| 유실·중복·순서 기준 | 워터마크 사전순 전진(롤백 불가), 당일 등가조건, `ON CONFLICT DO NOTHING`, 캡 무경보, 전량교체 스냅샷 | 확인 | §6.3 | 개발·운영 | 게이트웨이 전환 시 재설계 필요 |
| 장애·Fallback | 그림자라 현재는 무해. **자동 강등·폴백 코드 없음**, Oracle 미러 신선도 게이트 결손 | 확인 | §7, `crates/api/src/livemap.rs:4180-4182` | PM (P0-5) | Live 승격 시 전부 신규 |
| 모니터링·감사로그 | 헬스 3종·`etl_run_log`는 있으나 **push 알림 0**, Trace ID 없음, 백업 절차 없음 | 확인 | §7 | 개발·운영 | 비기능 구축분 별도 계상 |
| 보안·마스킹 | 인증 없음, CORS permissive, 무인증 킬스위치(8899), `.env` 0644, 스크립트·crontab 평문 비밀번호, **운전자 개인정보 무마스킹 노출** | 확인 | §7 | PM·보안 (P0-12) | 보안 하드닝이 별도 범위 |
| 기술 검증 필요 범위 | ①TOS 쓰기 인터페이스·식별자 ②종단 지연 실측 ③메시지율·행수·DB 증가율 실측 ④부하시험 ⑤버킷→개별 작업 단위 재설계 타당성 ⑥읽기전용 권한 증빙 | — | §6.2, §5.3 | 공동 | PoC 검증 항목의 뼈대 |

---

## 11. 견적 준비도 1차 판단

| 영역 | 준비도 | 판단 근거 |
|---|---|---|
| 알고리즘·계산 로직 | **높음** | 매칭 엔진이 구현·상시 가동 중이고 비용 모델·학습 파이프라인까지 작동. 상수·근거가 코드에 명시 |
| 데이터 입력 계약 | **중간** | 13개 Oracle 객체와 컬럼·키·시각 의미가 코드로 확인 가능. 다만 계약 문서(OpenAPI/스키마) 0건이고 지식센터 추출 문서가 실제보다 좁음 |
| **출력·TOS 연동** | **낮음** | 인터페이스 미정, 대상 식별자 부재, Ack·멱등성 개념 없음, 운영자 UI 없음. **0에서 시작** |
| 규모·성능 산정 | **낮음** | 장비 대수·메시지율·종단 지연·DB 증가율 어느 것도 실측이 없고 문서 값끼리 상충. 부하시험 0건 |
| 비기능(운영·보안) | **낮음** | 알림·재시도·백업·롤백·인증·마스킹이 사실상 미착수. 프로젝트 스스로 미착수로 기록 |
| 배포·이관 자산 | **낮음** | 핵심 서비스 유닛 2개와 cron 2건이 형상관리 밖. CI/CD·마이그레이션 적용기·백업 절차 없음 |
| 효과 근거 | **중간~낮음** | 비교 파이프라인(fair_compare)은 구현되어 있으나 **수치가 자료마다 8배 차이**나고 판정 기준이 이원화 |

**종합 판단.** 알고리즘 검증에 필요한 자료는 충분하나, **견적을 확정하기에는 부족하다.**
견적을 좌우하는 세 축이 모두 미확정이다 — ①TOS 출력 인터페이스와 대상 식별자(P0-1·P0-2), ②규모·지연 실측치(P0-8·P0-9),
③효과 판정 기준과 공식 수치(P0-6·P0-7). 이 3축이 확정되기 전의 견적은 범위 가정에 따라 크게 흔들린다.

**우선 착수 권고(당사 내부 관점, 범위 결정은 PM 소관).**
①은 조직 합의가 필요하므로 가장 먼저 착수해야 하고, ②는 이미 코드에 카운터가 있어 며칠 로깅만으로 확정 가능하며,
③은 지표 정의를 하나로 고정한 뒤 동일 창으로 재측정하면 된다.

---

## 12. 조사 방법과 한계

- **방법**: 8개 트랙(런타임·매칭·Oracle 입력·위치·출력·운영보안·데이터량·문서) 병렬 정적 분석 →
  트랙별 핵심 주장 16건에 대해 서로 다른 렌즈(근거 실재성 / 해석 과장)의 검증자 32명이 적대적 반박 →
  완결성 비평 1회 → 호스트 systemd·crontab 읽기 전용 관찰. 총 41개 조사 단위.
- **적대검증의 효과**: 초기 주장 다수가 줄 번호 오류·개수 오류·"코드 존재를 운영 활성으로 넘겨짚기"로 반박되어 정정되었다.
  본 문서의 수치·인용은 정정본 기준이다.
- **한계 1 — 저장소 밖**: `remote-toolbox-sql` 스크립트, Oracle 계정 권한, TOS 내부 스키마·알고리즘, ETW 게이트웨이 내부는 확인할 수 없었다.
- **한계 2 — 호스트 관찰**: `wp-api` 가동·타이머 활성·crontab은 조사 시점 1회 관찰이다. 지속 가동률·가동 이력은 확인하지 않았다.
- **한계 3 — 실행 미수행**: 빌드·테스트·쿼리·부하시험을 일절 실행하지 않았다. 성능·행수·지연 수치는 모두 "측정 필요"로 남겼다.
- **한계 4 — 문서 스냅샷**: `kc/`의 라이브 수치(절감률, 실현가능성 등)는 정적 HTML에 하드코딩된 과거 스냅샷이며 현재값 검증이 불가능하다.

---

## 부록. 함께 보는 문서

| 파일 | 내용 |
|---|---|
| `02_service2_architecture_and_runtime.md` | 모듈·실행 구조, 배포 대조표, 흐름, Mermaid 아키텍처 |
| `03_service2_data_contract_inventory.md` | 데이터 계약 전수 인벤토리(입력/내부상태/출력/피드백) |
| `04_service2_matching_logic_fact_sheet.md` | 매칭 로직 현재 사실표(후보풀·제약·비용·상수·중복방지) |
| `05_service2_open_questions.md` | 확인 주체별 질문 목록(P0/P1/P2) |
| `06_service2_handoff_for_data_gateway.md` | 중앙 데이터 게이트웨이 조사팀 인계 요약 |
| `service2_data_inventory.csv` | 데이터 항목 인벤토리(기계 판독용) |
| `service2_evidence_register.csv` | 근거 대장 — 조사 findings 전수 |
