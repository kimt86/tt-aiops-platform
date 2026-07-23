# 03. Service 2 데이터 계약 인벤토리

## 1. 문서 정보

| 항목 | 내용 |
|---|---|
| 문서 목적 | Service 2(TT Assignment)가 **읽는·만드는·내보내는 모든 데이터 항목**을 단일 표로 정리한다. 발주자·수행 후보사가 인터페이스 범위와 데이터 리스크를 계약 전에 판단할 수 있게 하는 것이 목적이다. |
| 조사 대상 | 저장소 `tt-aiops-platform` 단일 리포지터리 (브랜치 `scengen-collector`, **HEAD `10cc8c0`**) |
| 근거 기준 커밋 | 이하 모든 저장소 근거는 `10cc8c0` 기준. 개별 근거에는 커밋을 반복 표기하지 않는다. |
| 조사 시점 | 2026-07-22 |
| 주의 | 조사 시점 워킹트리에 미커밋 변경 5건(`crates/api/src/cycles.rs`, `scripts/populate_tt_cycle_recon.sql`, `web/public/livemap-roadgraph.geojson`, `web/src/CyclesPage.tsx`, `web/src/api.ts`)이 있었다. `scripts/populate_tt_cycle_recon.sql`은 systemd 유닛이 **파일 경로 그대로 실행**하므로, 미커밋 SQL이 이미 운영 타이머에서 돌고 있을 수 있다. |

### 1.1 범례 — 근거 유형

| 표기 | 의미 |
|---|---|
| (표기 없음) | **저장소 파일 근거.** `경로:줄범위` 형식. 코드/마이그레이션/유닛 파일에서 직접 확인. |
| **[호스트]** | 2026-07-22 조사용 호스트에서 읽기 전용 `systemctl --user` / `crontab -l` 관찰. 저장소 밖 근거이므로 재확인 권장. |
| **[문서]** | `kc/` 또는 `docs/`의 문서 주장. 코드 확인과 구분해서 읽어야 한다. |

### 1.2 범례 — 상태

| 상태 | 의미 |
|---|---|
| **확인** | 근거 파일에서 직접 확인된 사실. |
| **추정** | 정황·주석·간접 근거에 기반. 원 근거로 검증되지 않음. |
| **미확인** | 저장소·호스트 관찰로 확인할 수 없음. 담당자 확인 필요. |
| **상충** | 둘 이상의 근거가 서로 다른 사실을 말함. |

### 1.3 이 문서를 읽는 원칙

- **코드에 존재하는 것**과 **운영에서 활성화된 것**은 항상 분리해 적었다. 유닛 파일이 저장소에 있다는 사실은 그 유닛이 운영 호스트에서 켜져 있다는 뜻이 아니다.
- 없는 것은 "없음(검색 근거 명시)"으로 적었다. 측정되지 않은 수치는 추정하지 않고 "실측 없음"으로 남겼다.
- 비밀정보(비밀번호·토큰·키 값·내부 호스트/IP·계정명)는 **값을 옮기지 않았다.** 환경변수 **키 이름**까지만 적고 값은 `<redacted>`로 표기한다. 평문 비밀번호가 어느 파일에 존재한다는 **사실**만 기술한다.

---

## 2. 한눈 요약

| 항목 | 값 | 상태 | 근거 |
|---|---|---|---|
| 소스 시스템 수 | **6** (Oracle TOSADM / Azure ETW 게이트웨이 / GPS·PLC 웹소켓 / Open-Meteo / Tomorrow.io / 로컬 Postgres) | 확인 | §3 각 행 |
| Oracle TOSADM 객체 수 | **13** | 확인 | `crates/extractor/sql/` 16파일 + `crates/scengen/src/*.rs` 인라인 |
| Oracle 대상 SQL 문장 수 | **25** = SQL 파일 **16** + 인라인 **9**(extractor 3 + scengen 6). 전부 SELECT/WITH | 확인 | `crates/extractor/sql/` (16파일), `crates/extractor/src/{handover.rs:57, rtg_moves.rs:53, qc_moves.rs:53}`, `crates/scengen/src/{collect.rs:70, snapshot.rs:60, yard.rs:49, enrich.rs:64·108·119}` |
| Oracle 대상 쓰기(DML/PL-SQL) | **0건** — 단, 코드로 강제되지 않음(§10 R-10) | 확인(0건) / 미확인(강제 여부) | `crates/extractor/src/runner.rs:41-72`, `crates/scengen/src/toolbox.rs:44-73` |
| 비-Oracle 외부 입력 | **3종** ① Azure `tos_etw_gateway` HTTP ② GPS/PLC 웹소켓 ③ 외부 기상 API 2개(Open-Meteo·Tomorrow.io) | 확인 | `crates/extractor/src/workpool.rs:111-170`, `crates/api/src/livemap.rs:3005-3092`, `crates/extractor/src/weather.rs:1-40·79-83` |
| 배차 산출물(출력) 테이블 | **2** (`stage2_match_shadow`, `stage2_solver_shadow`) | 확인 | `crates/api/src/livemap.rs:4466`, `4478` |
| 결과 피드백 테이블 | **5** (`dispatch_compare_shadow`, `fair_compare_shadow`, `fair_compare_detail`, `dispatch_pred_sample`, `mm_arrival_shadow`) | 확인 | `crates/api/src/livemap.rs:4608·4983·4996`, `crates/api/src/workpool.rs:731`, `db/migrations/0090_mm_arrival_shadow.sql:1-19` |
| 외부 시스템으로의 출력(TOS write-back, DigiPort, webhook/kafka/mqtt/S3/CSV) | **0건** | 확인 | 전 저장소 검색 무히트(트랙 output OP-16) |
| API 계약 산출물(OpenAPI/AsyncAPI/JSON Schema) | **0건** | 확인 | §13 |
| 대시보드 API 라우트 | **31개 전부 GET**, 인증·인가 계층 없음 | 확인 | `crates/api/src/main.rs:45-75` |

> **가장 중요한 계약상의 사실 3가지**
> 1. 이 시스템은 TOS를 **읽기만** 한다. TOS로 배차를 되돌리는 코드 경로는 저장소에 존재하지 않는다(§7).
> 2. 배차 산출물 `stage2_match_shadow`에는 **컨테이너번호·작업지시 ID(MSNSEQ)가 없다.** 행은 (QC, 선박, 큐, 작업유형, 출발블록) **버킷 단위**라 현재 형태로는 TOS에 "어느 작업지시"인지 지목할 수 없다(§7, §10 R-13).
> 3. 이 시스템과 TOS 사이에 **문서화된 데이터 계약(스키마 산출물)이 존재하지 않는다.** 계약은 Rust 구조체와 `web/src` 타입에만 암묵적으로 존재한다(§13).

---

## 3. 소스 시스템 개요

| 시스템 | 접근 방식 | 게이트웨이 / 경로 | 주기 | 인증 방식(키 이름만) | 상태 | 근거 |
|---|---|---|---|---|---|---|
| **Oracle TOSADM** (운영 TOS DB) | 폴링(Polling). CDC/Kafka/Debezium/JDBC 직결 **없음** | 외부 CLI `remote-toolbox-sql`을 자식 프로세스로 실행, SQL은 임시파일(`--file`)로 전달. 게이트웨이가 **2개**(코드 공유 없음): extractor 경로(타임아웃 90초 하드코딩, 프로세스 전역 `ORACLE_LOCK`) / scengen 경로(타임아웃 = `scenario.config.oracle_timeout_s`, 기본 45초) | 60초 ~ 4시간 + 야간 1회 (§3.1) | 저장소에 자격증명 없음. 스크립트 경로만 환경변수 `SKILL_DIR`(값 `<redacted>`)로 지정. **접속 계정·권한은 저장소 밖** | 확인(경로) / 미확인(계정·권한) | `crates/extractor/src/runner.rs:41-72`, `crates/scengen/src/toolbox.rs:44-73`, `db/migrations/0093_scenario.sql:19` |
| **Azure `tos_etw_gateway`** (TOS RPC를 감싼 HTTP REST) | HTTP GET, `curl -m 8` 자식 프로세스 | `GET /v1/voyages/{vessel}/{voyage}/snapshot`. **`wp-etw-bridge` SSH 터널** 경유 | 90초 (workpool 틱 내 5번째 단계) | 환경변수 `ETW_GATEWAY_URL` (값 `<redacted>`). 별도 토큰·서명 헤더 없음 | 확인 | `crates/extractor/src/workpool.rs:111-170` |
| **GPS / PLC 웹소켓 피드** | 상시 스트리밍 구독(푸시) | API 프로세스가 로컬 루프백 터널(포트 9986)의 `ws://`에 직접 접속. 존 2개: `wpt_gps`(장비 GPS), `ctab`(크레인 PLC). 터널은 `wp-ws-bridge` SSH 유닛 | 이벤트 스트림 (TT 중앙 3초, RTG 중앙 60초, PLC 약 1초 [문서]) | 환경변수 `LIVEMAP_IDENTIFY` / `LIVEMAP_USERNAME` / `LIVEMAP_USER` — **소스에 기본값 상수가 박혀 있음**(값은 `<redacted>`). TLS 없음(로컬 터널 신뢰) | 확인 | `crates/api/src/livemap.rs:3005-3092`, `3094-3170`, `deploy/systemd/wp-ws-bridge.service` |
| **Open-Meteo** (공용 인터넷) | HTTP GET, `curl` | `api.open-meteo.com/v1/forecast`, 터미널 고정 좌표 상수 | 1시간 | **키 불필요** | 확인 | `crates/extractor/src/weather.rs:1-40` |
| **Tomorrow.io** (공용 인터넷) | HTTP GET, `curl` | `/v4/timelines` | 3분 | 환경변수 `TOMORROW_API_KEY` (값 `<redacted>`). 무료 쿼터 의존 | 확인 | `crates/extractor/src/weather.rs:79-83` |
| **로컬 Postgres** (우리 시스템 상태 저장소) | sqlx 직결 | 커넥션 풀 상한: API 8 / extractor 4 / scengen 2 | 상시 | 환경변수 `DATABASE_URL` (값 `<redacted>`) | 확인 | `crates/api/src/db.rs:1-13`, `crates/extractor/src/db.rs:1-16`, `crates/scengen/src/db.rs:1-14` |

### 3.1 운영에서 실제로 도는 수집 스케줄 [호스트]

호스트에서 관찰된 **enabled 타이머는 16개**다. 저장소 유닛 파일 집합(서비스 20 + 타이머 18)과 일치하지 않는다.

| 유닛 | 주기 | Oracle 접촉 | 비고 |
|---|---|---|---|
| `wp-handover` | 60초 | O | JOB_ORDER_HISTORY 증분, cap 3000 |
| `wp-workpool` | 90초 | O + ETW HTTP | **행수 캡 없는 유일한 Oracle 쿼리** |
| `wp-shift-t1` | 3분 | O | |
| `wp-weather-live` | 3분 | X | Tomorrow.io |
| `wp-qc-moves` / `wp-rtg-moves` | 각 5분(90초 오프셋) | O | MCH_OPERATION 증분, cap 5000 |
| `wp-tt-move-log` | 5분 | X | psql, Postgres 전용 |
| `wp-scenario-yard` | 5분 | O | cap 8000 |
| `wp-tt-cycle-recon` | 10분 | X | psql |
| `wp-scenario-collect` / `wp-scenario-yard-build` | 각 10분 | O / X | |
| `wp-shift-t2` | 15분 | O | |
| `wp-scenario-enrich` | 15분 | O | cap 20000 |
| `wp-weather` | 1시간 | X | Open-Meteo |
| `wp-scenario-snapshot` | 4시간 | O | CYY_CONTAINER |
| `wp-nightly` | 매일 01:30 | O | 전체 추출 |

**형상관리 밖에 있는 것 — 이관 시 반드시 확보해야 함**

| 항목 | 사실 | 상태 | 근거 |
|---|---|---|---|
| `wp-api.service` | 배차 그림자 매칭·GPS 수집·24개 백그라운드 태스크를 모두 돌리는 **핵심 프로세스**의 유닛 정의가 저장소에 없다. 호스트에는 존재하며 enabled + active(Restart=always, RestartSec=3) | 상충(저장소 부재 / 호스트 존재) | `deploy/systemd/` 목록에 없음 · [호스트] · [문서] `kc/reference/references.html:32` |
| `wp-etw-bridge.service` | ETW 게이트웨이용 SSH 터널 유닛이 저장소에 없다. 코드 주석은 이 터널을 전제 | 상충 | `crates/extractor/src/workpool.rs:114-116`, `deploy/systemd/` 목록에 없음 |
| crontab 2건 | 매시 11분 `scripts/reinfer_roadgraph.sh`(**배차 비용의 본체인 도로망 재추론**), 15분마다 `scripts/travel_gbm_shadow.py`. 저장소에 스케줄 정의 없음. **crontab 라인에 평문 DB 비밀번호 포함**(값 미기재) | 확인 | [호스트] `crontab -l` |
| `wp-tick-t1`, `wp-tick-t2` | 저장소에는 있으나 **호스트에 미설치**. 하필 `deploy/systemd/README.md`가 enable 대상으로 안내하는 5개 중 2개 | 상충 | `deploy/systemd/README.md:27,30` · [호스트] |

---

## 4. 입력 — Oracle TOSADM 13개 객체 (용도별 25행)

> 같은 테이블을 여러 용도로 읽는 경우 **용도별로 행을 나눴다.** 시각 컬럼은 대부분 VARCHAR `YYYYMMDD` + `HH24MISS`(터미널 현지시각 MYT = UTC+8) 조합이며, 문자열 사전순으로 인덱스 범위 스캔한다.

### 4.1 JOB_ORDER_LIST — 2용도

| # | 데이터 항목 | Source | Table/View/API/Topic | 주요 Column | Key | Timestamp(의미) | 갱신 방식·주기 | 사용 위치 | 품질 위험 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| O-01 | 라이브 작업풀 (진행중 A + 미배차 Q) | Oracle TOSADM | `JOB_ORDER_LIST` | JOB_ODR_QUEUENAME / VESSEL / VOYAGE / JOBTYPE / JOBSTATUS / YT_STATUS / YTNO / ARMGC / ETW_DT / ACTV_DT / CONTNO / MSNSEQ / YT_TOPOS / TWINTANDEM / TWINKEY, CRNT_PSN_IDX_NO1, YT_TO_PSN_IDX_NO1, UPD_DT, CRE_DT, COMPDATE | 업무키 (QUEUENAME, VESSEL, CONTNO, MSNSEQ) / 조인키 CONTNO·YTNO·QUEUENAME | `ETW_DT`=예상작업시각(**계획**), `ACTV_DT`=작업 활성화(**발생**), `UPD_DT`=행 최종수정(**반영**, 배차시각 D_tos의 근사), `CRE_DT`=생성 | 폴링 90초 (`wp-workpool.timer`). 매 tick DELETE 후 전량 재삽입 | `live_workpool` / `live_candidate` → Stage-1 수요·Stage-2 후보작업 | ① JOBSTATUS 코드 C/A/Q/P/B 중 **A·Q만 취득** → P(계획)·B(블록)는 통째 누락 ② `CRE_DT >= TRUNC(SYSDATE)-2` 2일 창 밖 미완료 행 누락 ③ **행수 캡 없음** ④ `TWINKEY`는 조회하지만 `live_workpool`에 저장 안 됨 ⑤ `UPD_DT`≈배차시각 근사의 오차 미측정 | 확인 | `crates/extractor/sql/workpool.sql:14-38`, `crates/extractor/src/workpool.rs:262-345`, `282-298`, `deploy/systemd/wp-workpool.timer:5-7` |
| O-02 | 배차된 TT 전종 (가동률 분모) | Oracle TOSADM | `JOB_ORDER_LIST` | JOB_ODR_YTNO, JOB_ODR_JOBSTATUS, JOB_ODR_COMPDATE, CRE_DT | YTNO (DISTINCT) | `CRE_DT >= TRUNC(SYSDATE)-2` (**생성시각** 기준 창) | 폴링 90초 (`wp-workpool.timer`). 전량 교체 | `live_assigned_tt` (가동률 분자/분모) | 작업이 없는 유휴 트럭은 TOS에 나타나지 않음 → **유휴 여유분 관측 불가**(SQL 주석에 명시) | 확인 | `crates/extractor/sql/assigned_tt.sql:7-13`, `crates/extractor/src/workpool.rs:186-190` |

### 4.2 JOB_ORDER_HISTORY — 6용도

> 트랙 조사의 `rg` 참조 집계는 extractor SQL 기준 **7회**이나, 인벤토리에서 식별된 **용도는 6종**이다. 차이는 동일 파일 내 복수 참조로 보인다(추정).

| # | 데이터 항목 | Source | Table | 주요 Column | Key | Timestamp(의미) | 갱신 방식·주기 | 사용 위치 | 품질 위험 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| O-03 | **완료 핸드오버 라벨** (트럭이 자유로워진 시각 = 배차 학습의 정답지) | Oracle TOSADM | `JOB_ORDER_HISTORY` | JOB_HIST_YTNO / ARMGC / JOBTYPE / CONTNO / POINT / SEQNO / DATE / TIME / ACTV_DT / YT_TOPOS, YT_DIS_DT, JOB_HIST_JOBSTATUS | (CONTNO, POINT, SEQNO) | `JOB_HIST_DATE‖TIME`=완료 **이벤트(발생)**, `YT_DIS_DT`=배차(발생), `ACTV_DT`=활성화(발생) | 폴링 60초 · **워터마크 증분** (`wp-handover.timer`) · FETCH_CAP 3000 | `tos_handover_label` → 곧빔 정답지·`tt_move_log` 스파인·자가보정 | ① 상한 시각 조건이 없어 워터마크가 밀리면 스캔 범위가 넓어짐 ② JOBSTATUS='C'만 취득 ③ **원천 보존 ~15일** → 깊은 백필 불가 ④ 캡 도달 경보 없음 | 확인 | `crates/extractor/src/handover.rs:43-68`, `82-87`, `106-120` |
| O-04 | K_RTG_Q (크레인 대기, 일별 + 시간별) | Oracle TOSADM | `JOB_ORDER_HISTORY` | JOB_HIST_DATE / JOBTYPE / ARMGC / VESSEL / VOYAGE / TIME, YT_DIS_DT, JOB_HIST_ACTV_DT | 집계 (work_date × jobtype), 시간별은 `SUBSTR(TIME,1,2)` | `crane_q = ACTV_DT − YT_DIS_DT`, 0~1800초만 유효 | 폴링: 야간 1회 + tick-t2 20분 + shift-t2 15분 | `raw_k_crane_q` / `raw_k_crane_q_hour` → K_RTG_Q | 음수·30분 초과 이상치를 별도 카운트할 만큼 흔함. 오래된 날짜는 YT_DIS_DT/ACTV_DT가 희소해 0행 | 확인 | `crates/extractor/sql/c08_k_crane_q.sql:5-35`, `e5_k_crane_q_by_hour.sql:4-36`, [문서] `README.md:83` |
| O-05 | K_EMPTY (공차 이동거리 분해) | Oracle TOSADM | `JOB_ORDER_HISTORY` | JOB_HIST_DATE / JOBTYPE / CONTNO / POINT / SEQNO / TIME, CRNT_PSN_IDX_NO1, LNDN_TRV_RNG, UN_LNDN_TRV_RNG | (DATE, JOBTYPE, CONTNO, POINT, SEQNO) | `JOB_HIST_TIME` 첫 이벤트로 교대(Night/Day/Evening) 판정 | 폴링: 야간 + tick-t2 20분 + shift-t2 15분 (~240K행 PK 범위스캔) | `raw_k_empty` → K_EMPTY / K_EMPTY_R | 거리 0~5000m 밖 통째 제외 + `HAVING ≥50건` 필터 → **표본 적은 조합이 사라짐** | 확인 | `crates/extractor/sql/e4_k_empty_decomposition.sql:4-39` |
| O-06 | K_CYCLE (컨테이너 처리 스팬) | Oracle TOSADM | `JOB_ORDER_HISTORY` | JOB_HIST_JOBTYPE / CONTNO / POINT / SEQNO / DATE / TIME | (JOBTYPE, CONTNO, POINT, SEQNO) | `MIN~MAX(DATE‖TIME)` 차이 = cycle_sec | 폴링: 야간 + tick-t2 + shift-t2 | `raw_k_cycle` | **이 값은 TT 싸이클이 아니라 컨테이너 처리 스팬**(SQL 주석의 명시적 경고). transitions>1 건만, 평균+2SD 이상치 별도 카운트 | 확인 | `crates/extractor/sql/e3b_k_cycle_refined_v2.sql:4-53` |
| O-07 | 시나리오 무브 이력 (scengen) | Oracle TOSADM | `JOB_ORDER_HISTORY` | JOB_HIST_CONTNO / JOBTYPE / VESSEL / VOYAGE / ARMGC, `SUBSTR(JOB_HIST_YT_TOPOS,1,40)`, DATE‖TIME | (contno, comp_ts, jobtype) | `DATE‖TIME` 완료 이벤트(**발생**). 워터마크 하한 + now 상한 양쪽 존재 | 폴링 10분 · 워터마크 증분 (`wp-scenario-collect.timer`) · FETCH_CAP 5000 · 킬스위치 | `scenario.move_hist` → 시나리오/에뮬레이터 | JOBSTATUS='C'인 DS/LD만 → **야드 내부 이동(MI/MO/LC)은 시나리오에 없음** | 확인 | `crates/scengen/src/collect.rs:70-110` |
| O-08 | (동일 소스, 적재만 분리) 완료 무브 → Postgres 축적 | Oracle TOSADM | `JOB_ORDER_HISTORY` | 상동 | PK (JOB_HIST_DATE, JOB_HIST_TIME) 범위스캔 | 상동 | 상동 | 상동 | ON CONFLICT DO NOTHING → 동일 키 후속 갱신 무시 | 확인 | `crates/scengen/src/collect.rs:102-107` |

### 4.3 JOB_QUEUE_SCHEDULE — 1용도

| # | 데이터 항목 | Source | Table | 주요 Column | Key | Timestamp(의미) | 갱신 방식·주기 | 사용 위치 | 품질 위험 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| O-09 | QC 작업큐 계획·진행 | Oracle TOSADM | `JOB_QUEUE_SCHEDULE` | JOB_QUE_CRANENO / VESSEL / VOYAGE / QUEUENAME / DISLOAD / SEQ / TOTALQTY / COMPQTY / PLANQTY, DELT_FLG, UPD_DT | (qc, vessel, queuename) — Postgres PK 동일 | `UPD_DT`(**반영시각**). 1일 이내 + 미완료 또는 6시간 내 완료 | 폴링 90초 (`wp-workpool.timer`) | `live_workqueue` → work-ETA(순번·잔여), 긴급도 | ① `JOB_QUE_ACTIVEYN`이 실무상 NULL이라 필터로 못 씀 ② **queuename이 선박·항차 간 재사용**돼 조인 시 fan-out 위험(그래서 Oracle 조인 회피) ③ seq 재계획 시 원거리 ETA 신뢰도 저하(코드 주석) | 확인 | `crates/extractor/sql/workqueue.sql:7-23`, `crates/extractor/src/workpool.rs:198-220` |

### 4.4 MCH_OPERATION — 8용도

| # | 데이터 항목 | Source | Table | 주요 Column | Key | Timestamp(의미) | 갱신 방식·주기 | 사용 위치 | 품질 위험 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| O-10 | **QC 무브 스트림** (안벽 핸드오버) | Oracle TOSADM | `MCH_OPERATION` | MCH_OPER_MACHNO / CONTNO / SEQNO / JOBTYPE / COMPDATE / COMPTIME / STATUS, TRK_ID, ST_DT | (machno, contno, seqno) | `ST_DT`=무브 시작(발생), `COMPDATE‖COMPTIME`=완료(**물리 핸드오버 시점**) | 폴링 5분 · 워터마크 증분 (`wp-qc-moves.timer`) · FETCH_CAP 5000 | `qc_move_log` → `tt_move_log`·싸이클 정답지 | ① **`COMPDATE = 오늘` 등가조건** → 자정 넘긴 정지는 영구 복구 불가 ② STATUS F=Full/M=empty 코드값의 공식 정의 미확인 ③ **지식센터 추출 문서에 미기재** ④ 캡 도달 경보 없음 | 확인 | `crates/extractor/src/qc_moves.rs:41-64`, `82-87` |
| O-11 | **RTG/ES 무브 스트림** (야드 핸드오버) | Oracle TOSADM | `MCH_OPERATION` | 상동 + `MACHNO LIKE 'RTG%'/'ES%'` | (machno, contno, seqno) | `ST_DT` / `COMPDATE‖COMPTIME` | 폴링 5분 · 워터마크 증분 (`wp-rtg-moves.timer`) · FETCH_CAP 5000 | `rtg_move_log` → 야드크레인 백로그·대기예측 피처 | DS 핸드오버는 RTG 무브의 **~20%뿐**(주석) — 나머지는 리셔플/게이트. 당일 등가조건 동일 | 확인 | `crates/extractor/src/rtg_moves.rs:36-64` |
| O-12 | 야드 스택위치 디코드 (scengen) | Oracle TOSADM | `MCH_OPERATION` | MACHNO / CONTNO / SEQNO / JOBTYPE / COMPDATE / COMPTIME, CRNT_PSN_IDX_NO1~NO4 | (machno, contno, seqno) | `COMPDATE‖COMPTIME` | 폴링 5분 · 워터마크 증분 (`wp-scenario-yard.timer`) · FETCH_CAP 8000 · 킬스위치 | `scenario.yard_move` → `yard_cell` 재구성 | NO1=블록·NO2=베이·NO3=row(A=0)·NO4+1=tier 디코드 규칙의 근거가 **코드 주석(CYY.CLOCATION 대조)뿐**, TOS 사양서 미확인 | 확인(코드) / 미확인(사양) | `crates/scengen/src/yard.rs:49-63` |
| O-13 | K_MPH 실시간 (QC 생산성) | Oracle TOSADM | `MCH_OPERATION` | MCH_OPER_VESSEL / VOYAGE / MACHNO / JOBTYPE / COMPDATE / COMPTIME / CONTNO, TRK_ID | (vessel, voyage, qc_machno) 집계 | `COMPDATE` 등가 + 교대 창 BETWEEN | 폴링: shift-t1 3분 + tick-t1 5분 + 야간 | `raw_k_mph_realtime`, 선박 교대 패널 | QC 정규식 `^C[0-9]+$`만 → **M/Z 프리픽스 크레인 제외**. `qc_move_time`은 C/M/Z를 쓰므로 **기준 불일치** | 확인 | `crates/extractor/sql/c07_k_mph_realtime.sql:5-25` |
| O-14 | K_TT_CYCLE (TT 무브 간격) | Oracle TOSADM | `MCH_OPERATION` | TRK_ID, MCH_OPER_JOBTYPE, COMPDATE‖COMPTIME, MACHNO | TRK_ID별 연속 무브 간격 | 완료시각 간 LAG 차이, **120~1200초만 채택** | 폴링: shift-t1 3분 + tick-t1 5분 + 야간 | `raw_k_tt_cycle` → 프론트 K_CYCLE | 120~1200초 밖 싸이클을 잘라내 **중앙값이 실제보다 낮게 산출**됨(별도 조사에서 41% 과소 이슈로 기록) | 확인 | `crates/extractor/sql/c10_k_tt_cycle.sql:8-40` |
| O-15 | QC/YC 가동률 (구간 병합) | Oracle TOSADM | `MCH_OPERATION` | MACHNO, ST_DT, COMPDATE‖COMPTIME | machno별 병합구간 | `ST_DT ~ 완료` 구간 병합 | 폴링: 야간 + shift tick (~90K행) | `raw_k_util_crane` | machine_type을 **정규식으로만 판정**(`^C`=QC, `RTG%`=YC) | 확인 | `crates/extractor/sql/e1c_k_util_crane_merged_intervals.sql:5-51` |
| O-16 | K_QC_Q (QC 유휴·트럭대기) | Oracle TOSADM | `MCH_OPERATION` | MACHNO / VESSEL / VOYAGE / JOBTYPE / QUEUENAME, ST_DT, COMPDATE‖COMPTIME | (qc, vessel, voyage) 병합구간 사이 갭 | 갭 = 다음 시작 − 이전 종료 | 폴링: shift-t1 3분(HAVING≥2) + 야간(HAVING≥10) | `raw_k_qc_q` → K_QC_NOMOVE / K_QC_TT_WAIT | 같은 QUEUENAME 유지 여부로 '진짜 트럭대기'를 근사 → **베이 변경 판정이 큐명 재사용에 취약** | 확인 | `crates/extractor/sql/f2_k_qc_q.sql:6-69` |
| O-17 | **QC 처리 cadence 학습** (배차 ETA 입력) | Oracle TOSADM | `MCH_OPERATION` | MACHNO(`^[CMZ][0-9]+$`), JOBTYPE, COMPDATE‖COMPTIME | (qc, jobtype, shift) + ALL 버킷 | `SYSDATE−3 ~ SYSDATE` **롤링 3일창**(날짜 토큰 아님) | 폴링: 야간 `run --kpi all` 안에서만 **1일 1회** | `learn_qc_move_time` → **Stage-1 work-ETA** | ① 날짜 키 없이 롤링이라 매일 밤 교체 → **과거 시점 재현 불가** ② 표본<30 조합은 사라짐 ③ 하루 1회 갱신이라 당일 변화 미반영 | 확인 | `crates/extractor/sql/qc_move_time.sql:7-32` |

### 4.5 MCH_WORKTIME + MCH_WORKSTOP + CDY_MACHINE — 1용도(3객체)

| # | 데이터 항목 | Source | Table | 주요 Column | Key | Timestamp(의미) | 갱신 방식·주기 | 사용 위치 | 품질 위험 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| O-18 | TT 가동률 (오퍼레이터 세션) | Oracle TOSADM | `MCH_WORKTIME` / `MCH_WORKSTOP` / `CDY_MACHINE` | MCH_WORK_MACHNO / START_DT / END_DT / STARTDATE / ENDDATE, MCH_STOP_* 동일, CDY_MCHN_CODE, `CDY_MCHN_TYPE='YT'` | machno | 세션 시작/종료(**발생**). 창 밖은 GREATEST/LEAST로 클리핑 | 폴링: **야간 1회만**(일중 tick에서 의도적 제외) | `raw_k_util_tt` | ① 오퍼레이터 로그아웃 누락으로 세션 겹침(`logout_anomaly` 플래그 존재) → 구간 병합으로 보정 ② **현재 교대 K_UTIL은 Oracle 대신 Postgres `util_tt_sample`로 대체됨** — 이 경로는 야간 집계 전용 | 확인 | `crates/extractor/sql/e3a_k_util_tt_merged.sql:9-93`, `crates/extractor/src/shift.rs:127-145` |

### 4.6 VSS_STATISTICS — 1용도(SQL 2개)

| # | 데이터 항목 | Source | Table | 주요 Column | Key | Timestamp(의미) | 갱신 방식·주기 | 사용 위치 | 품질 위험 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| O-19 | 항차 공식 실적 / 계획 물량 | Oracle TOSADM | `VSS_STATISTICS` | VESSEL / VOYAGE / UP_DT / CONFIRM / STTCHK / VAN / TEU / MOVES / SIN_MOV / TWN_MOV / TND_MOV / GROSSTIME / NETTIME / ABERTHTIME / WORKQC / GQCR / NQCR / GBP / NBP | (vessel, voyage) | `VSS_STT_UP_DT` = 확정/**반영시각**, 30일 창 | 폴링: 야간(`k_mph_voyage`) + shift-t1 3분(`voyage_plan`) | `raw_k_mph_voyage`, 라이브 선박 진행바(planned_moves=VAN) | ① 진행 중 항차는 MOVES가 NULL이라 **VAN을 계획 분모로 대체 사용** ② 숫자 컬럼이 문자형이라 `TO_NUMBER … ON CONVERSION ERROR` 필요 | 확인 | `crates/extractor/sql/c06_k_mph_voyage.sql:5-28`, `voyage_plan.sql:5-14` |

### 4.7 VSB_VOYAGE — 2용도

| # | 데이터 항목 | Source | Table | 주요 Column | Key | Timestamp(의미) | 갱신 방식·주기 | 사용 위치 | 품질 위험 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| O-20 | **선박 일정·마감** (Stage-1 데드라인의 원천) | Oracle TOSADM | `VSB_VOYAGE` | VESSEL / VOYAGE / STATUS / BERTHNO / ESTBER / ESTWKC / ESTDEP / CUTOFF / ACTBER / ACTDEP / DISVAN / LOADVAN / CANCEL | (vessel, voyage) | `EST*`=**계획**, `ACT*`=**실적**, `CUTOFF`=반입마감. 모두 DATE+TIME 문자열 결합(MYT→UTC 변환) | 폴링 90초 (`wp-workpool.timer`). 창 = ESTDEP `SYSDATE−2 ~ +10일` | `live_vessel_schedule` → deadline/slack/work-ETA | ① **ESTWKC가 자주 비현실적**이라 '출항 0~6시간 전'일 때만 신뢰하는 가드 존재 ② 취소 항차 `NVL(CANCEL,'N')='N'` 필터 의존 | 확인 | `crates/extractor/sql/vessel_schedule.sql:6-22`, `crates/api/src/workpool.rs:416-424` |
| O-21 | 선석 위치 (scengen) | Oracle TOSADM | `VSB_VOYAGE` | BERTHSIDE, STARTPOS (선석 시작 미터) | (vessel, voyage) | 마스터성 (시각 없음) | 폴링 15분 · 항차당 1회 · 킬스위치 | `scenario.vessel_call.startpos_m` → 시뮬 선석 span | **TOS가 주는 유일한 "위치성" 데이터**(좌표는 아님). 값 결측 시 선석 span 미구성 | 확인 | `crates/scengen/src/enrich.rs:64-79`, `crates/scengen/src/assemble.rs:65-75` |

### 4.8 CDV_VESSEL / CYY_CONTAINER / ETV_BAPLIE_CONT / ETV_MOVINS_STOWAGE — 각 1용도

| # | 데이터 항목 | Source | Table | 주요 Column | Key | Timestamp(의미) | 갱신 방식·주기 | 사용 위치 | 품질 위험 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| O-22 | 선박 제원 | Oracle TOSADM | `CDV_VESSEL` | CDV_VSL_CODE / NAME / LENGTH / WIDTH / DRAFT / MAXTEU / TOTALBAY | CDV_VSL_CODE | **없음(마스터)** | 폴링 15분 · 신규 vessel/voyage **6건/tick 제한** · 킬스위치 | `scenario.vessel_call` (시뮬 선석 span) | ① WIDTH·MAXTEU가 실데이터에서 **빈 경우 있음**(별도 조사) ② 지식센터 추출 문서 미기재 | 확인 | `crates/scengen/src/enrich.rs:64-79` |
| O-23 | 현재 야드 재고 | Oracle TOSADM | `CYY_CONTAINER` | CYY_CONT_CLOCATION, CRNT_PSN_IDX_NO1, CYY_CONT_STATUS / CONTTYPE / ISO / DISCHPORT | 블록(CLOCATION 첫 토큰) 집계 ~285행 | **없음** — 캡처 시각(`snapshot_ts`)은 **우리 쪽 `Utc::now()`** | 폴링 4시간 (`wp-scenario-snapshot.timer`) · 킬스위치 | `scenario.yard_snapshot` / `yard_block` | ① **CYY는 TOS ETL마다 덮어쓰기** → 과거 시점 야드상태 소급 불가, 앞으로만 축적 ② `DISCHPORT='MYPKG'`로 수입 판정하는 **하드코딩** | 확인 | `crates/scengen/src/snapshot.rs:1-7`, `58-69` |
| O-24 | 양하 적부계획·컨 속성 | Oracle TOSADM | `ETV_BAPLIE_CONT` | CONTNO / CONTISO / FULLEMPT / CONTTYPE / CONTSTWG / GROSWGHT(MEAS_WGT) / TMPRCONT / DNGRIMDG / DNGRUNNO / DCHGPORT / ORGNPORT / CONTOPER / OVERHIGH / NEXTVESSEL / NEXTVOYAGE | (vessel, voyage, contno, disload='D') | **없음(계획 문서)** | 폴링 15분 · 항차당 1회 · `FETCH FIRST 20000` · 킬스위치 | `scenario.container` | 중량은 MEAS_WGT/GROSWGHT 중 비어있지 않은 쪽을 취하는 COALESCE → **두 값 불일치 시 추적 불가**. 지식센터 문서 미기재 | 확인 | `crates/scengen/src/enrich.rs:108-118` |
| O-25 | 적하 적부계획 | Oracle TOSADM | `ETV_MOVINS_STOWAGE` | CONTNO / CONTISO / FULLEMPT / CONTTYPE / CONTSTWG / GROSWGHT(MVNS_ORG_WGT) / TMPRCONT / DNGRIMDG / DNGRUNNO / DCHGPORT / LOADPORT / OPERATOR / OVERHIGH | (vessel, voyage, contno, disload='L') | **없음(계획 문서)** | 폴링 15분 · 항차당 1회 · `FETCH FIRST 20000` · 킬스위치 | `scenario.container` | MOVINS는 **출항 1~2일 전에야 생성**되므로 최근 항차만 채워짐. 지식센터 문서 미기재 | 확인 | `crates/scengen/src/enrich.rs:119-129` |

### 4.9 Oracle 입력에 대한 공통 사실

| 항목 | 내용 | 상태 | 근거 |
|---|---|---|---|
| SQL 조립 방식 | **바인드 변수 없이 문자열 리터럴 치환**(`params::render`). 미치환 토큰이 남으면 실패 처리. scengen enrich는 Oracle에서 읽은 vessel/voyage 값을 그대로 삽입 | 확인 | `crates/extractor/src/params.rs:11-13`, `51-63`, `crates/scengen/src/enrich.rs:76-77` |
| 증분 워터마크 | 문자열 사전순, `GREATEST`로만 전진 → **롤백 불가** | 확인 | `crates/extractor/src/handover.rs:106-120` |
| 캡(FETCH) | handover 3000 / qc·rtg moves 5000 / scengen collect 5000 · yard 8000 · enrich 20000. 집계 SQL은 FETCH FIRST 15~120행 | 확인 | 각 모듈 상수 |
| 캡 도달 경보 | **scengen만** `scenario.gen_event`에 기록. extractor(handover/qc/rtg)는 **알리지 않음** | 확인 | `crates/scengen/src/collect.rs:133-152`, `crates/extractor/src/rtg_moves.rs:19-20` |
| 개인정보 | Oracle 쿼리는 장비번호·컨테이너·선박/항차만 조회. **운전자 이름/사번 컬럼 미조회** | 확인 | `crates/extractor/sql/e3a_k_util_tt_merged.sql:15-29`, `crates/scengen/src/enrich.rs:124` |
| 프로세스 간 Oracle 직렬화 | 각 프로세스는 자체 async Mutex만 보유. **프로세스 간 직렬화는 저장소 밖 `remote-toolbox-sql`에 위임**한다는 주석뿐 | 미확인 | `crates/extractor/src/runner.rs:9-10`, `crates/scengen/src/toolbox.rs:1-13` |
| 원천 보존기간 | JOB_ORDER_HISTORY ~15일 / MCH_OPERATION·VSS ≥35일 | 확인(문서) / 미확인(DB 딕셔너리) | [문서] `README.md:82-84` |

---

## 5. 입력 — 비 Oracle 외부 소스

| # | 데이터 항목 | Source | Table/API/Topic | 주요 Column/Field | Key | Timestamp(의미) | 갱신 방식·주기 | 사용 위치 | 품질 위험 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| X-01 | **컨테이너별 정확 ETW** | Azure `tos_etw_gateway` (HTTP, Oracle 아님) | `GET /v1/voyages/{vessel}/{voyage}/snapshot` → `tos_etw_cntr` | cntr_no, dis_ld, qc_etw_utc, vessel_etw_utc, fetched_at_utc, expires_at_utc | (vessel, voyage, cntr_no) | `qc_etw_utc`/`vessel_etw_utc` = **예상 작업시각(계획)**, `fetched_at_utc`/`expires_at_utc` = **수집 신선도** | 폴링 90초. `live_workpool`에 있는 컨테이너만 필터. **2시간 미갱신 행 DELETE** | 작업 front 정렬(`etw_accurate` 우선), `dispatch_pred_sample` | ① `curl` 실패 시 **warn 로그만 남기고 조용히 스킵** — 실패가 상태로 남지 않음 ② 게이트웨이 운영 주체·SLA·버전 정책 미확인 ③ 터널 유닛(`wp-etw-bridge`)이 형상관리 밖 | 확인(코드) / 미확인(계약) | `crates/extractor/src/workpool.rs:111-170` |
| X-02 | **TT/장비 GPS fix** (TT 좌표의 **유일한** 소스) | 웹소켓 zone `wpt_gps` (로컬 루프백 터널) | 프레임 `data.datas.gps_update` → 인메모리 `LiveMap.devices` | lat, lon, speed(km/h 문자열), engine_on, accuracy, fuel_level, batt, nett, distance, dtime, userid | `data.id` (예: `TT####`, 알파 접두사=장비클래스) | **수신시각 `Utc::now()`.** 소스 `dtime`은 문자열 통과만, 저장 안 됨 | 이벤트 스트림. 장비당 기저 ~3초(이동 시) | positions API, `classify_tt` → Stage-2 후보차량, 사이클 검출, 위치 이력 3종 | ① **정차 시 단말 침묵**(이동 기반 보고) — 유휴 onset·큐 대기·정밀 도착이 구조적으로 관측 불가 ② 프레임이 **이중 인코딩 JSON**이라 두 번 파싱 ③ `speed`는 문자열에서 단위를 잘라 파싱, 실패 시 0.0 → **포맷 변경 시 조용히 '정지'로 오분류** ④ (0,0) fix 폐기 등 다층 가드 필요 | 확인 | `crates/api/src/livemap.rs:3186-3245`, `3005-3092` |
| X-03 | **작업 컨텍스트 필드** (동일 프레임) | 웹소켓 `wpt_gps` | `gps_update` | jobtype, vslname, container1/2, cur_loc, topos1/topos2, arrival, arr_dtime | 장비 ID | `arr_dtime` = **HH:MM:SS MYT — 파싱되는 유일한 소스 시각** | 동일 스트림. 결측 시 latched 값 유지 | 사이클 v1/v2 단계, 도착 판정, 작업지점 학습 | ① 필드 결측이 잦아 latch 필요 ② 정차 트럭은 갱신 없음 ③ **장비 로컬시계 오차가 그대로 도착시각으로 들어감**(검증 로직 없음) | 확인 | `crates/api/src/livemap.rs:3226-3243`, `3330-3339`, `3774-3796` |
| X-04 | 크레인 PLC 상태 | 웹소켓 zone `ctab` | `plc_data` | load, lock, land, hpos, tpos | crane (C/M/Z id, GPS id와 동일) | **수신시각** | 이벤트, 크레인당 ~1초 [문서]. `identify`만 하고 `checkin` 없음 | QC 이동수·LD 핸드오버 엣지, QC starvation | **RTG에는 PLC가 없음** → DS(양하) 측 관측 비대칭. LD만 발화 | 확인 | `crates/api/src/livemap.rs:3094-3170` |
| X-05 | 터미널 날씨(시간별) | Open-Meteo (공용 인터넷) | `api.open-meteo.com/v1/forecast` → `weather_hourly` | precipitation, wind_speed_10m, visibility, weather_code | ts (UTC 시각 버킷) | UTC 시간 버킷. `past_days=1`로 자가치유 | 폴링 1시간 | 이동시간 모델 피처 | 공용 인터넷 의존. **폐쇄망 전환 시 피처 결측** | 확인 | `crates/extractor/src/weather.rs:1-40` |
| X-06 | 터미널 날씨(1분) | Tomorrow.io (공용 인터넷) | `/v4/timelines` → `weather_1min` | 상동 | ts (UTC) | UTC | 폴링 3분 | 이동시간 모델 피처 | **무료 쿼터**(문서상 500/day·25/hr)에 걸리면 결측. 키 `TOMORROW_API_KEY`(값 `<redacted>`) 미설정 시 실패 | 확인 | `crates/extractor/src/weather.rs:79-83` |

---

## 6. 내부 상태 (로컬 Postgres)

### 6.1 TOS 미러 — `live_*` 5종 (전량 교체 스냅샷)

| 테이블 | 내용 | Key | Timestamp | 갱신 | 위험 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|
| `live_workpool` | 진행중 무브 | (qc, queuename, ytno) 사실상 | `upd_ts` ≈ TOS 배차시각(근사), `etw_ts`, `actv_ts` | 90초 **DELETE 후 전량 재삽입** | 과거 큐·마감 추이 **이력 없음** | 확인 | `crates/extractor/src/workpool.rs:265-300` |
| `live_candidate` | 미배차 수요 버킷 | (queuename, vessel, jobtype, src_block) 집계 | `as_of_ts` = 추출 시각(UTC) | 90초 전량 교체 | 트윈은 twinkey로 1대 수요 합산 | 확인 | `crates/extractor/src/workpool.rs:262-345` |
| `live_workqueue` | QC 큐 계획·진행 | (qc, vessel, queuename) | `as_of_ts` | 90초 전량 교체 | seq 재계획 시 원거리 ETA 신뢰도 저하 | 확인 | `crates/extractor/src/workpool.rs:198-220` |
| `live_vessel_schedule` | 선박 일정(마감 원천) | (vessel, voyage) | MYT 문자열 → UTC 변환 | 90초 전량 교체 | ESTWKC 가드 필요 | 확인 | `crates/extractor/src/workpool.rs:226-256` |
| `live_assigned_tt` | 배차된 TT 목록 | ytno | — | 90초 전량 교체 | 유휴 트럭 미관측 | 확인 | `crates/extractor/src/workpool.rs:186-190` |

> **핵심 리스크**: `live_*`는 전량 교체이므로 **과거 큐·마감 추이의 사후 재현이 불가능**하다. 시뮬레이션·리플레이는 별도 append 이력이 필요하다. [문서] `kc/dispatch/simulator-spec.html:158`도 동일 지적.

### 6.2 TOS 이벤트 축적 테이블 (append)

| 테이블 | 소스 | Key | Timestamp | 갱신 | 위험 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|
| `tos_handover_label` | JOB_ORDER_HISTORY | (contno, point, seqno) | `comp_ts` = 완료(MYT→UTC) | 60초 증분, cap 3000 | `ON CONFLICT DO NOTHING` → 같은 키 재작업 시 두 번째 이벤트 유실. 원천 ~15일 | 확인 | `crates/extractor/src/handover.rs:82-87` |
| `qc_move_log` | MCH_OPERATION (C/M/Z) | (machno, contno, seqno) | `st_ts`, `comp_ts` | 5분 증분, cap 5000 | 당일 등가조건 → 자정 경계 유실 | 확인 | `crates/extractor/src/qc_moves.rs:82-87` |
| `rtg_move_log` | MCH_OPERATION (RTG/ES) | (machno, contno, seqno) | `comp_dt` | 5분 증분, cap 5000 | 동일 | 확인 | `crates/extractor/src/rtg_moves.rs:81-90` |
| `tos_etw_cntr` | ETW 게이트웨이 | (vessel, voyage, cntr_no) | `qc_etw_utc` 등 | 90초 upsert + 2시간 경과 DELETE | 게이트웨이 실패 시 조용히 스킵 | 확인 | `crates/extractor/src/workpool.rs:111-170` |
| `tt_move_log` | Postgres 조인 (`psql` 스크립트) | (ytno, contno, dispatch_ts) | 배차→완료 | 5분 (`wp-tt-move-log`) | Oracle 미접촉. SQL 파일이 미커밋 상태로 실행될 수 있음 | 확인 | `deploy/systemd/wp-tt-move-log.service:1-13` |
| `tt_cycle_recon` | Postgres 조인 (`psql` 스크립트) | — | — | 10분 (`wp-tt-cycle-recon`) | `truck_pos_hifreq` 보존기간(§11 상충) 안에서 돌아야 함 | 확인 | `deploy/systemd/wp-tt-cycle-recon.service:1-14` |

### 6.3 위치 이력 3종

| 테이블 | 내용 | Key | Timestamp | 갱신 | 보존 | 위험 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|---|
| `truck_pos_hist` | 트럭 위치 + 배차상태 | (ytno, ts) | **틱 시각** `Utc::now()` | 30초 배치 | 2일 | **raw fix 아님** — 30초 스냅샷. 커버리지 분석에 쓰면 과대표시(문서 경고) | 확인 | `crates/api/src/livemap.rs:4623-4680`, `db/migrations/0059_truck_pos_hist.sql` |
| `truck_pos_hifreq` | 고빈도 궤적 | (ytno, ts) | **3초 틱 시각** | 3초 틱, 직전 대비 **5m 이상 이동 시만** | 5일 | 정차 구간 행 없음. 보존기간 문서-코드 상충(§11) | 확인 | `crates/api/src/livemap.rs:4788-4835`, `db/migrations/0067_truck_pos_hifreq.sql:1-14` |
| `rtg_pos_hist` | RTG/ES 위치 | (machno, ts) | **3초 틱 시각** | 3초 틱 | 3일 | 정지 지터 포함(의도적), RTG GPS 40m 오프셋 | 확인 | `crates/api/src/livemap.rs:4686-4735`, `db/migrations/0086_rtg_pos_hist.sql:1-16` |

> 세 라이터 모두 `lm.connected`(GPS 웹소켓 연결)가 false면 틱을 건너뛰고, 신선(≤120초) 장비만 기록한다. 따라서 **피드 장애 구간은 데이터 '공백'으로만 남고 장애 마커 행이 없다** — 사후 분석에서 "트럭 없음"과 "피드 없음"이 구분되지 않는다. 근거: `crates/api/src/livemap.rs:4628-4656`, `4793-4812`.

### 6.4 도로망 (배차 비용의 본체)

| 테이블 | 내용 | Key | Timestamp | 갱신 | 위험 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|
| `road_node` | 추론 도로망 노드 | node id | — | **매시 cron** `scripts/reinfer_roadgraph.sh`(저장소에 스케줄 정의 없음) | **노드 id가 매 빌드 재번호**되어 REPEATABLE READ 스냅샷 필수(코드에 반영) | 확인 | `crates/api/src/roadgraph.rs:47-92` |
| `road_edge` | 엣지(len_m, speed_kmh, oneway) | (from_id, to_id) | — | 상동 | **그래프가 비면 전량 L3 맨해튼 폴백** → 모든 OD 비용이 열화 | 확인 | 상동 |
| `road_route_eval` | 경로시간↔실측 학습곡선 | — | `ts` (최근 14일 창) | 매 tick `RouteCost::load` | 곡선 미형성 시 상수 속도 폴백 | 확인 | `crates/api/src/roadgraph.rs:360-382` |

### 6.5 학습 산출물 (MV·테이블)

| 객체 | 내용 | Key | 갱신 | 위험 | 상태 | 근거 |
|---|---|---|---|---|---|---|
| `learn_free_in_bias` / `learn_free_in_stationary` / `learn_soon_idle_gate` | '곧 빔' 잔여시간·게이트 | (state, jobtype, dist_bin) 등 | `spawn_selfcal_refresh` 900초 | 표본 임계(50/100/200) 미달이면 **상수 폴백**. 값은 30~3600초 클램프. MV 갱신이 멈추면 조용히 상수로 회귀 | 확인 | `crates/api/src/livemap.rs:4129-4162`, `4243-4271` |
| `learn_work_eta_bias` | 작업-ETA 잔차 자가보정 | (qc, jobtype) | `dispatch_pred_logger` tick%10 (약 20분) | n<30(전역행 100) 무시, ±(−900,1800)초 클램프 | 확인 | `crates/api/src/workpool.rs:349-368`, `761-766` |
| `learn_qc_move_time` | QC 처리 cadence | (qc, jobtype, shift) | **야간 1회** (Oracle) | 롤링 3일창 → 과거 재현 불가 | 확인 | `crates/extractor/sql/qc_move_time.sql:7-32` |
| `learn_topos_point` | 학습된 작업지점 좌표 | topos | 5분 persist, 기동 시 복원 | 블록 300m 아웃라이어 게이트, QC는 스프레더 진동 보정 필요 | 확인 | `db/migrations/0026_learn_topos.sql:1-25` |
| `data/travel_gbm.pkl` | 이동시간 GBM 모델 파일 | — | 15분 cron | **`.gitignore` 제외** → 호스트 손실 시 수개월 학습분 소멸 | 확인 | `.gitignore:1-8`, `scripts/travel_gbm_shadow.py:11-13` |

### 6.6 감사·운영 상태

| 객체 | 내용 | Key | Timestamp | 갱신 | 위험 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|
| `etl_run_log` | ETL 실행 이력 (kpi_key, business_date, status RUNNING/OK/PARTIAL/FAILED, rows_written, error_text) | `run_id` (BIGSERIAL) | `started_at`/`finished_at` = **실행 시각**(UTC) | 추출기 실행마다 INSERT+UPDATE | **보존/파티셔닝 정책 없음 — 무한 증가.** API 엔드포인트로 노출되지 않음(직접 조회만) | 확인 | `db/migrations/0001_run_log.sql:5-16`, `crates/extractor/src/db.rs:17-70` |
| `data_freshness` | KPI별 신선도 | `kpi_key` (PK) | `last_success_at` | 추출 종료 시 upsert | **일 단위 ETL 신호라 실시간 장애 판정에 부적합**(코드 주석 명시) | 확인 | `db/migrations/0001_run_log.sql:19-26`, `crates/api/src/routes.rs:469-490` |
| `scenario.gen_run` | 시나리오 수집 실행 상태 | `run_id` | started/updated/finished_at | scengen 틱마다 | 무인증 엔드포인트(포트 8899)로 노출 | 확인 | `db/migrations/0093_scenario.sql:26-45` |
| `scenario.gen_event` | 이벤트 저널 (info/warn/error × query/chunk/heartbeat/skip/tick_failed, payload JSONB) | `event_id` | `ts` = 발생 시각 | append-only | 보존정책 미정의(gen_run 삭제 시 CASCADE만) | 확인 | `db/migrations/0093_scenario.sql:47-55` |
| `scenario.config` | 수집기 킬스위치·튜닝 (enabled, chunk_minutes, offpeak_only, offpeak_start_h/end_h, oracle_timeout_s, retention_days) | `id=1` 싱글턴 | `updated_at` | 매 틱 read; `POST /api/scenario/config`로 write | **쓰기 엔드포인트가 0.0.0.0:8899에 무인증 노출.** `enabled` 기본값 true → 마이그레이션 적용만으로 수집 시작 | 확인 | `db/migrations/0093_scenario.sql:11-24`, `crates/scengen/src/serve.rs:33-44` |

---

## 7. 출력 (Service 2가 만들어 내보내는 것)

| # | 산출물 | 저장/전달 형태 | 주요 Column | Key | Timestamp | 주기·보존 | 소비처 | 계약상 제약 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|---|---|---|
| Y-01 | **Stage-2 매칭 권고 (트럭→작업)** | Postgres `stage2_match_shadow` | ts, tick, ytno, qc, vessel, queuename, jobtype, src_block, veh_state, arrival_s, od_p90_s, deadline_slack_s, feasible, cost_tier, switched, dest_lat, dest_lon, src_lat, src_lon | (ts, ytno) | `ts` = **추천 tick 시각** | 60초, **21일 보존** | `/api/stage2/advisory`, `/api/stage2/shadow`, `/api/health/dispatch` → Stage2Page·TtPage·LiveMapPage | ⚠ **컨테이너번호·작업지시 ID(MSNSEQ) 없음.** 행이 (QC, 선박, 큐, 작업유형, 출발블록) 버킷 단위라 **TOS에 개별 지시를 지목 불가** | 확인 | `crates/api/src/livemap.rs:4466`, `4486`, `db/migrations/0052_stage2_match_shadow.sql:5-22`, `crates/api/src/workpool.rs:796-822` |
| Y-02 | 솔버 성능 (그리디 대비) | Postgres `stage2_solver_shadow` | n_trucks, n_works, greedy_n, greedy_cost_s, optimal_n, optimal_cost_s, gap_pct, greedy_miss, optimal_miss | ts | `ts` | 60초, 21일 | `/api/stage2/shadow`의 savings_pct, `/api/health/dispatch` | greedy는 **실제로 쓰이지 않는 가상 베이스라인** | 확인 | `crates/api/src/livemap.rs:4478-4484` |
| Y-03 | 대시보드 API | HTTP `GET /api/stage2/*` (그 외 포함 **총 31개 라우트 전부 GET**) | — | — | — | 프론트 폴링 15초 | 자체 React SPA | **인증·인가 계층 없음**, `CorsLayer::permissive()`, 기본 바인드 루프백:8080 | 확인 | `crates/api/src/main.rs:45-75` |
| Y-04 | 시나리오/에뮬레이터 JSON | `GET /api/scenario/download/:kind` (포트 8899) → 브라우저 첨부파일 | `assemble::build` 결과 (scenario, emulator) | window_start/window_end | comp_ts, snapshot_ts | 온디맨드 동기 조립 | 브라우저 다운로드 | **대시보드 API에 마운트되지 않음.** 실배차와 완전 분리. 마이그레이션 주석의 "crates/api가 /sim UI로 읽는다"는 서술은 **사실과 다름**(해당 라우트·`web/sim` 디렉터리 없음) | 확인 / 상충(주석) | `crates/scengen/src/serve.rs:86-115`, `db/migrations/0093_scenario.sql:1-8` |

### 7.1 출력에 **존재하지 않는 것** (계약 협의에 직결)

| 부재 항목 | 확인 방법 | 상태 |
|---|---|---|
| TOS(Oracle) write-back 경로 | TOSADM 대상 INSERT/UPDATE/DELETE/MERGE/PL-SQL 호출 전수 검색 **0건** | 확인 |
| 배차 지시용 커맨드 ID · Ack · 재전송 · 순서보장 | 코드에 개념 자체 없음. 멱등성은 Postgres upsert 수준뿐(`db/migrations/0092_tt_move_log.sql:41` 등) | 확인 |
| 운영자 Override/Swap 채택·거부 수집 | 수집 코드·테이블 없음 → **롤아웃 게이트 기준인 "현장 거부율"을 현재 데이터로 측정 불가** | 확인 |
| 외부 KPI 계층(DigiPort 등) 내보내기 | webhook/kafka/mqtt/S3/CSV export 코드 **0건** | 확인 |
| 프론트에서의 쓰기 조작 | `web/src`에 POST/PUT/DELETE fetch **0건**. `api.ts:298`의 단일 fetch는 옵션 없는 GET | 확인 |
| 자동 강등/폴백 로직 | 코드 레벨 없음. 현재는 그림자라 시스템이 멈춰도 TOS가 그대로 배차 | 확인 |

---

## 8. 결과 피드백 (효과 측정용 산출물)

| # | 항목 | 테이블 | 주요 Column | Key | Timestamp | 주기·보존 | 사용 위치 | 해석 시 주의 | 상태 | 근거 |
|---|---|---|---|---|---|---|---|---|---|---|
| F-01 | TOS 실제 배차 vs 우리 권고 (작업별) | `dispatch_compare_shadow` | qc, queuename, jobtype, tos_ytno, tos_arrival_s, our_ytno, our_arrival_s, agree, reason, delta_s, tos_upd | (qc, queuename, tos_ytno, tos_upd) | `tos_upd` = **TOS 배차시각(소스 시각)** | 60초, 21일 | `/api/stage2/compare`, `compare-picks` | **작업별 독립 최근접 선택**이라 동일 트럭 중복 선택 가능 → **우위 과대평가**(코드 주석도 optimistic으로 명시). `reason='now'` 행은 집계에서 제외 | 확인 | `crates/api/src/livemap.rs:4585-4612`, `crates/api/src/workpool.rs:1310` |
| F-02 | 공정 1:1 비교 | `fair_compare_shadow` / `fair_compare_detail` | window_min, n, tos_total_s, our_total_s, savings_pct, same_n / jobtype, qc, tos_s, our_s | ts | `ts` | 300초(창 15분), 21일 / 7일 | `/api/stage2/fair-compare`, `fair-breakdown` | n<4면 스킵, **MAX_N=120으로 표본 절단**. 절감치가 "대부분 원거리 교정"이라는 문서상 주의 | 확인 | `crates/api/src/livemap.rs:4930-5000`, `db/migrations/0061_fair_compare_shadow.sql` |
| F-03 | Stage-1 예측 검증 | `dispatch_pred_sample` | qc, vessel, contno, queuename, jobtype, pred_work_eta_ts, dispatch_deadline_ts, assigned, slack_s, lead_s, became_assigned_at, tos_upd_dt, etw_qc_ts | 행 단위(컨테이너 1회 로깅) | `logged_at` / `pred_work_eta_ts` | 120초, 21일 | `learn_work_eta_bias` 학습, `/api/learn/dispatch-pred` | **QC별 front 6건만 표본** | 확인 | `crates/api/src/workpool.rs:730-760` |
| F-04 | 맵매칭 도착 섀도 | `mm_arrival_shadow` | leg_dur_s, route_m, progress_frac, min_dest_m, saw_arrived, max_gap_s, max_jump_m | id(bigserial), ytno+dest_topos | `logged_at`(수신 기준) | leg 전환 시(5초 틱 평가) | 도착 포착 개선 측정 | **섀도 전용 — 라이브 도착 판정·배차에 미반영** | 확인 | `db/migrations/0090_mm_arrival_shadow.sql:1-19` |

> **효과 수치 인용 경고 (상충)**: "최적 매칭 이득"에 대해 [문서] `kc/dispatch/stage2-journey.html:40`은 "총 도착시간 약 40% 감소", `kc/data/tos-verification.html:59`는 "38~43% 절감", `kc/start/launch-plan.html:32`는 "−5.1%(단순 그리디 대비 아낀 공차시간)"로 서로 다르다. 게다가 코드 내 정의도 둘이다 — `gap_pct=(greedy−opt)/opt`(`crates/api/src/livemap.rs:4476`) vs `savings_pct=(greedy−opt)/greedy`(`crates/api/src/workpool.rs:921-928`). **세 문서 모두 정적 HTML 하드코딩이며 라이브 갱신되지 않는다.** 외부 인용 전 지표 정의·측정창·기준선 단일화가 필요하다 — Service 2 담당 PM 확인 필요.

---

## 9. Key · Timestamp 의미 정리

### 9.1 시각의 4가지 층

| 층 | 정의 | 이 시스템에서의 예 | 확보 여부 |
|---|---|---|---|
| **① 업무 발생시각** | 현장에서 사건이 실제로 일어난 시각 | `YT_DIS_DT`(배차), `ACTV_DT`(작업 활성화), `JOB_HIST_DATE‖TIME`(완료), `MCH_OPER_ST_DT`/`COMPTIME`(크레인 무브 시작·완료), GPS `arr_dtime`(도착) | 부분 확보 |
| **② 반영시각** | TOS DB에 그 사건이 기록된 시각 | `JOB_ORDER_LIST.UPD_DT`, `JOB_QUEUE_SCHEDULE.UPD_DT`, `VSS_STT_UP_DT` | 확보 |
| **③ 수집시각** | 우리가 그 데이터를 읽어온 시각 | `live_*.as_of_ts`, `truck_pos_hist.ts`(틱 시각), GPS `last_seen_ms`, `tos_etw_cntr.fetched_at_utc`, `etl_run_log.started_at` | 확보 |
| **④ 표시시각** | 화면에 그려진 시각 | 라이브맵 기본 **5초 지연 버퍼** 재생 → 화면상 '지금'은 최소 5초 과거 | 확보 |

> **①→② 구간(현장 이벤트 → TOS Oracle 반영 지연)은 전혀 측정되지 않았다.** 우리가 알 수 있는 것은 ②→③(폴링 주기)과 ③→④(표시 버퍼)뿐이다. 상태: **미확인**. 실시간성 SLA를 약속하려면 이 구간 실측이 선행되어야 한다 — Service 2 담당 PM/개발 확인 필요.

### 9.2 데이터별 시각 층 매핑

| 데이터 | 저장되는 시각 | 어느 층인가 | 근거 |
|---|---|---|---|
| `tos_handover_label.comp_ts` | JOB_HIST_DATE‖TIME (MYT→UTC) | **① 발생** | `crates/extractor/src/handover.rs:70-99` |
| `qc_move_log.st_ts` / `comp_ts` | MCH_OPER 소스 문자열 파싱 | **① 발생** | `crates/extractor/src/qc_moves.rs:78-95` |
| `live_workpool.upd_ts` | JOB_ORDER_LIST.UPD_DT | **② 반영** (배차시각 D_tos의 **근사**) | `crates/extractor/sql/workpool.sql:23-25` |
| `live_*.as_of_ts` | 추출 실행 시각 | **③ 수집** | `crates/extractor/src/workpool.rs:262-345` |
| `truck_pos_hist.ts` / `hifreq.ts` / `rtg_pos_hist.ts` | 배치 틱의 `Utc::now()` | **③ 수집** (엄밀히는 '틱 시각' — 마지막 수신이 최대 120초 지난 고정점도 새 ts로 재기록) | `crates/api/src/livemap.rs:4633`, `4697`, `4799` |
| GPS `dtime`(단말 시각) | **저장 안 됨.** 문자열로 통과해 프론트 '기기시각' 표시에만 사용 | (해당 없음) | `crates/api/src/livemap.rs:3236`, `1123` |
| GPS `arr_dtime` | **유일한 예외** — 파싱되어 `leg.arrived_ms`로 승격, `tt_cycle_v2`·`tt_wp_arrival.arrived_at`에 저장 | **① 발생** | `crates/api/src/livemap.rs:3774-3796`, `3523-3525`, `4115` |
| `dispatch_compare_shadow.tos_upd` | TOS UPD_DT 그대로 | **② 반영** | `crates/api/src/livemap.rs:4607-4614` |
| `stage2_match_shadow.ts` | 추천 tick 시각 | **③ 수집(계산)** | `crates/api/src/livemap.rs:4466` |

> **결론**: **GPS 계열은 수신시각만 저장된다**(예외 `arr_dtime` 1건). 따라서 **소스↔수집 지연을 분리 측정할 수 없다.** 반면 **TOS 계열은 소스 시각(문자열 `YYYYMMDD`+`HH24MISS`, MYT)을 파싱해 저장**하므로 발생시각을 쓸 수 있다. 두 계열의 시각 성격이 다르다는 점이 사이클·대기시간 지표 해석의 핵심이다.

### 9.3 Key 규약

| 대상 | Key | 주의 |
|---|---|---|
| TOS 작업지시 | (QUEUENAME, VESSEL, CONTNO, MSNSEQ) | `queuename`이 선박·항차 간 **재사용**되어 Oracle 조인 시 fan-out 위험 → 코드는 Oracle 조인을 회피하고 Postgres에서 부착 |
| 완료 핸드오버 | (CONTNO, POINT, SEQNO) | `ON CONFLICT DO NOTHING` |
| 크레인 무브 | (MACHNO, CONTNO, SEQNO) | 같은 키가 다른 날 재사용되면 **두 번째 이벤트 유실** (미확인 — 실제 재사용 여부 확인 필요) |
| 장비 | `ytno` / `machno` / GPS `data.id` (`TT####`) | GPS id와 TOS TRK_ID가 같은 체계 |
| 트윈 | `TWINKEY` | 조회는 하지만 `live_workpool`에 **저장되지 않음** → 하류에서 재계산 필요 |
| 배차 권고 | (ts, ytno) | **작업 측 식별자가 버킷 단위** — 개별 지시 지목 불가 |

---

## 10. 데이터 품질 위험 종합

| # | 위험 | 대상 | 발생 조건 | 영향 | 상태 | 근거 |
|---|---|---|---|---|---|---|
| R-01 | **워터마크 사전순 전진·롤백 불가** | handover, qc_moves, rtg_moves, scengen collect/yard/enrich | 미래 날짜 이상치 1건이 유입되면 워터마크가 앞으로 튐 | 이후 정상 데이터가 **영구 누락**. `GREATEST`로만 전진하므로 되돌릴 수 없음 | 확인 | `crates/extractor/src/handover.rs:106-120` |
| R-02 | **당일 등가조건(`COMPDATE = 오늘`)** | qc_moves, rtg_moves, scengen yard_moves | 서비스가 자정을 넘겨 정지 | 전날 미수집분 **영구 복구 불가**. 구멍이 `tt_move_log`·싸이클 KPI로 그대로 전파 | 확인 | `crates/extractor/src/qc_moves.rs:59-64`, `rtg_moves.rs:59-64`, `crates/scengen/src/yard.rs:56-62` |
| R-03 | **`live_*` 전량 교체** | live_workpool/candidate/workqueue/vessel_schedule/assigned_tt | 매 90초 tick | 과거 큐·마감 추이 **이력 없음** → 사후 재현·시뮬 리플레이 불가 | 확인 | `crates/extractor/src/workpool.rs:186-190`, `266-268` |
| R-04 | **`ON CONFLICT DO NOTHING`** | tos_handover_label, qc_move_log, rtg_move_log, scenario.move_hist | 동일 업무키의 후속 갱신·재작업 | 두 번째 이벤트가 **조용히 버려짐**. 중복 억제와 유실이 구분되지 않음 | 확인 | `crates/extractor/src/qc_moves.rs:82-87`, `crates/scengen/src/collect.rs:102-107` |
| R-05 | **캡 도달 무경보** | extractor handover(3000)/qc(5000)/rtg(5000) | 물량 급증으로 FETCH_CAP 도달 | 밀림이 조용히 누적. scengen만 `gen_event`에 기록하고 extractor는 알리지 않음 | 확인 | `crates/extractor/src/rtg_moves.rs:19-20`, `crates/scengen/src/collect.rs:133-152` |
| R-06 | **행수 캡 없는 쿼리** | `workpool.sql` / `assigned_tt.sql` (90초 주기) | 물량 폭주·미완료 주문 적체 | 반환 행 수가 무제한 증가 → 90초 틱 지연 → 화면 FROZEN(300초 임계). **실측 없음** | 확인(캡 부재) / 미확인(행수 분포) | `crates/extractor/sql/workpool.sql:1-40`, `crates/extractor/src/workpool.rs:1-10` |
| R-07 | **문자열 SQL 조립(바인드 변수 없음)** | 전 Oracle 쿼리, 특히 scengen enrich | 원천 vessel/voyage 값에 작은따옴표 포함 | 쿼리가 깨지거나 의도치 않은 술어가 됨(읽기 전용이라 파괴적이진 않음) | 확인 | `crates/extractor/src/params.rs:11-13`, `crates/scengen/src/enrich.rs:76-77` |
| R-08 | **상태 코드 필터로 인한 누락** | JOB_ORDER_LIST | JOBSTATUS **A·Q만** 취득 + `CRE_DT ≥ SYSDATE−2` | P(계획)·B(블록) 상태와 2일 창 밖 작업이 **수요에서 통째 제외** → 매칭이 과소 공급으로 편향되고 비교 분모도 달라짐 | 확인 | `crates/extractor/sql/workpool.sql:14-38` |
| R-09 | **표본 필터로 인한 소멸** | K_EMPTY(`HAVING ≥50`), K_TT_CYCLE(120~1200초 클립), qc_move_time(표본<30) | 표본이 적은 조합 | 조합이 통째로 사라지거나 중앙값이 실제보다 낮게 산출(41% 과소 이슈) | 확인 | `crates/extractor/sql/e4_k_empty_decomposition.sql:4-39`, `c10_k_tt_cycle.sql:8-40` |
| R-10 | **읽기 전용이 구조적 보장이 아님** | Oracle 게이트웨이 2개 | `run_sql`이 임의 SQL 문자열을 그대로 외부 스크립트에 전달. SELECT 강제·DML 거부 가드 **없음** | 실제 강제는 저장소 밖 `remote-toolbox-sql`과 Oracle 계정 권한에 달려 있고 **저장소로 검증 불가** | 미확인 | `crates/extractor/src/runner.rs:41-72`, `crates/scengen/src/toolbox.rs:44-73` |
| R-11 | **신선도 게이트 결손** | Stage-2 매칭 | `wp-workpool` 타이머가 죽어도 Stage-2는 GPS 연결 여부만 검사 | **낡은 작업목록으로 매칭이 계속 산출·기록**되어 그림자 지표가 조용히 오염. 300초 FROZEN은 프론트 전용 | 확인 | `crates/api/src/livemap.rs:4173-4183`, `crates/api/src/workpool.rs:183-187`, `web/src/TtPage.tsx:583` |
| R-12 | **GPS 정차 침묵** | 모든 위치 파생 지표 | 단말이 "움직일 때만 보고" | 유휴 onset·큐 대기·정밀 도착이 **구조적으로 관측 불가**. 개별 트럭 유휴 리드타임 예측은 [문서] ADR에서 **불가로 확정** | 확인 | [문서] `kc/data/websocket-coverage.html:19,37-40`, `kc/dispatch/leadtime-adr.html` |
| R-13 | **산출물에 작업 식별자 부재** | `stage2_match_shadow` | 항상 | TOS 연동 시 "어느 지시를 어느 TT에"를 지목할 수 없음 → **스키마 변경 + 매칭 입력 단위 재설계 수반** | 확인 | `db/migrations/0052_stage2_match_shadow.sql:5-22` |
| R-14 | **트윈/탠덤 미반영** | Stage-2 수요 산정 | 트윈 작업 존재 시 | `Stage2Work`에 twintandem 필드 없음 → 크레인별 수요 상한 **과대 산정 가능** | 확인 | `crates/api/src/workpool.rs:796-822` |
| R-15 | **피드 장애가 '공백'으로만 남음** | truck_pos_* 3종 | `lm.connected=false` | 장애 마커 행이 없어 사후 분석에서 "트럭 없음"과 "피드 없음"이 구분되지 않음 | 확인 | `crates/api/src/livemap.rs:4628-4656` |
| R-16 | **'라이브'의 정의가 층마다 다름** | 백엔드 vs 프론트 | 항상 | 백엔드 STALE 120초 / 배너 red 60초 / 맵 stale 120초 / 작업풀 FROZEN 300초 / PLC stale 30초 → **SLA 문구를 하나로 정해야 함** | 확인 | `crates/api/src/livemap.rs:27-33`, `web/src/App.tsx:65-86`, `web/src/TtPage.tsx:300-313` |
| R-17 | **`truck_pos_hist` 함정** | 커버리지 분석 | 이 테이블로 신선도·커버리지를 계산할 때 | raw fix가 아니라 30초 배치 스냅샷이라 **실제보다 2배 이상 좋게 보임** | 확인 | [문서] `kc/data/websocket-coverage.html:68` |
| R-18 | **프루닝 정리가 프로세스에 종속** | 위치 이력·그림자 테이블 전체 | API 프로세스 정지 | 보존 DELETE도 함께 멈춰 **디스크가 무한 증가** | 확인 | `crates/api/src/livemap.rs:4673-4676`, `4726-4730`, `4827-4831` |
| R-19 | **`etl_run_log` 무한 증가** | 감사 로그 | 항상 | 보존/파티셔닝 정책 없음 | 확인 | `db/migrations/0001_run_log.sql:5-16` |
| R-20 | **QC 식별 기준 불일치** | KPI 간 | MCH_OPERATION 조회 시 | `^C[0-9]+$`(c07/c10/f2/e1c) vs `^[CMZ][0-9]+$`(qc_move_time·qc_moves) → **M/Z 크레인이 일부 KPI에서 누락** | 확인 | `crates/extractor/sql/c07_k_mph_realtime.sql:5-25`, `qc_move_time.sql:7-32` |
| R-21 | **시각 문자열 품질 파손** | 전 TOS 시각 컬럼 | COMPTIME 길이<6, 시>23 등 | SQL마다 방어 필터가 들어가 있음 = **원천 데이터가 문자열 수준에서 깨져 있다는 신호** | 확인 | `crates/extractor/sql/c10_k_tt_cycle.sql:17-19` |
| R-22 | **학습 MV 중단 시 조용한 상수 회귀** | free_in / work_eta_bias | MV 리프레시 정지 또는 표본 임계 미달 | 배차 비용이 학습값 대신 하드코딩 상수로 되돌아감(경보 없음) | 확인 | `crates/api/src/livemap.rs:4243-4271` |
| R-23 | **도로망 재추론이 형상관리 밖 cron** | road_node/road_edge | 이관·호스트 재구축 시 누락 | 그래프가 낡은 채 고정되고 **모든 OD 비용이 서서히 왜곡**되거나 전량 L3 폴백 | 확인 | [호스트] `crontab -l`, `crates/api/src/roadgraph.rs:47-50` |

---

## 11. 보존기간

### 11.1 원천(TOS Oracle)

| 테이블 | 보존 | 상태 | 근거 |
|---|---|---|---|
| `JOB_ORDER_HISTORY` | 약 **15일** | 확인(문서) / 미확인(DB 딕셔너리) | [문서] `README.md:82-84` |
| `MCH_OPERATION`, `VSS_STATISTICS` | **35일 이상** | 확인(문서) / 미확인 | [문서] `README.md:82-84` |
| `CYY_CONTAINER` | TOS ETL마다 **덮어쓰기**(이력 없음) | 확인 | `crates/scengen/src/snapshot.rs:1-7` |

> **의미**: 깊은 백필이 불가능하다. 장기 학습·과거 시나리오 재현의 유일한 방법은 **우리 Postgres에 앞으로 축적하는 것**이며, 축적이 끊긴 구간은 Oracle에서 재추출할 수 없다. 견적에서 "백필로 메우면 된다"는 전제는 성립하지 않는다.

### 11.2 우리 Postgres

| 테이블 | 보존 | 정리 주체 | 상태 | 근거 |
|---|---|---|---|---|
| `zone_density` | 4일 | API 프로세스 내부 DELETE | 확인 | `crates/api/src/livemap.rs:2305` |
| `truck_pos_hist` | 2일 | API 내부 DELETE (120틱마다) | 확인 | `crates/api/src/livemap.rs:4673-4676` |
| `rtg_pos_hist` | 3일 | API 내부 DELETE (200틱마다) | 확인 | `crates/api/src/livemap.rs:4726-4730` |
| `truck_pos_hifreq` | **5일** (코드·마이그레이션) ↔ **~1일** (스크립트·유닛 주석) | API 내부 DELETE (200틱마다) | **상충** | `crates/api/src/livemap.rs:4827-4831`, `db/migrations/0067_truck_pos_hifreq.sql:5` vs `scripts/populate_tt_cycle_recon.sql:17-19`, `deploy/systemd/wp-tt-cycle-recon.service:8-11` |
| `qc_wait_sample` | 14일 | API 내부 DELETE | 확인 | `crates/api/src/livemap.rs:2514` |
| `qc_wait_qc_sample` | 21일 | API 내부 DELETE | 확인 | `crates/api/src/livemap.rs:2514-2518` |
| `stage2_match_shadow` / `stage2_solver_shadow` | 21일 | API 내부 DELETE | 확인 | `crates/api/src/livemap.rs:4486` |
| `dispatch_compare_shadow` | 21일 | API 내부 DELETE | 확인 | `crates/api/src/livemap.rs:4585-4612` |
| `fair_compare_shadow` | 21일 / `fair_compare_detail` 7일 | API 내부 DELETE | 확인 | `crates/api/src/livemap.rs:4930-5000` |
| `dispatch_pred_sample` | 21일 | API 내부 DELETE | 확인 | `crates/api/src/workpool.rs:730-760` |
| `free_in_sample` | 30일 | API 내부 DELETE | 확인 | `crates/api/src/livemap.rs:1776` |
| `road_route_eval` | 60일 | API 내부 DELETE | 확인 | `crates/api/src/roadgraph.rs:521` |
| `scenario.*` | `scenario.config.retention_days` 기본 **45일** (설정값) | scengen | 확인 | `db/migrations/0093_scenario.sql:20` |
| `etl_run_log`, `data_freshness`, `scenario.gen_event` | **정책 없음(무한 증가)** | 없음 | 확인 | `db/migrations/0001_run_log.sql:5-26`, `0093_scenario.sql:47-55` |

### 11.3 정리 주체에 대한 구조적 지적

- 보존정책이 **코드 곳곳의 인라인 DELETE로 분산**되어 있다(2·3·4·5·7·14·21·30·45·60일). 단일 지점에서 조정할 수 없다.
- 정리 주체는 **API 프로세스 내부 태스크**뿐이다. **프로세스가 죽어 있으면 정리도 멈춘다.**
- **백업/복구 절차가 저장소 전체에 없다**(`pg_dump` 검색 0건). 학습 산출물 `data/travel_gbm.pkl`은 `.gitignore` 제외라 호스트 손실 시 수개월 학습분이 소멸한다. 상태: 확인. 근거: `.gitignore:1-8`, `scripts/travel_gbm_shadow.py:11-13`.
- **DB 실제 크기·일 증가율 실측이 없다.** `crates/api/src/learn.rs:697`이 `reltuples`를 런타임 조회할 뿐 값이 저장되지 않는다. 상태: 미확인 — 용량 산정 근거로 사용 불가.

---

## 12. 개인정보 · 민감정보

### 12.1 개인정보

| 항목 | 사실 | 상태 | 근거 |
|---|---|---|---|
| Oracle 쿼리 | **개인 식별 컬럼을 조회하지 않는다.** 장비번호(YTNO/MACHNO/TRK_ID)·컨테이너번호·선박/항차만 취득. `MCH_WORKTIME`은 오퍼레이터 로그인 세션이지만 사람 식별자는 선택하지 않음. `ETV_CMOV_OPERATOR`는 선사 코드(사람 아님) | 확인 | `crates/extractor/sql/e3a_k_util_tt_merged.sql:15-29`, `crates/scengen/src/enrich.rs:124` |
| **GPS 피드 `userid`** | **운전자 ID + 전화번호**가 원문(HTML `<br/>` 포함)으로 유입된다. `clean_driver()`는 태그·공백 정리만 하고 **마스킹하지 않는다**. 값은 `/api/livemap/positions` 응답에 그대로 실려 화면 상세 패널 "운전자" 행에 표시된다. **이 API에는 인증이 없다** | 확인 | `crates/api/src/livemap.rs:3233`, `3808-3812`, `1120`, `web/src/LiveVehicleDetail.tsx:166`, [문서] `kc/data/websocket-fields.html:42` |
| GPS `userid`의 DB 영속 저장 | 마이그레이션에서 `userid` 컬럼이 **확인되지 않음** → 영속 저장은 없는 것으로 보임 | 추정 | 마이그레이션 전수 확인 |

> **조치 필요**: 무인증 API로 전화번호가 노출된다. 개인정보 처리 정책·마스킹 요건 확인이 필요하다 — Service 2 담당 PM/보안 확인 필요. 요건이 확정되면 응답 스키마와 프론트 표시 양쪽 수정이 발생한다.

### 12.2 비밀정보 취급 현황 (값은 일절 기재하지 않음)

| 항목 | 사실 | 상태 | 근거 |
|---|---|---|---|
| 비밀 관리 방식 | 시크릿 매니저 없음. **저장소 루트 `.env` 단일 파일** + systemd `EnvironmentFile` 주입 | 확인 | `deploy/systemd/wp-nightly.service:7` |
| 사용 키 이름 | `DATABASE_URL`, `SKILL_DIR`, `API_ADDR`, `ETW_GATEWAY_URL`, `TOMORROW_API_KEY` (모든 값 `<redacted>`). 코드에는 `KC_DIR`, `WEB_DIST`, `LIVEMAP_WS_URL`, `LIVEMAP_IDENTIFY`/`USERNAME`/`USER`도 사용됨 | 확인 | `.env.example:1-10`, `crates/api/src/main.rs:88-101`, `crates/api/src/livemap.rs:3005-3009` |
| `.env.example` 누락 | 실운영 키 2개(`ETW_GATEWAY_URL`, `TOMORROW_API_KEY`)가 예시 파일에 없음 → 신규 환경 구축 시 ETW·실시간 날씨가 **조용히 기본값으로 떨어짐** | 상충 | `.env.example:1-10` vs `crates/extractor/src/workpool.rs:114-124` |
| `.env` 파일 권한 | **0644** — 동일 호스트의 다른 계정이 읽을 수 있음. 0600 조정 필요 | 확인 | [호스트] `ls -l` |
| **평문 DB 비밀번호가 존재하는 위치** | `scripts/reinfer_roadgraph.sh`, `scripts/estimate_equipment_specs.sh`, `scripts/travel_gbm_shadow.py`, `db/grants.sql`, 그리고 [호스트] crontab 라인. **값은 이 문서에 옮기지 않는다.** 즉 **저장소 자체가 자격증명 저장소가 되어 있다** → 외부 인수 시 비밀 회전 절차 필요 | 확인 | 각 파일 |
| GPS 웹소켓 자격증명 | 접속용 identify/username 기본값이 **소스 상수로 존재**(값 `<redacted>`) | 확인 | `crates/api/src/livemap.rs:3005-3009` |
| DB 권한 분리 | `db/grants.sql`의 읽기전용 `wp_ro` 역할은 **채택된 적 없는 잔존 계획**이다. API는 Postgres에 다수 INSERT/DELETE를 수행하므로(예: `livemap.rs` INSERT 31건) 이 역할로는 기동 자체가 불가능하다. `wp_ro`는 README·grants.sql 밖 어디에도 없다 | 상충 | `db/grants.sql:16-28`, `README.md:75-76`, `crates/api/src/livemap.rs:1497·2093·2560·2611` |
| 접근 통제 | 대시보드 API: 인증 없음 + `CorsLayer::permissive()`. scengen 웹: **0.0.0.0:8899에 무인증 킬스위치 POST** 노출 | 확인 | `crates/api/src/main.rs:75-76`, `crates/scengen/src/serve.rs:33-44` |

---

## 13. 계약 산출물 부재

| 항목 | 결과 | 확인 방법 | 상태 |
|---|---|---|---|
| **OpenAPI / Swagger 문서** | **0건** | `utoipa` 의존성이 워크스페이스에 **선언만** 되어 있고 코드 사용 0건(`rg 'utoipa|OpenApi'` 무히트) | 확인 |
| **AsyncAPI 명세** (웹소켓 피드용) | **0건** | 저장소 전수 검색 | 확인 |
| **JSON Schema / Avro / Protobuf** | **0건** | 저장소 전수 검색 | 확인 |
| **GPS 피드 프로토콜 사양서** | **없음.** 저장소에는 리버스 엔지니어링된 파서만 존재. `speed` 파싱은 문자열 휴리스틱 | 확인 | `crates/api/src/livemap.rs:3186-3245` |
| **ETW 게이트웨이 계약**(스키마·버전·SLA·인증) | **없음** | 코드에는 URL 조립과 응답 파싱만 존재 | 미확인 |
| **TOS 소비 계약**(배차 권고를 TOS가 받는 인터페이스) | **미착수**로 문서에 기재 | [문서] `kc/start/launch-plan.html` A1/A2 미체크 | 확인(미착수) |
| **DB 마이그레이션 적용 이력** | 적용기 없음(`sqlx::migrate!` 호출 0건), 적용 이력 테이블 없음, **`0098` 번호가 두 파일에 중복** | `db/migrations/0098_scenario_yard_block.sql`, `0098_tt_cycle_recon.sql`, `db/apply.sh:1-20` | 확인 |
| **부하시험·벤치마크** | **0건** | 테스트는 전부 기능 단위. 용량 문서의 vCPU/RAM/스토리지 수치는 전부 추정이며 문서 스스로 단서를 달았음 | 확인 |
| **매칭 로직 단위/회귀 테스트** | **0건** (Mcmf·classify_tt·비용 계산). API 크레이트의 유일한 테스트 모듈은 `periods.rs` | 확인 | `crates/api/src/periods.rs:132` |

> **의미**: 공통 게이트웨이와 공유할 데이터 계약을 정의하려면 **현재는 Rust 구조체와 `web/src` 타입을 역공학해야 한다.** 계약 문서화 공수를 별도로 계상해야 한다 — 범위·일정은 Service 2 담당 PM 확인 필요.

---

## 부록 A. 이 문서에서 "미확인"으로 남은 항목

| # | 항목 | 확인 주체 |
|---|---|---|
| A-1 | `remote-toolbox-sql`(저장소 밖)의 실제 접속 계정·권한이 읽기 전용으로 강제되는가. 이관 범위에 포함되는가 | 개발/보안 |
| A-2 | Azure `tos_etw_gateway`의 운영 주체·감싸는 TOS RPC·인증·SLA·버전 정책. 발주서의 '공통 게이트웨이'와 동일 대상인가 | Service 2 담당 PM |
| A-3 | TOS가 배차 권고를 소비할 인터페이스(테이블/API/메시지)의 존재 여부와 요구 식별자(작업지시 ID/MSNSEQ/컨테이너번호) | Service 2 담당 PM |
| A-4 | 현장 이벤트 → TOS Oracle 반영(UPD_DT) 지연 실측 | 개발 |
| A-5 | 90초 workpool 쿼리(캡 없는 유일한 쿼리)의 실제 반환 행 수 분포 | 개발 |
| A-6 | `MCH_OPERATION`·`JOB_ORDER_HISTORY`에 의존 인덱스가 실제로 존재·유지되는가 | 개발/운영 |
| A-7 | 상태 코드값(MCH_OPER_STATUS F/M, JOB_ODR_JOBSTATUS C/A/Q/P/B, YT_STATUS, VSB_VOY_STATUS)의 공식 목록·의미 | Service 2 담당 PM |
| A-8 | `CRNT_PSN_IDX_NO1~NO4` 디코딩 규칙의 TOS 사양서 근거 | 개발/운영 |
| A-9 | 동일 (machno, contno, seqno)가 다른 날짜에 재사용되는가 (재사용 시 무브 로그 유실) | 개발/운영 |
| A-10 | TOS 보존기간(~15일/≥35일)이 현재도 유효한가, 아카이브 테이블이 별도로 존재하는가 | Service 2 담당 PM |
| A-11 | `truck_pos_hifreq` 실제 운영 보존기간(1일 vs 5일 상충) | 개발/운영 |
| A-12 | Postgres 백업·복구 정책, 현재 DB 용량 및 일 증가율 | 개발/운영 |
| A-13 | 마이그레이션 적용 절차와 적용 이력(0098 번호 중복 포함) | 개발 |
| A-14 | GPS 피드 `userid`(운전자 ID+전화번호) 무마스킹 노출에 대한 개인정보 처리 정책 | Service 2 담당 PM/보안 |
| A-15 | 외부 인터넷 기상 API 사용이 고객 보안정책상 허용되는가, 폐쇄망 전환 시 대체 소스 | Service 2 담당 PM |
| A-16 | 장비 총 대수(TT/RTG/QC)의 권위 있는 값 — 문서 관측치가 서로 불일치하여 계약 수치로 사용 불가 | Service 2 담당 PM |
| A-17 | `wp-api.service`·`wp-etw-bridge.service`·crontab 2건을 저장소로 형상관리할 수 있는가 | 개발/운영 |
| A-18 | 최적매칭 이득의 공식 수치·정의·측정창 단일화 | 당사 |
| A-19 | scengen의 Oracle 접근(4개 신규 객체, 10~15분 주기)이 고객사에 사전 고지·승인되었는가 | Service 2 담당 PM |

---

## 부록 B. 문서-코드 상충 목록 (인용 전 확인)

| # | 문서 | 주장 | 코드/현행 사실 | 근거 |
|---|---|---|---|---|
| B-1 | `kc/data/tos-extraction.html` (단일출처 표방) | 추출점 **18개** | 실제로는 `qc_moves`(MCH_OPERATION C/M/Z)와 scengen 4개 객체(CDV_VESSEL·CYY_CONTAINER·ETV_BAPLIE_CONT·ETV_MOVINS_STOWAGE), **ETW 게이트웨이가 누락** — 이 문서를 "우리가 긁는 것 전부"로 제시하면 부하·범위를 과소 보고하게 됨 | `crates/extractor/src/qc_moves.rs:53-65`, `crates/scengen/src/enrich.rs:64-129` |
| B-2 | `README.md` (초기 커밋 이후 미갱신) | 저장소 = "6개 KPI 대시보드". "extractor만 Oracle을 만진다" | GPS 라이브맵·2단계 배차 그림자·학습·scengen 미반영. scengen도 Oracle을 조회함 | `crates/scengen/src/toolbox.rs`, `deploy/systemd/wp-scenario-*.service` |
| B-3 | `deploy/systemd/README.md` | 타이머 5종만 문서화 | 저장소 유닛은 서비스 20 + 타이머 18, [호스트] 실제 가동 16종. 문서가 안내하는 5종 중 2종(`wp-tick-t1/t2`)은 호스트에 미설치 | `deploy/systemd/` · [호스트] |
| B-4 | `db/migrations/0052` 주석 | `cost_tier` = L2(225m 격자) | 현행 코드는 R(도로망 라우팅) / L3(맨해튼). 225m 격자는 mig 0082에서 폐기 | `crates/api/src/livemap.rs:4308-4324` |
| B-5 | `db/migrations/0093` 주석 | "crates/api가 `/sim` UI로 scenario.*를 읽는다" | 해당 라우트·`web/sim` 디렉터리 없음 | `crates/api/src/main.rs:45-75` |
| B-6 | `docs/cycle-detection-v2-design.md` | `tt_cycle_v2`는 "승격 대기 그림자" | 현재는 학습·데이터 카탈로그의 **상시 소스** | `crates/api/src/learn.rs:379-392`, `638-641` |
| B-7 | `live-map-dev-guide.md` (draft, 2026-06-04) | TT 상태 5종 | 현행 `classify_tt`는 6종 | `crates/api/src/livemap.rs:915-1024` |
| B-8 | `db/grants.sql` + `README.md:75-76` | API는 읽기전용 `wp_ro`로 접속 | API는 Postgres에 다수 INSERT/DELETE 수행 → 이 역할로는 기동 불가. 채택된 적 없는 잔존 계획 | `crates/api/src/livemap.rs:1497` 외 |
