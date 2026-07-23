# 05. Service 2 미확인 사항과 질문 목록

## 1. 문서 정보와 사용법

### 1.1 문서 정보

| 항목 | 내용 |
|---|---|
| 대상 | Westports "Service 2 — TT Assignment" 사전조사 |
| 조사 기준 저장소 | `/home/tkadmin/projects/tt-aiops-platform` (단일 Git 저장소) |
| 기준 커밋 | 브랜치 `scengen-collector`, HEAD **10cc8c0** (이하 개별 근거에 커밋을 반복 표기하지 않음) |
| 조사일 | 2026-07-22 |
| 문서 성격 | **미확인 사항 정리**. 조사 결과 확정된 사실은 본 시리즈의 앞 문서를 참조하고, 이 문서는 "아직 답이 없는 것"만 다룬다. |
| 작성 원칙 | 이 문서는 최적화 로직의 개선안·대안 설계를 제시하지 않는다. 일정·범위·비용도 확정하지 않는다. |

### 1.2 근거 표기

| 표기 | 의미 |
|---|---|
| `경로:줄범위` | 저장소 파일에서 직접 확인한 근거 |
| **[호스트]** | 2026-07-22 조사용 호스트에서 읽기 전용 `systemctl --user` / `crontab -l` 관찰. 저장소 밖 근거이므로 재확인 권장 |
| **[문서]** | `kc/` 또는 `docs/` 문서의 주장. 코드로 확인된 사실과 구분 |
| **[없음]** | 검색했으나 근거가 발견되지 않음 |

각 질문에는 상태가 붙는다: **확인**(사실은 확인됐고 판단·승인만 남음) / **추정** / **미확인** / **상충**(자료 간 값이 다름).

### 1.3 우선순위 정의 (P0/P1/P2)

| 우선순위 | 정의 | 판단 기준 |
|---|---|---|
| **P0** | 답이 없으면 **견적·범위 확정 또는 안전한 TOS 연계가 불가능**한 것 | 이 질문의 답에 따라 작업 범위 자체가 생기거나 사라진다 / 답이 없으면 운영 안전성을 보증할 수 없다 |
| **P1** | 답이 없으면 **설계·검증 계획에 큰 재작업 위험**이 있는 것 | 범위는 유지되지만 공수 산정·검증 방법이 흔들린다 |
| **P2** | 답이 없어도 진행은 가능하나 **품질·위생 측면에서 정리가 필요한** 것 | 문서화·정합성·운영 편의 수준 |

### 1.4 확인 주체 구분

| 구분 | 절 | 주체 | 성격 |
|---|---|---|---|
| A | §3 | Service 2 담당 PM | 범위·계약·조직 합의·정책 결정이 필요한 사항 |
| B | §4 | Service 2 개발·운영 담당자 | 운영 호스트·형상관리·실측으로 답할 수 있는 사항 |
| C | §5 | 당사(발주자) 내부 확인 | 대외 인용 수치·문서 정합성·내부 방침 결정 |
| D | §6 | TOS 벤더 / 고객사 IT (일부 항목은 GPS 단말·텔레매틱스 벤더) | 우리 저장소 밖 시스템의 사양·권한·SLA |

> 하나의 질문이 두 주체에 걸치는 경우, **주 응답자**가 있는 절에 싣고 "협의 대상"을 비고로 표시했다.

---

## 2. P0 요약표 (견적·범위·안전한 TOS 연계에 필수)

canon 확정 사실표 §10의 P0 12건이다. 이 12건이 닫히기 전에는 견적 금액·기간·범위를 확정할 수 없다.

| ID | 우선순위 | 질문(요약) | 확인 주체 | 상태 | 근거 |
|---|---|---|---|---|---|
| **P0-1** | P0 | TOS 측에 배차 권고를 소비할 인터페이스(테이블/API/메시지)가 존재하는가. 스키마·주기·인증·승인 절차는? | A(PM) · D(TOS 벤더) | 미확인 | `crates/api/src/main.rs:1-3`, [문서] `kc/start/launch-plan.html` A1/A2 미착수 |
| **P0-2** | P0 | TOS가 요구하는 배차 대상 식별자는 무엇인가(작업지시 ID/MSNSEQ/컨테이너번호). 현재 산출물은 버킷 단위라 개별 지시를 지목할 수 없다 | A(PM) | 미확인 | `db/migrations/0052_stage2_match_shadow.sql:5-22`, `crates/api/src/workpool.rs:796-822` |
| **P0-3** | P0 | `remote-toolbox-sql`(저장소 밖, `SKILL_DIR` 하위)의 실제 접속 계정·권한이 읽기 전용으로 강제되는가. 이관 범위에 포함되는가? | B(개발·보안) · D(고객사 IT) | 미확인 | `crates/extractor/src/runner.rs:41-72`, `crates/scengen/src/toolbox.rs:44-73` |
| **P0-4** | P0 | Stage-2 권고를 실제 배차 담당자가 참고하는 절차가 존재하는가(= Recommendation 운영인가, 미사용 Shadow인가) | A(PM) | 미확인 | `crates/api/src/workpool.rs:935-936`, `crates/api/src/livemap.rs:3968-3972` |
| **P0-5** | P0 | 우리 시스템 정지 시 TOS 기본 배차로 자동 복귀됨이 TOS 측에서 보장되는가(부분 적용 시 폴백 계약) | A(PM) · D(TOS 벤더) | 미확인 | [문서] `kc/start/launch-plan.html` C1 미착수, 코드 레벨 자동 강등 로직 **[없음]** |
| **P0-6** | P0 | 효과 판정 기준을 고객 세션의 수치 목표(공차거리 −8%/−15% 등)로 할 것인가, kc의 정성 게이트로 할 것인가 | A(PM) | 상충 | [문서] `docs/` 2026-06-08 고객 세션 자료 vs `kc/dispatch/stage2-rollout.html` |
| **P0-7** | P0 | 최적매칭 이득의 공식 수치·정의·측정창을 하나로 확정(40% vs 38~43% vs −5.1%) | C(당사) | 상충 | [문서] `kc/dispatch/stage2-journey.html:40`, `kc/data/tos-verification.html:59`, `kc/start/launch-plan.html:32`; 코드 정의도 2종 `crates/api/src/livemap.rs:4476` vs `crates/api/src/workpool.rs:921-928` |
| **P0-8** | P0 | TT 총 대수·교대당 가동 대수의 권위 있는 값은 무엇인가 | A(PM) | 상충 | [문서] TT~280 / TOS 야드트랙터 495 / 웹소켓 관측 539 / 엔진ON 동시 ~138 (`kc/data/websocket-coverage.html:66` 등). 코드 상수 **[없음]** |
| **P0-9** | P0 | 현장 이벤트 발생 → TOS Oracle 반영(UPD_DT/JOB_HIST) 까지의 실제 지연은 얼마인가 | B(개발) · D(TOS 벤더) | 미확인 | 저장소에는 우리 폴링 주기만 존재(`deploy/systemd/*.timer`), 상류 지연 측정 코드 **[없음]** |
| **P0-10** | P0 | Azure `tos_etw_gateway`의 운영 주체·감싸는 TOS RPC·인증·SLA·버전 정책은? 발주서의 '공통 게이트웨이'와 동일 대상인가? | A(PM) · D(TOS 벤더) | 미확인 | `crates/extractor/src/workpool.rs:111-170` |
| **P0-11** | P0 | `wp-api.service`·`wp-etw-bridge.service` 유닛과 crontab 2건을 저장소로 형상관리할 수 있는가 | B(개발·운영) | 확인(사실)·미확정(방침) | [호스트] 두 유닛 가동·crontab 2건 등록 확인 / `deploy/systemd/` 내 해당 유닛 **[없음]** |
| **P0-12** | P0 | GPS 피드 `userid`(운전자 ID+전화번호) 무마스킹 노출에 대한 개인정보 처리 정책은? | A(PM·보안) · C(당사) | 미확인 | `crates/api/src/livemap.rs:3234`, `/api/livemap/positions` 응답에 포함 |

> 위 12건 외에 §3~§6에 P1 25건·P2 12건을 정리했다. P1/P2는 견적 자체를 막지는 않지만 공수 편차의 원인이 된다.

---

## 3. (A) Service 2 담당 PM 확인 사항

범위·계약·조직 합의·정책 결정이 필요한 항목이다.

| 우선순위 | 질문 | 확인 주체 | 필요한 이유 | 미확인 시 영향 | 근거 |
|---|---|---|---|---|---|
| **P0** | (P0-1) TOS 제품에 우리 배차 권고를 받아들이는 인터페이스(테이블/API/메시지)가 존재하는가? 존재한다면 스키마·호출 주기·인증 방식·승인 절차는 무엇인가? | PM (TOS 벤더와 협의) | 저장소 어디에도 TOS 쓰기 코드가 없고(TOSADM 대상 DML/MERGE/프로시저 호출 검색 무히트), 실배차 연동 방식이 통째로 미정이다 | Live 승격 견적의 최대 미지수. 인터페이스 형태(파일/테이블/REST/큐)에 따라 공수가 수 배 차이 난다 | `crates/api/src/main.rs:1-3`(API 크레이트는 Oracle/SSH 접근 없음); [문서] `kc/start/launch-plan.html` A1(권고 출력 채널)·A2(TOS 소비 계약) 미착수 |
| **P0** | (P0-2) TOS가 배차 지시를 받을 때 요구하는 **대상 식별자**는 무엇인가 — 작업지시 ID(MSNSEQ), 컨테이너번호, 그 밖의 키 중 어느 것인가? | PM | 현재 산출물 `stage2_match_shadow`에는 컨테이너번호·작업지시 ID가 없고 (ts, ytno, qc, vessel, queuename, jobtype, src_block) 버킷 단위 행만 남는다 | 식별자가 확정되지 않으면 출력 스키마와 **매칭 입력 단위(버킷→개별 지시) 재설계**가 필요해 견적이 크게 흔들린다 | `db/migrations/0052_stage2_match_shadow.sql:5-22`; `crates/api/src/workpool.rs:796-822` |
| **P0** | (P0-4) 현재 Stage-2 권고를 실제 배차 담당자가 참고하는 절차가 존재하는가? 존재한다면 누가 언제 어떤 화면을 보고 무엇을 하는가? | PM | 코드는 "표시까지"만 보장하며(주석에 "never drives live dispatch"), 사람 개입 절차는 저장소 밖 사안이다 | "Recommendation 운영"인지 "미사용 Shadow"인지에 따라 다음 단계 범위·검증 요구·효과 측정 방법이 모두 달라진다 | `crates/api/src/workpool.rs:935-936`; `crates/api/src/livemap.rs:3968-3972`; `db/migrations/0052_stage2_match_shadow.sql:1-2` |
| **P0** | (P0-5) 우리 시스템이 정지하면 TOS 기본 배차로 자동 복귀되는가? 부분 적용(일부 QC/일부 트럭)일 때의 폴백 계약은 문서로 존재하는가? | PM (TOS 벤더와 협의) | 지금은 그림자라 자명하지만, 부분 적용 단계의 폴백은 코드에도 문서에도 없다 | 폴백 계약이 없으면 부분 적용 자체가 고객사 안전 승인을 통과하지 못한다 | [문서] `kc/start/launch-plan.html` C1 미착수; 저장소 내 자동 강등·폴백 로직 **[없음]** |
| **P0** | (P0-6) 효과 판정 기준을 고객 세션 자료의 수치 목표(공차거리 −8%/−15% 등)로 할 것인가, 지식센터의 정성 게이트(현장 거부율 등)로 할 것인가? 계약 기준은 어느 쪽인가? | PM | 판정 기준이 두 갈래로 존재해 Go/No-Go 판정을 내릴 수 없다 | 인수 단계에서 "효과 미달" 분쟁의 직접 원인이 된다 | [문서] `docs/` 2026-06-08 고객 세션 자료 vs `kc/dispatch/stage2-rollout.html` |
| **P0** | (P0-8) 운영 중인 TT(야드 트랙터) 총 대수와 교대당 실제 가동 대수의 **권위 있는 값**은 몇 대인가? GPS 단말 장착 대수는 별도로 몇 대인가? | PM | 장비 대수가 처리량·매칭 규모·수집 대역·사이징의 1차 변수인데 코드에 권위 있는 정의가 없고 문서 관측치가 서로 다르다 | 견적 규모 변수 전체가 흔들린다. GPS 커버리지 KPI의 분모가 정해지지 않아 'lost %'가 크게 요동한다 | [문서] `kc/data/websocket-coverage.html:66`(관측치); 코드 상수 **[없음]** |
| **P0** | (P0-10) Azure `tos_etw_gateway`는 누가 운영하며 어떤 TOS RPC를 감싸는가? 인증·SLA·버전 정책은? 발주서의 '공통 게이트웨이'와 동일 대상인가? | PM (TOS 벤더와 협의) | 현 구현이 이미 이 게이트웨이에 의존한다(90초 워크풀 틱마다 `curl -m 8`로 항차 스냅샷 조회) | 게이트웨이 계약이 불명확하면 ETW 기반 마감 산정 전체와 향후 전달 경로의 범위·공수를 산정할 수 없다 | `crates/extractor/src/workpool.rs:111-170`(`ETW_GATEWAY_URL`, `wp-etw-bridge` SSH 터널 경유, 실패 시 warn 로그 후 스킵) |
| **P0** | (P0-12) GPS 피드의 `userid`(운전자 ID + 전화번호)가 마스킹 없이 API 응답·화면에 노출되는데, 고객사 개인정보 처리·보관 정책상 허용되는가? 마스킹·보존 요건이 부과되는가? | PM(보안) · 당사 | 위치 데이터가 개인 식별자와 결합된 상태로 유통되고 있다 | 요건이 부과되면 파서·API 응답·프론트 표시·(필요 시) 스키마 수정이 추가 범위가 된다 | `crates/api/src/livemap.rs:3234`(`userid` 파싱); `/api/livemap/positions` 응답 포함. DB 영속 저장은 마이그레이션에서 확인되지 않음 |
| **P0** | 출력 채널(A1)과 TOS 소비 계약(A2)을 **누가 언제** 확정하는가? TOS팀과의 합의 일정이 잡혀 있는가? | PM | 지식센터가 A1→A2를 크리티컬 패스이자 조직 합의 대상으로 명시하고 둘 다 미착수로 표시했다 | 확정 일정이 없으면 그림자→자문 전환 일정과 그 이후 견적을 산정할 수 없다 | [문서] `kc/start/launch-plan.html:44-47` |
| **P0** | 고객 세션에서 제시된 일정(UAT 8/3, 1단계 실투입 8/17, 2단계 10/30)은 여전히 유효한가, 재협의되었는가? | PM | 현재 저장소 상태는 Phase 0 그림자이고 A1/A2/D2가 모두 미착수다. 또한 "거리만 쓰는 1단계"는 실제로 존재하지 않는다(비용은 처음부터 시간 기반) | 일정 전제가 틀린 채로 견적이 나가면 착수 직후 재협의가 불가피하다 | [문서] `docs/` 2026-06-08 고객 세션 자료 vs 현행 코드(`crates/api/src/livemap.rs:3823-3825` 비용 정의) |
| **P0** | Stage-2 추천을 실제 TOS 배차로 되돌리는 경로(쓰기 인터페이스·승인·롤백)를 **이번 계약 범위에 포함**하는가? | PM | 현 코드는 그림자 기록만 하며 운영 반영 코드가 전혀 없다 | '배차 자동화' 기대와 실제 구현 범위가 어긋나면 가장 큰 공수 누락이 발생한다 | `crates/api/src/livemap.rs:4466`(`stage2_match_shadow` INSERT)만 존재; TOS 쓰기 **[없음]** |
| P1 | 운영자가 권고를 채택/기각한 사실을 어떤 경로로 기록할 것인가? TOS 화면 로그를 쓸 수 있는가, 별도 UI를 만들어야 하는가? | PM | 롤아웃 게이트 기준이 "현장 거부가 적다"인데, 저장소에 override/adopt/reject 수집 코드·테이블이 없다 | 게이트 판정을 위한 데이터를 만들 수 없어 Phase 1→2 전환 근거가 성립하지 않는다 | [문서] `kc/dispatch/stage2-rollout.html`; 채택/거부 수집 코드 **[없음]** |
| P1 | 배차 지시를 실제로 보낼 때 요구되는 전달 보증 수준(커맨드 ID·Ack·재전송·순서 보장·멱등성 키)은 무엇인가? | PM | 저장소의 멱등성은 Postgres upsert 수준뿐이고 지시 전달용 커맨드/Ack 개념이 전혀 없다 | 단순 REST 폴링과 신뢰성 큐는 공수가 배 이상 차이 난다 | `db/migrations/0092_tt_move_log.sql:41`(upsert 수준); 커맨드/Ack 개념 **[없음]** |
| P1 | 하드코딩된 크레인 처리량 상수(`NEED_HORIZON_S`=900초, `DS_MOVE_S`=90, `LD_MOVE_S`=110)가 현장 실측과 맞는가? 누가 이 값을 승인하는가? | PM | 이 값들이 크레인당 트럭 수요 상한을 직접 결정하는데, work-ETA 쪽은 학습값(`learn_qc_move_time`)을 쓰는 **이중 기준**이다 | 상한이 어긋나면 특정 QC에 트럭이 몰리거나 반대로 굶는 QC가 생긴다 | `crates/api/src/livemap.rs:3826-3831` |
| P1 | 현행 TOS 배차 알고리즘(고객 세션의 "즉시 1:1·최근접·사후 스왑" 서술)에 대한 **고객 확인 문서**가 있는가? | PM | 이 AS-IS 서술이 비교 baseline의 전제로 쓰이는데 저장소에서 검증한 근거가 없고, 시뮬레이터 기획은 오히려 "TOS 알고리즘 역공학 금지"라고 적었다 | baseline 전제가 틀리면 절감률 주장 전체의 해석이 달라진다 | [문서] `docs/` 2026-06-08 고객 세션 자료; 저장소 내 검증 근거 **[없음]** |
| P1 | scengen(시나리오 서브시스템)의 Oracle 접근(신규 4개 객체, 10~15분 주기)이 고객사에 **사전 고지·승인**된 것인가? | PM | `scenario.config.enabled` 기본값이 true이고, 지식센터의 추출 단일출처 문서(18지점)에는 이 접근이 기재돼 있지 않다 | 미고지 부하로 신뢰 이슈가 생기거나, 급히 꺼야 해서 시나리오 기능 일정이 밀린다 | `db/migrations/0093_scenario.sql:13`; [문서] `kc/data/tos-extraction.html` 누락 |
| P1 | 맵매칭(`mm_arrival_shadow`)을 섀도에서 운영으로 승격할 것인가? 승격 기준(도착 포착률 목표)은? | PM · 당사 | 현재 명시적으로 라이브 미반영 상태이며, 도착 포착 개선의 유일한 대기 지렛대다 | 승격 여부에 따라 검증 범위와 도착 판정 품질 목표가 달라진다 | `db/migrations/0090_mm_arrival_shadow.sql:1-4` |
| P2 | 동일 최소비용 해가 복수일 때(타이) 어떤 트럭을 고를지에 대한 **운영 규칙**이 필요한가? | PM | 매처에 명시적 tie-break가 없어 엣지 삽입 순서에 의존한다 | 추천이 미세하게 흔들려 보이면 현장 신뢰가 떨어진다 | `crates/api/src/livemap.rs:3855-3966`(Mcmf 구현부에 tie-break 규칙 **[없음]**) |
| P2 | 마감 위반(`feasible=false`) 매칭을 그대로 추천해도 되는가, 필터링해야 하는가? | PM | 현재 feasibility는 필터가 아니라 사후 라벨이며, 늦는 매칭도 그대로 채택된다 | Live 승격 시 필터 도입 여부에 따라 커버리지와 지각률이 트레이드오프된다 | `crates/api/src/livemap.rs:4452-4474` |
| P2 | DigiPort 또는 상위 KPI 계층으로 데이터를 내보내야 하는 요구가 실제로 존재하는가? | PM | 저장소에 해당 연계 코드·계약이 전혀 없어 요구 자체를 확인할 수 없다 | 요구가 있다면 완전 신규 범위다 | webhook/kafka/mqtt/S3/CSV export 코드 **[없음]** |
| P2 | Tomorrow.io 등 외부 인터넷 API 사용이 고객 보안정책상 허용되는가? 폐쇄망 전환 시 대체 소스는? | PM | 이동시간 모델 피처가 외부 무료 API(Open-Meteo 매시 / Tomorrow.io 3분)에 의존한다 | 차단 시 기상 피처가 결측되어 모델 재학습이 필요해진다 | `crates/extractor/src/weather.rs:1-33`, `:73-99`; `deploy/systemd/wp-weather.timer:5-7` |
| P2 | 부하시험 기준(동시 사용자 수, 목표 응답시간, 피크 물량)을 무엇으로 잡을 것인가? | PM | 저장소에 벤치마크가 전무하고, 용량 문서도 "부하 테스트 후 확정"이라고 스스로 단서를 달았다 | 성능 검증 공수를 견적에 계상하지 못하면 인수 단계 분쟁이 된다 | [문서] `kc/reference/capacity-planning.html`; 벤치마크 산출물 **[없음]** |

---

## 4. (B) Service 2 개발·운영 담당자 확인 사항

운영 호스트 조회·형상관리·짧은 실측으로 답할 수 있는 항목이다. 아래 몇 건은 2026-07-22 [호스트] 관찰로 1차 확인됐으나, 저장소 밖 관찰이므로 **재확인 및 형상관리 반영 방침**이 여전히 열려 있다.

| 우선순위 | 질문 | 확인 주체 | 필요한 이유 | 미확인 시 영향 | 근거 |
|---|---|---|---|---|---|
| **P0** | (P0-3) `remote-toolbox-sql`(저장소 밖, `SKILL_DIR` 하위)은 어떤 Oracle 계정·권한·DSN·세션 파라미터로 접속하는가? 읽기 전용이 **DB 권한으로 강제**되는가? 이 스크립트는 이관 범위에 포함되는가? | 개발·운영 (보안 검토 동반) | 저장소의 모든 Oracle 접근이 이 외부 스크립트에 위임되어 있고, `run_sql`은 임의 SQL 문자열을 그대로 넘긴다 — SELECT 강제·DML 거부 가드가 코드에 없다 | "TOS 쓰기 불가"의 최종 보증 근거가 저장소로 검증되지 않는다. 보안 검토에서 권한 분리 증빙을 요구받으면 답할 수 없다. 이관 목록에서 빠지면 운영 개시 자체가 불가능하다 | `crates/extractor/src/runner.rs:41-72`(타임아웃 90초, 프로세스 전역 `ORACLE_LOCK`); `crates/scengen/src/toolbox.rs:44-73` |
| **P0** | (P0-9) 현장 이벤트 발생 → TOS Oracle 반영(`UPD_DT`/`JOB_HIST_DATE`+`TIME`)까지의 실제 지연을 실측할 수 있는가? 측정 방법은? | 개발 (TOS 벤더 협조 필요할 수 있음) | 우리 폴링 주기(60~90초)는 알지만 그 앞단 지연은 전혀 측정되지 않아 종단 지연을 말할 수 없다 | 근거 없이 '실시간' SLA를 약속하게 되어 계약 리스크가 된다 | 상류 지연 측정 코드 **[없음]**; 폴링 주기는 `deploy/systemd/*.timer` |
| **P0** | (P0-11) `wp-api.service`·`wp-etw-bridge.service` 유닛 정의와 crontab 2건을 저장소로 형상관리할 수 있는가? 불가하다면 이유와 대안 보관 위치는? | 개발·운영 | 배차 엔진 호스트 프로세스(GPS 수집·Stage-2 그림자 등 24개 백그라운드 태스크를 담는 프로세스)와 ETW 터널 정의가 형상관리 밖에 있다. crontab의 매시 도로망 재추론은 **배차 비용의 본체**를 생산한다 | 이관·재해복구 절차가 성립하지 않는다. 이관 시 도로망 갱신이 멈추면 비용 곡선이 서서히 왜곡된다 | [호스트] 두 유닛 가동·crontab 2건 등록; `deploy/systemd/`에 해당 유닛 **[없음]**; `crates/api/src/main.rs:115-139`; `scripts/reinfer_roadgraph.sh:1-6`; `scripts/travel_gbm_shadow.py:1-6` |
| **P0** | 운영 API는 Postgres에 **어떤 역할(role)로** 접속하는가 — `wp_ro`(읽기전용)인가 쓰기 권한 역할인가? `db/grants.sql`의 `wp_ro`는 현재 사용되는가, 폐기된 계획인가? | 개발·운영 | README와 `db/grants.sql`은 읽기전용 `wp_ro`를 선언하지만 API는 다수 테이블에 INSERT/DELETE를 수행한다. `.env` 값은 조사 규칙상 확인하지 않았다 | 보안·감사 문답에서 "문서와 실제가 다르다"는 지적을 받는다. 폐기 상태라면 명시적으로 정리해야 한다 | `README.md:75-76`; `db/grants.sql`; `crates/api/src/livemap.rs:1497` 외 INSERT 다수 |
| **P0** | 도로망 재추론(`reinfer_roadgraph.sh`)의 실행 주체·주기·**최근 성공 시각**은? 실패 시 알림 경로가 있는가? | 개발·운영 | 그래프가 비거나 낡으면 모든 OD 비용이 L3 맨해튼 폴백으로 떨어진다 | 매칭 품질과 `cost_tier` 지표가 동시에 왜곡되는데, 알아챌 신호가 없다 | `crates/api/src/roadgraph.rs:1-9`, `:47-92`; [호스트] crontab 매시 11분 |
| P1 | 운영 호스트의 crontab 실제 등록 내용(전체 라인)은 무엇이며, 평문 DB 비밀번호를 환경변수 주입 방식으로 바꿀 계획이 있는가? | 개발·운영(보안) | 스크립트와 crontab에 평문 DB 비밀번호가 존재한다는 사실이 확인됐다(값은 본 문서에 옮기지 않음). `.env` 권한도 0644다 | 외부 인수·형상관리 시 비밀 회전 절차가 별도 범위로 필요하다 | [호스트] crontab 2건; `scripts/reinfer_roadgraph.sh`, `scripts/estimate_equipment_specs.sh`, `scripts/travel_gbm_shadow.py`, `db/grants.sql` |
| P1 | Postgres 백업·복구 정책은 무엇이며, 현재 DB 용량과 일일 증가율(특히 상위 10개 테이블)은 얼마인가? | 개발·운영 | 저장소에 백업 스크립트가 전무하고(`pg_dump` 히트 0건), 보존은 코드 곳곳의 인라인 DELETE(2~30일)뿐이다. 학습 산출물 `data/travel_gbm.pkl`은 `.gitignore` 제외다 | 용량·스토리지 견적의 근거가 비어 있고, 호스트 손실 시 수개월치 학습 자산이 소멸한다 | `.gitignore:1-8`; `crates/api/src/livemap.rs:4675,4728,4829`(인라인 DELETE); `scripts/travel_gbm_shadow.py:11-13` |
| P1 | DB 마이그레이션은 어떤 절차로 적용되며 적용 이력 테이블이 있는가? `0098` 번호 중복(`0098_scenario_yard_block.sql`, `0098_tt_cycle_recon.sql`)은 어떻게 정리할 것인가? | 개발·운영 | 코드에 적용기 호출이 없고(`sqlx::migrate!` 0건) 문서화된 절차도 없다 | 환경 간 스키마 드리프트를 확인할 수 없고, 자동화 도입 시 번호 중복이 즉시 충돌한다 | `db/migrations/`(104개, 0098 중복); `Cargo.toml:26-29` |
| P1 | `live_candidate` 추출 필터(`JOBSTATUS='Q'` AND YTNO 공백, `CRE_DT >= TRUNC(SYSDATE)-2`)가 실제 미배차 수요를 빠짐없이 포괄하는가? P(계획)/B(블록) 상태와 2일 창 밖 작업의 실제 비중은? | 개발·운영 | 해당 상태·기간의 작업이 통째로 제외되고 있다 | 수요가 누락되면 매칭이 과소 공급으로 편향되고, 절감률 비교의 분모도 달라진다 | `crates/extractor/sql/workpool.sql:14-38`, `:34-36` |
| P1 | GPS 웹소켓의 실제 초당 메시지 수와 평균 메시지 크기는? (문서가 ~40건/초와 ~965건/분으로 상충) | 개발·운영 | `/api/livemap/health`의 `rate_per_min`을 며칠 로깅하면 확정 가능하다(코드 이미 존재) | 수집 CPU·대역·인프라 비용 견적 기준이 2.5배 차이 난다 | [문서] 상충; 코드에는 `rate_per_min` 카운터만 있고 저장 로그 **[없음]** |
| P1 | 90초 워크풀 쿼리(`JOB_ORDER_LIST` A+Q)가 실제로 반환하는 행 수의 분포(평시/피크)는? | 개발·운영 | 이 쿼리만 FETCH 캡이 없어 상한이 열려 있고, Oracle 접근이 직렬화되어 지연이 전 파이프라인에 전파된다 | 물량 피크 시 작업풀 갱신이 밀려 화면이 FROZEN(300초 임계)으로 넘어갈 수 있다 | `crates/extractor/sql/workpool.sql:14-38`; `web/src/TtPage.tsx:583` |
| P1 | `remote-toolbox-sql`의 프로세스 간 Oracle 직렬화(파일 락 등)가 실제로 구현돼 있는가? 타임아웃·재시도 정책은? | 개발·운영 | extractor와 scengen이 각자 프로세스 내 Mutex만 갖고 있고, 프로세스 간 직렬화는 "스크립트에 있다"는 주석뿐이다 | 타이머가 겹치는 시각에 Oracle 세션 2개가 동시에 떠 부하 상한 가정이 깨진다 | `crates/extractor/src/runner.rs:41-72`; `crates/scengen/src/toolbox.rs:44-73` |
| P1 | 현재 운영 배포본(`target/release` 바이너리)은 **어느 커밋에서 빌드**된 것인가? 릴리스 태깅·버전 표기 방법을 도입할 수 있는가? | 개발·운영 | CI/CD와 릴리스 태깅이 없고(CI/CD 산출물 0건) 미커밋 변경 5건이 남아 있다 | 장애 원인 분석 시 어떤 코드가 돌고 있는지 확정할 수 없다 | CI/CD·Dockerfile·IaC 산출물 **[없음]**; `deploy/systemd/README.md:19-38`(수동 빌드·복사) |
| P1 | `approaching` TT 상태는 폐기된 것인가, 회귀 버그인가? | 개발 | `classify_tt`가 `approaching`을 반환하지 않는데도 후보 필터와 `learn_free_in_bias` 버킷에는 남아 있다 | 죽은 코드라면 문서·모델 설명이 실제와 어긋난 채로 유지된다 | `crates/api/src/livemap.rs:915-1024`(classify_tt), 후보 필터 및 학습 버킷에 잔존 |
| P1 | Stage-2가 Oracle 미러(`live_workpool`)의 신선도를 검사하지 않는 것은 의도인가? 신선도 게이트 도입 여부를 누가 결정하는가? | 개발 (PM 협의) | 매칭은 GPS 연결 여부만 게이트로 삼는다. 300초 FROZEN 판정은 프론트 전용이다 | 워크풀 타이머가 죽어도 낡은 작업목록으로 매칭이 계속 산출·기록되어 그림자 지표가 조용히 오염된다. 실배차 승격 시 곧바로 오배차 경로가 된다 | `crates/api/src/livemap.rs:4173-4183`; `crates/api/src/workpool.rs:183-187`; `web/src/TtPage.tsx:583` |
| P1 | `scenario.config.enabled`는 현재 운영 DB에서 true인가 false인가? | 개발·운영 | 기본값은 true지만 UI에서 뒤집을 수 있어 현재 상태는 DB를 봐야 안다 | scengen이 프로덕션 Oracle을 5~15분 주기로 추가 조회 중인지 여부가 달라진다(고객 고지·부하 산정에 직결) | `db/migrations/0093_scenario.sql:13` |
| P1 | scengen serve(0.0.0.0:8899)와 대시보드 API(127.0.0.1:8080)의 접근 통제는 네트워크 경계 외에 무엇이 있는가? | 개발·운영 | scengen 웹은 인증 없이 킬스위치 POST를 노출하고, API는 `CorsLayer::permissive()`이며 인증 계층이 없다 | 내부망 확장 시 임의 사용자가 시나리오 수집을 정지시키거나 데이터를 열람할 수 있다 | `crates/scengen/src/serve.rs:34-41`; `crates/api/src/main.rs:82` |
| P2 | `truck_pos_hifreq`의 실제 운영 보존 기간은 1일인가 5일인가? | 개발·운영 | 스크립트 주석(~1일)과 마이그레이션·DELETE 쿼리(5일)가 상충한다 | 1일이면 `tt_cycle_recon`의 GPS 결합률이 조용히 낮아져 사이클 분해 품질이 떨어진다 | 마이그레이션·DELETE(5일) vs 스크립트 주석(~1일) |
| P2 | `MCH_OPERATION`의 QC 식별 기준이 KPI마다 `^C[0-9]+$` 와 `^[CMZ][0-9]+$` 로 다른데 어느 쪽이 올바른가? | 개발 (TOS 벤더 확인 동반) | c07/c10/f2/e1c는 `^C`만 쓰고, `qc_move_time`·`qc_moves`는 C/M/Z를 쓴다 | M/Z 크레인이 실재한다면 K_MPH·K_QC_Q 등 주요 KPI가 일부 크레인을 누락한 채 산출된다 | `crates/extractor/sql/c07_k_mph_realtime.sql:5-25` vs `crates/extractor/sql/qc_move_time.sql:7-32` |
| P2 | oneshot 추출 유닛에 `Restart=`/`OnFailure=`를 붙이고 실패 알림(push) 경로를 만들 계획이 있는가? | 개발·운영 | 현재 실패 시 재시도 없이 다음 주기까지 대기하며, Prometheus·Grafana·OTel·Sentry·webhook·SMTP·`OnFailure=`가 전부 0건이라 감지는 "사람이 화면을 봐야 하는" pull 방식뿐이다 | 조용한 결손을 늦게 발견하며, 운영 SLA를 약속할 근거가 없다 | `deploy/systemd/`(oneshot 유닛에 Restart/OnFailure **[없음]**) |
| P2 | 워터마크 캡 도달을 extractor(handover/qc/rtg)에서도 알릴 계획이 있는가? 워터마크가 미래 날짜 이상치로 튀었을 때의 복구 절차는? | 개발·운영 | 워터마크는 `GREATEST`로만 전진해 롤백이 불가하고, 캡 도달 경보는 scengen만 `gen_event`에 남긴다 | 자정을 넘겨 정지하면 전날 미수집분이 영구 복구 불가이며, 이상치 1건으로 이후 정상 데이터가 영구 누락될 수 있다 | `crates/extractor/src/handover.rs:57-68`; `crates/extractor/src/qc_moves.rs:53-65`; `crates/extractor/src/rtg_moves.rs:53-65` |

---

## 5. (C) 당사(발주자) 내부 확인 사항

대외 인용 수치, 문서 정합성, 내부 방침에 관한 항목이다.

| 우선순위 | 질문 | 확인 주체 | 필요한 이유 | 미확인 시 영향 | 근거 |
|---|---|---|---|---|---|
| **P0** | (P0-7) 최적매칭 이득의 **공식 수치·정의·측정창**을 하나로 확정할 수 있는가 — 40%인가, 38~43%인가, −5.1%인가? 기준선은 단순 그리디인가 TOS 실적인가? | 당사 (제안·영업 포함) | 세 문서의 수치가 크게 다르고, 앞 둘은 "그리디 대비 총 도착시간", 뒤는 "공차시간"이라 지표 정의·창·기준선이 서로 다르다. 코드 내 정의도 둘이다 | 대외 인용 실수 시 신뢰도에 치명적이며, 계약상 효과 판정 기준과도 직결된다 | [문서] `kc/dispatch/stage2-journey.html:40`, `kc/data/tos-verification.html:59`, `kc/start/launch-plan.html:32`(세 페이지 모두 정적 HTML 하드코딩); `crates/api/src/livemap.rs:4476`(`gap_pct=(greedy−opt)/opt`) vs `crates/api/src/workpool.rs:921-928`(`savings_pct=(greedy−opt)/greedy`) |
| **P0** | `fair_compare` 절감 상한 "~30%"의 근거는 무엇이며 현재 표본에서 재현되는가? | 당사 | 절감 주장의 천장이자 시뮬레이터 달성가능분의 분모로 쓰인다 | 재현되지 않는 수치를 제안서에 실으면 인수 단계에서 반박된다 | `crates/api/src/livemap.rs:4930-5000`(fair_compare, n<4 스킵·MAX_N=120 절단) |
| P1 | 지식센터가 서술하는 라이브 수치(선박 데드라인 243척, 2단계 실현가능성 81.8%, 작업지점 5,888곳 등)는 현재도 유효한가? 갱신 주기와 책임자는? | 당사 | 정적 HTML에 박제된 스냅샷 값이라 저장소만으로는 현재값을 검증할 수 없다 | 대외 자료에 낡은 수치가 그대로 인용된다 | [문서] `kc/` 정적 HTML 페이지들 |
| P1 | canon §9에 정리된 문서 상충 10건(README 미갱신, `deploy/systemd/README.md` 5종만 문서화, `kc/data/tos-extraction.html` 추출점 누락, 0052 주석의 cost_tier, cycle-v2 설계문서, live-map-dev-guide 상태 5종 등)을 **누가 언제** 정정하는가? | 당사 | 이 문서들이 외부 인용 및 인수인계의 기준 자료로 쓰인다 | 인수 상대가 문서를 사실로 믿고 설계하면 재작업이 발생한다 | `README.md`; `deploy/systemd/README.md`; [문서] `kc/data/tos-extraction.html`; `db/migrations/0052_stage2_match_shadow.sql:19`; `docs/cycle-detection-v2-design.md`; `docs/live-map-dev-guide.md` |
| P1 | GPS 이력 보존기간(현행 2/3/5일)을 늘려야 하는 분석·감사 요구가 있는가? 저장소 용량 제약은? | 당사 (개발 협의) | 프루닝이 API 프로세스 내부 DELETE에만 의존해, 프로세스가 죽으면 정리도 멈춘다 | 장기 재현·모델 재학습 가능 범위와 DB 사이징이 달라진다 | `crates/api/src/livemap.rs:4673,4726,4827` |
| P1 | 매칭 로직(Mcmf·classify_tt·비용 계산)에 대한 회귀 테스트 체계를 이번 범위에 포함할 것인가? | 당사 (PM 협의) | 매칭 로직 단위/회귀 테스트가 0건이고, API 크레이트의 유일한 테스트 모듈은 `periods.rs`다. 통합 테스트 3건은 실 Postgres를 요구한다 | 실배차 승격 단계에서 검증 체계 구축 비용이 별도로 발생하며, 로직 변경 시 회귀를 잡을 안전망이 없다 | `crates/api/src/livemap.rs:3855-3966`(대응 테스트 **[없음]**); `crates/extractor/tests/transform_pg.rs:1-20` |
| P2 | 트윈/탠덤 작업(1지시=2컨테이너)을 매칭 수요 산정에서 어떻게 취급할 것인가? | 당사 (개발 협의) | `Stage2Work`에 트윈 필드가 없어 수요가 버킷 수량 n으로만 다뤄진다. 트윈 정보는 표시·사이클 계측 쪽에만 존재한다 | 크레인별 수요 상한이 과대 산정될 수 있다 | `crates/api/src/workpool.rs:796-822`; `crates/api/src/livemap.rs:2790-2860`, `:4325-4366` |
| P2 | 조사 시점 미커밋 5건(`crates/api/src/cycles.rs`, `scripts/populate_tt_cycle_recon.sql`, `web/public/livemap-roadgraph.geojson`, `web/src/CyclesPage.tsx`, `web/src/api.ts`)은 언제 커밋되며, 본 조사 문서의 인용을 갱신할 필요가 있는가? | 당사 | 사이클 KPI와 프론트 API 계약이 미커밋 상태로 변경 중이다 | 커밋 후 인용한 수치·엔드포인트가 달라질 수 있다 | 조사 시점 `git status` 5건 |
| P2 | scengen/scenario 서브시스템의 운영 절차(킬스위치·백필·장애 시 조치)를 문서화할 계획이 있는가? `scengen backfill`이 미구현 스텁인 상태를 언제까지 유지하는가? | 당사 | 현재 근거가 마이그레이션 주석뿐이고, 과거 구간 시나리오 요청이 오면 즉시 대응할 수 없다 | 인수인계 리스크이며, 과거 구간 요구가 오면 신규 개발이 필요하다 | `crates/scengen/src/main.rs:78-84`("skeleton stub — not yet implemented") |

---

## 6. (D) TOS 벤더 / 고객사 IT 확인 사항

우리 저장소 밖 시스템의 사양·권한·SLA에 관한 항목이다. **일부는 GPS 단말·텔레매틱스 벤더** 대상이며 비고에 표시했다.

| 우선순위 | 질문 | 확인 주체 | 필요한 이유 | 미확인 시 영향 | 근거 |
|---|---|---|---|---|---|
| **P0** | TOS 제품이 외부 배차 권고를 수용하는 **기술 사양**(테이블/API/메시지, 필드, 인증, 호출 제한)을 문서로 제공할 수 있는가? | TOS 벤더 | P0-1의 기술 측면. 우리 저장소에는 TOS 쓰기 경로가 전혀 없다 | 인터페이스 사양 없이는 출력 채널 설계·공수 산정이 불가능하다 | `crates/api/src/main.rs:1-3`; TOSADM 대상 DML **[없음]** |
| **P0** | 외부 권고가 끊겼을 때 TOS 기본 배차 로직이 그대로 동작함을 벤더가 보증할 수 있는가? 부분 적용 시 어떤 단위(QC/트럭/작업유형)로 분리 가능한가? | TOS 벤더 | P0-5의 기술 측면 | 폴백 보증 없이는 부분 적용 안전 승인이 나지 않는다 | [문서] `kc/start/launch-plan.html` C1 미착수 |
| **P0** | Azure `tos_etw_gateway`(`GET /v1/voyages/{vessel}/{voyage}/snapshot`)의 소유·운영 주체, 인증 방식, 가용성 SLA, 버전 정책은? | TOS 벤더 / 고객사 IT | 현 구현이 90초마다 이 게이트웨이를 호출하며, 실패 시 warn 로그만 남기고 조용히 스킵한다 | ETW 기반 마감 산정 전체가 계약 없는 의존에 놓인다 | `crates/extractor/src/workpool.rs:111-170` |
| **P0** | Oracle 접속 계정의 권한 부여 주체·현재 부여된 권한 목록(읽기 전용 여부)을 공식 확인해 줄 수 있는가? | 고객사 IT / DBA | P0-3의 상대측 답변. 읽기 전용 강제는 우리 코드가 아니라 Oracle 계정 권한에 달려 있다 | 감사 대응 근거가 없고, DML 가능 계정이면 운영 리스크가 남는다 | `crates/extractor/src/runner.rs:41-72`; SELECT 강제 가드 **[없음]** |
| **P0** | 현장 이벤트가 TOS Oracle에 반영되기까지의 전형적 지연을 벤더가 제시할 수 있는가(P0-9의 상대측 답변)? | TOS 벤더 | 우리 쪽에서는 상류 지연을 측정할 방법이 없다 | 종단 지연·SLA를 근거 없이 말하게 된다 | 상류 지연 측정 코드 **[없음]** |
| P1 | `JOB_ORDER_HISTORY` ~15일 / `MCH_OPERATION`·`VSS_STATISTICS` ≥35일 보존은 현재도 유효한가? 별도 아카이브 테이블이 존재하는가? | TOS 벤더 / 고객사 IT | 보존기간 근거가 README와 조사문서 서술뿐이고 DB 딕셔너리로 확인된 바 없다 | 과거 구간 재현·학습 데이터 확보 전략(축적 vs 백필)이 뒤집힌다 | `README.md:82-84` [문서] |
| P1 | 우리가 의존하는 인덱스(`IDX_MCH_OPERATION_COMPDATE`, `IDX_JOBHIST_DATETIME`)가 실제로 존재하고 유지되는가? 변경 시 사전 통지가 가능한가? | 고객사 IT / DBA | 코드 주석은 인덱스 존재를 전제하지만 저장소에 DDL이 없다 | 인덱스가 없거나 변경되면 5분 주기 폴링이 풀스캔이 되어 운영 DB에 부하를 준다 | `crates/extractor/src/qc_moves.rs:53-65`, `crates/extractor/src/handover.rs:57-68`(주석 전제); DDL **[없음]** |
| P1 | `MCH_OPER_STATUS`(F/M), `JOB_ODR_JOBSTATUS`(C/A/Q/P/B), `JOB_ODR_YT_STATUS`, `VSB_VOY_STATUS`의 **코드값 전체 목록과 공식 의미**를 제공할 수 있는가? | TOS 벤더 | 코드 주석에 일부만 적혀 있고 YT_STATUS·VSB_VOY_STATUS는 의미가 문서화돼 있지 않다 | 필터가 일부 상태를 조용히 누락시켜 KPI 분모가 틀어질 수 있다 | `crates/extractor/sql/workpool.sql:14-38`; `crates/extractor/sql/vessel_schedule.sql:6-22` |
| P1 | `CRNT_PSN_IDX_NO1~NO4`의 공식 디코딩 규칙(블록·베이·행·단)을 TOS 사양서로 확인할 수 있는가? | TOS 벤더 | 현재 근거는 "CYY.CLOCATION 대조로 검증했다"는 코드 주석뿐이고 사양 근거가 없다 | 야드 스택모델 전체(yard_cell·시나리오 t0)가 틀린 좌표계 위에 세워질 수 있다 | `crates/scengen/src/yard.rs:1-5`, `:49-63` |
| P1 | 동일 `(MACHNO, CONTNO, SEQNO)` 조합이 서로 다른 날짜에 재사용되는 경우가 실제로 있는가? | TOS 벤더 | `qc_move_log`/`rtg_move_log`의 PK가 이 3개 컬럼이고 `ON CONFLICT DO NOTHING`이라, 재사용 시 후속 이벤트가 유실된다 | 무브 로그 누락이 조용히 발생해 사이클·가동률 지표가 과소 집계된다 | `crates/extractor/src/qc_moves.rs:53-65`; `crates/extractor/src/rtg_moves.rs:53-65` |
| **P0** | (GPS 단말·텔레매틱스 벤더) GPS 웹소켓 피드의 공식 프로토콜 스펙(필드 계약·버전·SLA) 문서가 존재하는가? | 단말 벤더 | 저장소에는 역공학된 파서만 있고(이중 JSON 인코딩, `speed`는 문자열 휴리스틱 파싱) 공식 스키마 문서가 없다 | 필드 포맷이 바뀌면 조용히 속도 0·상태 오분류가 발생하며, TT 좌표의 **유일한 소스**가 무너진다 | `crates/api/src/livemap.rs:3186-3245` |
| **P0** | (GPS 단말 벤더) 단말이 "이동 시에만 보고"하는 설정을 변경할 수 있는가 — 정차 중 heartbeat 주기를 강제할 수 있는가? | 단말 벤더 | 정차 침묵이 유휴 onset·큐 대기·정밀 도착 관측 불가의 **근본 원인**으로 감사되어 있다 | 바꿀 수 없으면 곧-유휴/대기 예측 정확도가 영구 상한에 걸리고, 이를 전제로 KPI 목표를 잡아야 한다 | [문서] `kc/data/websocket-coverage.html:19,37-40`; [문서] `kc/dispatch/leadtime-adr.html` |
| **P0** | `wp-ws-bridge`(GPS SSH 터널) 대상 호스트의 소유·운영 책임은 누구인가? 장애 시 연락 체계와 복구 SLA는? | 고객사 IT / 단말 벤더 | 이 터널이 위치 파이프라인 전체의 단일 장애점이며, `deploy/systemd/README.md` 설치 절차에도 빠져 있다 | 장애 복구 절차·SLA를 견적에 넣을 수 없다 | `deploy/systemd/README.md:19-38`(ws-bridge 안내 **[없음]**) |
| P1 | (GPS 단말 벤더) 소스 타임스탬프 `dtime`의 정확한 의미·시간대·장비 시계 동기 방식은 무엇인가? | 단말 벤더 | 현재 코드는 `dtime`을 저장하지 않고 수신 시각만 쓴다(예외는 `arr_dtime` 하나) | 소스↔수집 지연을 분리 측정할 수 없어 사이클 시간 편향 크기를 정량화하지 못한다 | `crates/api/src/livemap.rs:3236`, `:4632` |

---

## 7. 질문별 확인 방법 제안 (P0 한정)

아래는 **확인 경로 제안**이며, 이 문서 작성 과정에서 실행하지 않았다.

| ID | 확인 방법 제안 |
|---|---|
| P0-1 | TOS 벤더와 1회 기술 세션 — 의제: "외부 시스템이 배차 지시를 투입할 수 있는 공식 인터페이스 목록과 사양서". 산출물은 인터페이스 사양서(필드·인증·호출 제한) 1부. |
| P0-2 | 위 세션의 후속 의제로 "지시 1건을 특정하는 최소 키 집합" 확정. 우리 쪽은 `db/migrations/0052_stage2_match_shadow.sql`의 현재 컬럼 목록을 제시 자료로 사용. |
| P0-3 | 고객사 IT/DBA에 접속 계정의 `SELECT ANY`/객체 권한 목록 조회 결과(스크린샷 또는 권한 리스트) 요청. 병행하여 `SKILL_DIR` 하위 `remote-toolbox-sql` 스크립트 원문을 이관 자산 목록에 넣을 수 있는지 확인. |
| P0-4 | 배차 오퍼레이션 현장 인터뷰 1회(운영 반장 + PM) — "지금 이 화면을 보는 사람이 있는가, 보고 무엇을 하는가"를 관찰 기반으로 기록. |
| P0-5 | TOS 벤더 서면 회신 — "외부 권고 미수신 시 기본 배차 로직 동작 보증" 및 부분 적용 분리 단위. 계약 부속서에 편입 검토. |
| P0-6 | 당사 경영진 + PM 결정 회의 1회. 2026-06-08 고객 세션 자료와 `kc/dispatch/stage2-rollout.html` 두 기준을 나란히 놓고 계약 판정 기준 1개를 선택. |
| P0-7 | 당사 내부 정리 — `stage2_solver_shadow`(`gap_pct`)와 `fair_compare_shadow`(`savings_pct`)의 정의·측정창·표본수를 표로 나열한 뒤 대외 인용용 수치 1개와 그 정의문을 확정. 세 kc 페이지의 하드코딩 값 갱신 담당자 지정. |
| P0-8 | 고객사 장비 대장(야드 트랙터 등록 목록)과 GPS 단말 설치 대장을 문서로 수령. 문서 관측치(280/495/539/138)와 대조표 작성. |
| P0-9 | TOS 벤더에 지연 특성 문의 + 당사 측 보조 측정: 이미 수집 중인 `dispatch_pred_sample`의 `tos_upd_dt`와 GPS 관측 시각의 차이 분포를 며칠 집계(신규 개발 없이 조회만으로 가능). |
| P0-10 | 게이트웨이 운영 주체 식별 후 서면 문의 — 감싸는 TOS RPC, 인증 방식, 가용성 SLA, 버전·호환성 정책. 발주서의 '공통 게이트웨이'와 동일 대상인지 명시적으로 확인. |
| P0-11 | 운영 호스트에서 `systemctl --user cat wp-api.service wp-etw-bridge.service`와 `crontab -l` 결과를 확보해 `deploy/systemd/`에 반영하는 PR 1건으로 처리 가능한지 개발·운영 담당자와 협의(비밀값은 EnvironmentFile로 분리). |
| P0-12 | 당사 보안 담당 + PM 검토 회의 1회 — 현행 `/api/livemap/positions` 응답 예시(개인 식별자 포함 사실만, 값은 제외)를 근거로 마스킹·보존 요건 결정. 고객사 개인정보 정책 문서 수령 여부 확인. |

---

## 8. 비고

- 본 문서의 모든 항목은 **코드에 존재하는 것**과 **운영에서 활성화된 것**을 구분해 기술했다. [호스트] 표기 항목은 2026-07-22 단일 시점 관찰이므로 재확인이 필요하다.
- 일정·범위·비용에 관한 판단은 이 문서에서 확정하지 않았다. 해당 결정이 필요한 항목은 모두 **Service 2 담당 PM 확인 필요**로 표시했다.
- 비밀정보(계정·비밀번호·토큰·호스트명·URL 실값)는 본 문서에 기재하지 않았다. 평문 비밀번호가 특정 스크립트·crontab에 존재한다는 **사실**만 §4에 남겼고, 환경변수는 키 이름(`DATABASE_URL`, `SKILL_DIR`, `API_ADDR`, `ETW_GATEWAY_URL`, `TOMORROW_API_KEY`)까지만 표기했다.
