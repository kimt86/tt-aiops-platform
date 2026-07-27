# 06. 중앙 데이터 게이트웨이 조사팀 인계 요약

> **⚠ 이 문서는 2026-07-22 시점의 스냅샷입니다.** 이후 두 가지가 바뀌어 일부 서술이 실제와 다릅니다:
> ① 프로젝트 개명(`wp-tt-dashboard` → `tt-aiops-platform`)으로 **systemd 유닛이 전부 `wp-*` → `tt-*`**, 크레이트가 `tt-*`가 됐습니다.
> ② 시나리오 서브시스템(`scengen`)이 재설계돼 출력 구조·수집 경로가 달라졌습니다.
> 현재 상태는 `deploy/systemd/README.md`, 루트 `README.md`, `/kc/data/equipment-deployment.html`을 기준으로 보세요.


## 1. 목적·범위·조사 기준

이 문서는 Westports **Service 2 — TT Assignment**가 현재 실제로 소비·산출하는 데이터를 중앙 데이터 게이트웨이 조사팀에 인계하기 위한 요약이다. 다루는 것은 (a) Service 2가 필요로 하는 입력 Event/Snapshot, (b) 신선도·지연 요구, (c) 현재 TOS/Oracle 직접 연결 현황, (d) 게이트웨이 전환 시 위험과 미결 사항이다. Service 2 내부 매칭 로직·가중치, 일정·비용, Service 1·3·4는 범위 밖이다.

조사 기준: 저장소 `/home/tkadmin/projects/tt-aiops-platform`(단일 Git 저장소), 브랜치 `scengen-collector`, **HEAD `10cc8c0`**, 조사일 **2026-07-22**. 이하 근거는 `파일경로:줄범위` 형식이며 커밋은 재표기하지 않는다. 저장소 밖 관찰은 **[호스트]**, kc/docs 문서 주장은 **[문서]**로 구분한다. 조사 시점 워킹트리에 미커밋 변경 5건이 있어 인용 값이 이후 달라질 수 있다(상태: **확인**).

핵심 전제 하나를 먼저 밝힌다. Service 2는 현재 **상시 Shadow 운영 + 사내 대시보드 표시** 단계이며, 산출한 배차 권고를 TOS로 되돌리는 경로는 코드에 존재하지 않는다(상태: **확인**, `crates/api/src/livemap.rs:3968-3972`, `db/migrations/0052_stage2_match_shadow.sql:1-2`). 따라서 아래의 "출력" 항목은 전부 **미구현 요구사항**으로 읽어야 한다.

## 2. Service 2가 필요로 하는 공통 입력 (Event / Snapshot)

| 필요 데이터 | 현재 취득 경로 | 필요한 신선도 | 사용 목적 | 게이트웨이 전환 시 형태 |
|---|---|---|---|---|
| 미배차 작업 큐 | Oracle `TOSADM.JOB_ORDER_LIST` 폴링 90초 → `live_candidate` 전량 교체 | 90초(현재 주기 = 지연 하한) | 매칭 대상 수요 버킷 산정 | **Snapshot**(전량 상태) + 상태전이 Event 병행이 바람직 |
| 진행중 배차 | 동일 테이블, 동일 90초 → `live_workpool` | 90초 | 진행 작업 ETA·비교 지표 | **Snapshot** (배차/해제는 Event로도 필요) |
| QC 작업큐·진행률 | Oracle `TOSADM.JOB_QUEUE_SCHEDULE` 90초 → `live_workqueue` | 90초 | 작업 순번·잔여량 기반 마감 산정 | **Snapshot** |
| 선박 일정(마감 원천) | Oracle `TOSADM.VSB_VOYAGE` 90초 → `live_vessel_schedule` | 분 단위로 충분 | 마감(ETD/작업완료) 역산 | **Snapshot**(변경 시 Event 알림 유용) |
| 컨테이너별 ETW | **Azure `tos_etw_gateway` HTTP** `GET /v1/voyages/{vessel}/{voyage}/snapshot`, 90초 | 90초 | 작업 순서 정밀화 | **Snapshot**(현재도 스냅샷 API) |
| 완료 핸드오버 이벤트 | Oracle `TOSADM.JOB_ORDER_HISTORY` 워터마크 증분 60초 | 60초 | 트럭 자유 시각 정답지·학습 라벨 | **Event**(append-only 스트림) |
| 크레인 무브 이벤트 | Oracle `TOSADM.MCH_OPERATION` 증분 5분(QC/RTG 각각) | 5분 | 크레인 처리속도 학습·핸드오버 검증 | **Event** |
| 장비 위치 | **WebSocket GPS 피드**(TOS 아님, 로컬 SSH 터널) 상시 스트림 | 초 단위(STALE 120초) | 차량 상태 분류·이동시간 비용 | **Event 스트림**(스냅샷 폴링으로 대체 불가) |

근거: `crates/extractor/src/workpool.rs:111-345`, `crates/extractor/src/handover.rs:57-68`, `crates/extractor/src/qc_moves.rs:53-65`, `crates/api/src/livemap.rs:3186-3245`(상태: **확인**).

TOS 추출 SQL 16개에 좌표 컬럼은 **0건**이다 — 위치는 TOS가 주지 않는다(상태: **확인**, `crates/extractor/sql/assigned_tt.sql` 주석).

## 3. 저지연·신선도 요구

- 현재 실제 주기는 **60초(핸드오버) / 90초(작업풀·ETW) / 5분(크레인 무브)** 이며, 게이트웨이가 이보다 느려지면 그대로 Service 2의 지연이 된다. **현 주기가 곧 지연 하한**이다(상태: **확인**, [호스트] 타이머 관찰 + `deploy/systemd/*.timer`).
- 매칭 자체는 60초 주기로 도는데, GPS 웹소켓 미연결 틱은 통째로 건너뛴다(`crates/api/src/livemap.rs:4180-4182`). 반면 **Oracle 미러의 신선도는 검사하지 않는다** — 작업풀 갱신이 멈춰도 낡은 목록으로 계속 산출된다(상태: **확인**, `crates/api/src/workpool.rs:183-187`). 게이트웨이 전환 시 **입력별 신선도 메타데이터(as_of/유효기한)를 계약에 넣어야 한다.**
- 위치 피드는 폴링이 아니라 **상시 스트림**이며, 소비 측 임계는 `FRESH_UNDER_S=15` / `STALE_AFTER_S=120`(초과 시 응답에서 제외) / `LOST_AFTER_S=600`이다(상태: **확인**, `crates/api/src/livemap.rs`). 이 경로를 요청·응답형 게이트웨이로 바꾸면 기능이 성립하지 않는다.
- **미측정 구간(중요)**: 현장 이벤트가 TOS Oracle에 반영되기까지의 지연은 **전혀 측정되지 않았다**(상태: **미확인**). 우리는 폴링·표시 지연만 안다. 또한 GPS 프레임의 단말 시각(`dtime`)은 저장되지 않고 수신 시각만 쓰이므로 **소스↔수집 지연을 분리 측정할 수 없다**(상태: **확인**). 게이트웨이가 원천 발생시각을 보존해 주면 이 측정이 처음으로 가능해진다.

## 4. Oracle/TOS 직접 연결 현황

- 접근 방식은 **폴링**이다. CDC·Kafka·Debezium·JDBC 직결은 **없음**(전 저장소 검색 무히트, 상태: **확인**). 외부 CLI `remote-toolbox-sql`을 자식 프로세스로 실행하고 SQL을 임시파일로 넘긴다.
- **게이트웨이 구현이 2개**이며 코드를 공유하지 않는다: `crates/extractor/src/runner.rs:41-72`(타임아웃 90초 하드코딩, 프로세스 전역 락) / `crates/scengen/src/toolbox.rs:44-73`(타임아웃 설정값, 기본 45초). 상태: **확인**.
- 대상은 **TOSADM 스키마 13개 객체**(JOB_ORDER_LIST, JOB_ORDER_HISTORY, JOB_QUEUE_SCHEDULE, MCH_OPERATION, MCH_WORKTIME, MCH_WORKSTOP, CDY_MACHINE, VSS_STATISTICS, VSB_VOYAGE, CDV_VESSEL, CYY_CONTAINER, ETV_BAPLIE_CONT, ETV_MOVINS_STOWAGE), SQL 25개 문장은 **전부 SELECT/WITH**이고 TOS 대상 쓰기 코드는 **0건**이다(상태: **확인**).
- 다만 **읽기 전용은 구조적 보장이 아니다.** `run_sql`은 임의 SQL 문자열을 그대로 외부 스크립트에 넘기며 SELECT 강제나 DML 거부 가드가 없다. 실제 강제는 **저장소 밖 스크립트와 Oracle 계정 권한**에 달려 있고, 저장소만으로는 검증할 수 없다(상태: **미확인**, 감사 관점 P0). 프로세스 간 Oracle 직렬화도 같은 외부 스크립트에 위임되어 있다는 주석뿐이다.
- **원천 보존기간**: `JOB_ORDER_HISTORY` 약 15일, `MCH_OPERATION`/`VSS_STATISTICS` 35일 이상([문서] `README.md:82-84`). 따라서 **깊은 백필은 불가**하고 앞으로 쌓는 것만 가능하다. 게이트웨이가 자체 보존을 제공하는지는 확인 필요.

## 5. 별도로 존재하는 TOS 경로 — Azure ETW HTTP 게이트웨이 (P0)

Oracle 폴링과 **별개로**, 이미 HTTP REST 게이트웨이가 운영 경로에 들어와 있다. 90초 작업풀 틱마다 `curl`로 항차별 스냅샷을 받아 컨테이너별 ETW를 적재하며, `wp-etw-bridge` SSH 터널을 경유하고 접속 정보는 환경변수 키 `ETW_GATEWAY_URL`(값 `<redacted>`)로 주입된다. 실패 시 warn 로그만 남기고 조용히 스킵한다(상태: **확인**, `crates/extractor/src/workpool.rs:111-170`).

**발주서가 말하는 '공통 데이터 게이트웨이'가 이 `tos_etw_gateway`와 동일 대상인지 확인이 필요하다(P0).** 동일하다면 Service 2는 이미 부분적으로 게이트웨이 소비자이고, 다르다면 TOS 경로가 3개(Oracle 폴링·ETW HTTP·신규 게이트웨이)로 늘어난다. 운영 주체·감싸는 TOS RPC·인증·SLA·버전 정책 모두 **미확인**이다. 참고로 `wp-etw-bridge` 유닛 정의는 저장소에 없고 호스트에만 존재한다([호스트], 상태: **확인**).

## 6. 공통 게이트웨이 전환 시 위험

| 위험 | 이유 | 전환 시 유지해야 할 것 |
|---|---|---|
| 전량 교체 스냅샷 구조 | `live_*` 5개 테이블은 매 tick DELETE 후 전량 재삽입 → 과거 큐·마감 추이 이력이 없음 | 스냅샷 전량성(부분 갱신으로 바뀌면 소비 로직 재설계) 또는 이력 보존 계약 |
| 워터마크 문자열 전진 | 증분 수집이 문자열 사전순 워터마크를 `GREATEST`로만 전진 → 롤백 불가. 미래 날짜 이상치 1건이면 이후 정상 데이터가 영구 누락 | 단조·재생 가능한 오프셋(재구독·리플레이 가능한 커서) |
| 당일 등가조건 | 크레인 무브 증분이 `COMPDATE = 오늘` 등가조건 → **자정 넘겨 정지하면 전날 미수집분 영구 복구 불가** | 시각 경계에 걸리지 않는 범위 조건 + 재수집 창 |
| 캡 도달 무경보 | handover/qc/rtg 수집은 행수 캡(3000·5000)에 도달해도 알리지 않음(scengen만 이벤트 기록) | 잘림(truncation) 신호를 응답에 포함 |
| 낮은 지연 경로(위치 스트림) | 위치는 초 단위 스트림이고 STALE 120초로 소비 | 스트림 전달 방식 자체(요청·응답 전환 불가) |
| 캡 없는 쿼리 | 90초 작업풀 쿼리만 행수 캡이 없고 반환 행 수 실측도 없음 | 물량 급증 시 지연 특성(백프레셔·페이징 규약) |
| 신선도 게이트 결손 | 소비 측이 Oracle 미러 신선도를 검사하지 않음 | 페이로드에 `as_of`/유효기한을 필수 필드로 |

(상태: 각 행 **확인**. 근거: `crates/extractor/src/{handover,qc_moves,rtg_moves,workpool}.rs`, `crates/scengen/src/collect.rs`, `crates/api/src/livemap.rs:4173-4183`)

## 7. Service 2 출력 Event와 DigiPort 연계

현재 출력은 **로컬 Postgres 그림자 테이블 + 사내 GET API가 전부**다. 대시보드 API 라우트는 **31개 전부 GET**이고, DigiPort를 포함한 외부 시스템으로의 송출 코드·계약은 **0건**이다(webhook/Kafka/MQTT/S3/CSV export 없음). OpenAPI·AsyncAPI·JSON Schema 산출물도 **0건**이며(`utoipa` 의존성은 선언만 되고 사용 0), 계약은 Rust 구조체와 프론트 타입에만 존재한다(상태: **확인**).

향후 출력 계약을 정의할 때 미결인 항목:

- **대상 식별자**: 현재 `stage2_match_shadow`에는 **컨테이너번호·작업지시 ID(MSNSEQ)가 없고** 행이 (ts, ytno, qc, vessel, queuename, jobtype, src_block) 버킷 단위다 → 지금 형태로는 "어느 작업지시"인지 지목 불가(상태: **확인**, `db/migrations/0052_stage2_match_shadow.sql:5-22`).
- **멱등성**: 커맨드 ID·재전송 규약 개념이 없다. 멱등성은 Postgres upsert 수준뿐(상태: **확인**).
- **Ack·수용 피드백**: 운영자 채택/거부를 수집하는 코드·테이블이 **없다** → 수용률을 현재 데이터로 측정 불가(상태: **확인**).

## 8. Key·Timestamp·Trace 요구

공통 키 후보와 함정(상태: 전부 **확인**):

| 키 후보 | 용도 | 함정 |
|---|---|---|
| `contno`(컨테이너번호) | 작업·핸드오버·무브 연결 | 단독으로는 재사용 가능 — `(contno, point, seqno)` 등 복합키 필요 |
| `ytno` / `trk_id`(트럭) | 차량 식별, GPS ID와 대조 | TOS 표기와 GPS 장비 ID 표기 규약이 경로마다 다름 |
| `queuename` | 작업 버킷 | **선박·항차 간 재사용됨** → Oracle 조인 시 fan-out, 코드가 조인을 의도적으로 회피 |
| `vessel` + `voyage` | 항차 단위 조인 | 취소 항차 필터 의존, ETW 게이트웨이도 이 키로 조회 |
| `machno`(크레인) | 크레인 무브 | 크레인 종류 판정이 정규식(`^C`=QC, `RTG%`=YC)에 의존, KPI별 대상 집합 불일치 |

시각은 최소 **4종을 구분해야 한다**: ① 업무 발생시각(YT_DIS_DT=배차, ACTV_DT=활성화, JOB_HIST_DATE‖TIME=완료, MCH_OPER_ST_DT/COMPTIME=무브) ② TOS 반영시각(UPD_DT, VSS_STT_UP_DT) ③ 우리 수집시각(as_of) ④ 소비·표시 시각. 현재 `live_workpool.upd_ts`를 배차시각으로 쓰는 것은 **근사**이며 오차가 측정되지 않았다(상태: **미확인**). Oracle 측 시각 컬럼 상당수가 `VARCHAR`(`YYYYMMDD`+`HH24MISS`, MYT) 조합이라 문자열 범위 스캔에 의존한다 — 게이트웨이가 타임존 명시된 정규 타임스탬프로 정규화해 주면 방어 필터 다수가 불필요해진다.

**Trace**: 요청 단위 Trace ID·상관관계 ID가 **없고** HTTP 로깅 미들웨어도 붙어 있지 않다(`crates/api/src/main.rs:43-77`). 감사 흔적은 배치 단위 `etl_run_log`(run_id, 상태, rows_written, error_text)와 `data_freshness`뿐이며 `etl_run_log`는 보존정책이 없다(상태: **확인**). 게이트웨이 도입 시 **요청 상관관계 ID를 계약 필수 필드로 넣는 것**이 사실상 유일한 개선 지점이다.

## 9. P0 미확인 사항 (게이트웨이 관점)

1. Azure `tos_etw_gateway`가 발주서의 '공통 데이터 게이트웨이'와 **동일 대상인가**. 운영 주체·인증·SLA·버전 정책은? (담당: Service 2 담당 PM 확인 필요)
2. 외부 스크립트 `remote-toolbox-sql`의 접속 계정 권한이 **읽기 전용으로 강제되는가**, 그리고 이 스크립트가 이관 범위에 포함되는가. 저장소로는 검증 불가.
3. 게이트웨이가 위 8종 입력을 **Event/Snapshot 중 어느 형태로 제공하는가**, 그리고 현재 60초/90초/5분 주기보다 느려지지 않음이 보장되는가.
4. **위치 스트림**을 게이트웨이가 취급하는가, 아니면 GPS 피드는 범위 밖으로 남는가(현재 TOS는 좌표를 제공하지 않음).
5. **현장 이벤트 → TOS 반영 지연**의 실측값. 미측정 상태에서는 종단 신선도 SLA를 합의할 수 없다.
6. TOS가 배차 지시를 수신할 때 요구하는 **대상 식별자**(작업지시 ID/MSNSEQ/컨테이너번호)와 Ack·멱등성 규약. 현재 산출물은 버킷 단위라 지목 불가.
7. 게이트웨이 측 **보존기간·리플레이 가능 범위**(원천은 15일/35일). 정지 후 복구 시 어디까지 재수집 가능한가.

일정·범위·비용은 이 문서에서 확정하지 않는다 — **Service 2 담당 PM 확인 필요**.
