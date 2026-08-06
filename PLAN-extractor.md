# PLAN-extractor — TOS Oracle 부하 최소화 (추출기 최적화)

⚠ 이 파일은 `PLAN.md`(배차 에이전트의 계약)와 **별개**다. 그쪽을 읽지도 고치지도 마라.

## 목표

TOS Oracle(운영 DB) 접근을 왕복 522회/h → ~300회/h, 전송 행수 −80%로 줄인다.
**불가침**: qc-moves·rtg-moves·handover의 60초 주기(배차 실시간 요구), stowplan의 데이터 자체.
모든 변경은 킬스위치를 남기고, 임계경로 규칙(아래 RULES)을 지킨다.

## RULES (임계경로 — 위반 시 라이브 배차가 죽는다)

1. **마이그레이션 먼저**, 코드 배포는 그 다음. 전부 멱등(`IF NOT EXISTS` / `ON CONFLICT`).
   적용법: `source .env && psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f db/migrations/<파일>` (전용 러너 없음).
2. `qc_move_log`·`rtg_move_log`·`tos_handover_label`에 **CHECK 제약 금지** — 한 행이 배치를 굴리면
   워터마크가 멈춰 스트림이 영구 정지한다(수집기는 배치 전체를 한 트랜잭션+`?` 전파로 처리).
3. Oracle NUMBER 컬럼을 Rust `Option<String>`으로 받으면 배치 전체가 Err — NUMBER는 `Option<i64>`로
   받거나 SQL에서 `TO_CHAR()`. VARCHAR2는 String 계열.
4. 커밋은 반드시 `git commit -- <경로>` (인덱스가 공유 자원 — acb8677 사고).
5. 빌드: `cargo build --release -p tt-extractor` (바이너리 이름은 `extractor`),
   scengen은 `-p scengen`. **없는 패키지명을 주면 성공처럼 보이니** 배포 확인은
   `ls -l --time-style=+%T target/release/<bin>` mtime으로.
6. 유닛 배포: `install -m644 deploy/systemd/<unit> ~/.config/systemd/user/ && systemctl --user daemon-reload`
   (심링크 아님·복사본). 재시작 대상은 각 단계에 명시.
7. Oracle 직접 조회는 검증 용도만, 항상 범위 제한. 실행법:
   `flock /var/tmp/tt-sql/oracle-toolbox.lock timeout 60 /home/aiadmin/.codex/skills/yard-db-ops/scripts/remote-toolbox-sql oracle-prod --sql "<SQL>"`
8. Postgres(`wp_tt`@127.0.0.1:5433)도 **운영 DB**다. 무거운 탐색은
   `BEGIN; SET LOCAL statement_timeout='5s'; EXPLAIN (ANALYZE) ...; ROLLBACK;`.

## GLOSSARY

| 용어 | 실체 |
|---|---|
| 본 추출기 | `crates/extractor` (바이너리 `extractor`) — 배차·대시보드용. Oracle 접근은 `crates/extractor/src/runner.rs`의 `Toolbox` |
| scengen | `crates/scengen` (바이너리 `scengen`) — 시나리오 수집. 자체 `Toolbox`(`crates/scengen/src/toolbox.rs`) + flock `/var/tmp/tt-sql/oracle-toolbox.lock` |
| MCH_OPERATION | TOS의 장비 무브 원장. 우리 로컬 사본 = `public.qc_move_log`(QC, machno `^[CMZ]`) + `public.rtg_move_log`(RTG/ES) |
| JOB_ORDER_HISTORY | TOS의 완료 작업 원장. 로컬 사본 = `public.tos_handover_label`(DS/LD·JOBSTATUS='C'만, `crates/extractor/src/handover.rs`가 60초 수집) |
| VSP_SHIP | TOS 적부계획(상자별 작업 순번). 로컬 = `public.live_stow_plan`(`crates/extractor/src/stowplan.rs`, 5분 전체교체). **UPD_DT에 인덱스 있음**(`IDX_VSP_SHIP_UPD_DT`, 실측) |
| t1/t2 틱 | `tt-shift-t1.timer`(3분)/`tt-shift-t2.timer`(15분) → `extractor tick --shift --tier t1|t2` → `crates/extractor/src/shift.rs::tick_shift` |
| KPI 로컬화 | Oracle 재스캔 KPI를 로컬 사본(qc_move_log 등)으로 계산하는 것. 이 런에서는 **병산(parity)까지만** — 양쪽 다 계산해 `kpi_parity_log`에 기록, 절체는 나중 |
| 병산(parity) | 같은 KPI를 Oracle 경로와 로컬 경로로 동시에 계산해 값을 나란히 기록하는 것 |
| 델타 스트림 | 전체 스냅샷 대신 `UPD_DT >= 워터마크`로 변한 행만 받아 로컬 거울(mirror)에 병합 + 주기적 전체 스냅샷으로 자가치유 |
| 워터마크 | 스트림별 진행 커서. 본 추출기는 `public.etl_watermark`(kpi_key, watermark TEXT), scengen은 `scenario.watermark` |
| shift 창 | MYT(UTC+8) 기준 N 00-08 / D 08-16 / E 16-24. 코드 권위 = `crates/core/src/shift.rs` |
| DAY_STR | Oracle KPI SQL 템플릿의 `{{DAY_STR}}` = MYT 날짜 `YYYYMMDD` (`crates/extractor/src/shift.rs::fetch_oracle` 참조) |
| kpi_shift | KPI 착지 표(`public.kpi_shift` + `kpi_shift_history`), `shift.rs::upsert_shift` |
| 유닛 env | 추출기·scengen 유닛은 `EnvironmentFile=%h/projects/tt-aiops-platform/.env` — 킬스위치 env는 이 파일에 추가하고 유닛 재시작 |
| mig 번호 | `db/migrations/` 다음 번호는 **0133**부터 (0132까지 사용됨) |
| 부하 장부 | 이 계획 1-1에서 만드는 `scripts/oracle_load_ledger.sh`의 출력 |

## 현재 부하 실측 (2026-08-06 기준선)

- 왕복 ≈522회/h: 무브 3종 180 · workpool 틱 160(4쿼리×40) · t1 80(MCH 하루재스캔 60 포함) ·
  stowplan 12 · t2 8 · scengen ~42 · nightly 일1회
- 최대 전송원 = stowplan **6,200행×5분**(=74k행/h), 다음 workpool 스냅샷 1,270행×40
- 매분 같은 초에 qc/rtg/handover 3세션 동시 발화(저널 실측 02:01:58.1/58.6/58.1)
- 검증된 사실: JOB_ORDER_LIST·VSP_SHIP 모두 **완료가 UPD_DT를 갱신**(각각 최근 1h 4,263행 중
  2,631 완료 / 오늘 63,750행 중 17,039 완료) → 델타로 완료까지 보임

---

## CHUNK 1 — 부하 장부 + 타이머 분산 (무위험·즉시)

### 1-1. `scripts/oracle_load_ledger.sh` 신설
journalctl(사용자 유닛)과 `scenario.gen_run`으로 최근 24h를 집계해 표로 출력하고
`data/oracle_load/ledger-$(date +%Y%m%d-%H%M).txt`에 저장:
- 유닛별: 실행 수, 벽시계 합(Starting→Finished 차), 평균/최대
- scengen: `SELECT kind, count(*), sum((load_stats->>'query_ms')::int) FROM scenario.gen_run WHERE started_at > now()-interval '24 hours' GROUP BY 1`
- 대상 유닛: tt-qc-moves tt-rtg-moves tt-handover tt-workpool tt-shift-t1 tt-shift-t2 tt-stowplan tt-nightly tt-scenario-{collect,yard,gate,contspec,enrich}
집계 방식은 이 세션 실측과 동일: `journalctl --user -u <u> --since '24 hours ago' -o short-unix | awk '/Starting/{s=$1} /Finished/{if(s){print $1-s; s=""}}'`

**검증**: `bash scripts/oracle_load_ledger.sh` 실행 → 표 출력에 tt-workpool 행이 있고 runs>500,
저장 파일 존재. 출력 숫자를 보고에 포함할 것.

### 1-2. 60초 유닛 3개 발화 초 분산
`deploy/systemd/tt-qc-moves.timer`·`tt-rtg-moves.timer`·`tt-handover.timer`를
`OnUnitActiveSec=60s` → `OnCalendar` 방식으로 교체:
```
[Timer]
OnBootSec=90s
OnCalendar=*-*-* *:*:05   # rtg는 :25, handover는 :45
AccuracySec=1s
```
(OnBootSec은 유지. OnCalendar+OnBootSec 병존 허용.)
설치(RULES 6) 후 `systemctl --user restart tt-qc-moves.timer tt-rtg-moves.timer tt-handover.timer`.

**검증**: 5분 뒤 `journalctl --user -u tt-qc-moves -u tt-rtg-moves -u tt-handover --since '4 min ago' -o short-precise | grep Starting`
→ 시작 초가 각각 :05/:25/:45 (±2초). 세 유닛 Result=success 유지.

### 1-3. scengen 5분 타이머 4개 발화 분산
`tt-scenario-collect.timer`(600s)·`tt-scenario-yard.timer`(300s)·`tt-scenario-gate.timer`(300s)·
`tt-scenario-contspec.timer`(300s)·`tt-scenario-plan.timer`(300s)를 OnCalendar로:
collect `*:0/10:40`, yard `*:0/5:10`, gate `*:0/5:30`, contspec `*:0/5:50`, plan `*:0/5:20`, AccuracySec=5s.
(assemble·enrich·web은 그대로.)

**검증**: 10분 뒤 list-timers에서 NEXT가 서로 다른 초. 각 유닛 Result=success.

### 1-4. 기준선 저장
1-1 스크립트를 한 번 실행해 저장된 파일 경로와 요약 숫자(왕복/h 근사)를 보고.

---

## CHUNK 2 — KPI 병산 (Oracle은 그대로, 로컬 값을 나란히 기록)

절체 아님. Oracle 경로가 계속 권위값을 쓴다. 이 청크는 로컬 계산을 **추가**하고 둘을 기록만 한다.

### 2-1. mig `0134_kpi_parity.sql` (0133은 배차 트랙이 선점)
```sql
CREATE TABLE IF NOT EXISTS kpi_parity_log (
  kpi_key       TEXT NOT NULL,
  business_date DATE NOT NULL,
  shift         TEXT NOT NULL,           -- 'N'|'D'|'E' (shift.rs 라벨)
  src           TEXT NOT NULL,           -- 'oracle' | 'local'
  value         DOUBLE PRECISION,
  sample_n      BIGINT,
  computed_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS kpi_parity_lookup ON kpi_parity_log (kpi_key, business_date, shift, src, computed_at);
```
적용(RULES 1) 후 `\d kpi_parity_log` 확인.

### 2-2. 로컬 KPI SQL 5본 신설 — `crates/extractor/sql/local/`
각각 Oracle 원본과 **같은 의미**를 Postgres 사본으로 재현. 입력은 `$1,$2 = 창 시작/끝(timestamptz UTC)`.
MYT 창은 호출부(shift.rs)가 이미 계산한다(`shift::window` → `terminal_to_utc`).

| 파일 | 원본(의미 기준) | 로컬 소스 | 핵심 |
|---|---|---|---|
| `l_mph.sql` | `sql/c07_k_mph_realtime.sql` | `qc_move_log` | machno `~'^C[0-9]+$'`, jobtype IN('LD','DS'), comp_ts ∈ 창. 크레인별 LD/DS 카운트+first/last → 호출부가 동일 집계 |
| `l_qc_q.sql` | `sql/f2_k_qc_q.sql` | `qc_move_log` | ★정정(1차 실행에서 드러남): "연속 comp 갭" 단순화 금지 — 원본과 −20% 벌어진다. 원본의 **구간 병합**을 그대로 재현하라: `st_ts`(=원본 ST_DT의 사본)→`comp_ts` 구간을 크레인별로 겹침 병합(running max 섬-갭 기법)한 뒤 병합 블록 **사이의** 갭에 원본 버킷 적용 |
| `l_tt_cycle.sql` | `sql/c10_k_tt_cycle.sql` | `qc_move_log` | trk_id별 연속 comp_ts 갭, 120~1200s만 |
| `l_crane_q.sql` | `sql/c08_k_crane_q.sql` | `tos_handover_label` | dis_ts(=YT_DIS_DT)·actv_ts(=ACTV_DT) NOT NULL, 갭=actv−dis 상당 — **원본 SQL을 열어 산식을 그대로 옮길 것**(창은 comp_ts 기준) |
| `l_cycle.sql` | `sql/e3b_k_cycle_refined_v2.sql` | `tt_move_log` | dispatch_ts→free_ts 사이클. **의미가 원본과 다름을 안다**(원본은 HISTORY 전이) — 병산 목적이 바로 그 차이 측정 |

주의: 원본 SQL의 수치 산식(버킷 경계·캡·가중)을 임의 해석하지 말고 파일을 열어 옮긴다.
k_crane_q_hour(e5)는 c08과 같은 소스라 이번엔 **c08만**(대표) 병산한다. k_empty는 CHUNK 4 뒤에만 가능(컬럼 없음) — 이번 런 제외.

★병산 정렬 3원칙(1차 실행 후 추가 — 게이트에 쓰려면 필수):
1. **창 기준을 Oracle쪽과 동일하게**: 병산의 로컬 창은 "shift.rs가 그 KPI의 Oracle 값에
   실제로 쓴 창"과 같아야 한다. K_RTG_Q는 Oracle쪽이 하루 누적(DAY_STR 하루 스캔·표본
   20,585)이므로 로컬도 같은 하루 기준으로 — 각 src_* 함수가 Oracle 결과에서 값을 만드는
   방식을 읽고 맞춰라.
2. **sample_n 의미를 미러**: oracle 행의 sample_n 파생(크레인 수든 갭 수든)을 로컬 행도
   동일 의미로 기록. 다르면 게이트 질의가 오독한다.
3. **oracle 값이 없으면 oracle 행을 넣지 않는다**(K_CYCLE의 당일 raw_k_cycle 부재처럼) —
   NULL 행을 틱마다 쌓지 말 것. local 행은 넣는다.

★게이트 판독 노트(2026-08-06 병산 결과 확정):
- K_MPH/K_QC_Q/K_TT_CYCLE = 0.0~0.1% 일치. 게이트는 이 셋의 지속 일치로 판정.
- **K_RTG_Q +6.0%는 영구적·설명된 차이** — Oracle 원본(c08)이 JOB_ORDER_HISTORY의
  전이 행(컨테이너당 ~10행, dis/actv가 전 행에 복제)을 그대로 세어 상자를 ~10중 가중.
  로컬(tos_handover_label, 작업당 1행)이 올바른 가중이다. 로컬을 중복에 맞추지 마라.
  절체 시 표시값이 ~+6% 이동함을 사용자에게 고지할 것.
- K_CYCLE은 nightly(raw_k_cycle)가 하루 뒤 착지하므로 business_date 기준으로 D+1에만 짝이 생긴다.

### 2-3. `crates/extractor/src/shift.rs` 병산 배선
- env `KPI_PARITY=off|on` (기본 on, `.env`에 `KPI_PARITY=on` 추가).
- t1 경로: `src_mph_vessels`·`src_qcq`·`src_cycle` 각각의 Oracle 집계 완료 직후,
  같은 창으로 로컬 SQL 실행 → `kpi_parity_log`에 (src='oracle', 원본값)·(src='local', 로컬값) 2행 INSERT.
  Oracle 값은 이미 계산된 것을 재사용(재조회 금지). KPI 키: `K_MPH`/`K_QC_Q`/`K_TT_CYCLE`.
- t2 경로(`want_heavy`): `src_craneq` 뒤 `l_crane_q` 병산(키 `K_RTG_Q`), `l_cycle`은 `src_cycle`이 아닌
  **t2에서 K_CYCLE 병산**(키 `K_CYCLE`).
- 병산 실패는 `tracing::warn!` 후 계속(step! 매크로와 같은 태도) — KPI 본선을 절대 막지 않는다.

### 2-4. 빌드·배포·실증
빌드(RULES 5) → `systemctl --user start tt-shift-t1.service` 수동 1회 → t2도 1회.

**검증**:
```
psql: SELECT kpi_key, src, round(value::numeric,2), sample_n FROM kpi_parity_log
      WHERE computed_at > now()-interval '10 min' ORDER BY kpi_key, src;
```
→ K_MPH/K_QC_Q/K_TT_CYCLE/K_RTG_Q/K_CYCLE 각각 oracle·local 2행(총 10행).
값 일치를 단정하지 말 것 — **두 값과 차이%를 그대로 보고**(차이가 크면 그것이 발견이다).
추가: `journalctl --user -u tt-shift-t1 -n 20`에 error 없음.

---

## CHUNK 3 — 적부계획(stowplan) 델타 스트림화

최대 전송원(6,200행×5분)을 인덱스 있는 UPD_DT 델타로 바꾸고 주기를 2분으로 올린다(신선도↑).

### 3-1. mig — 파일명은 `db/migrations/` 의 **다음 빈 번호**로 (0133·0134는 사용됨) `NNNN_stow_plan_delta.sql`
- `live_stow_plan`의 현재 정의를 `db/migrations/0128_live_stow_plan.sql`·`0129_...`에서 확인.
- UNIQUE 인덱스가 없으면: `CREATE UNIQUE INDEX IF NOT EXISTS live_stow_plan_key ON live_stow_plan (vessel, voyage, contno, disload);`
  ⚠ 만들기 전에 현재 데이터 중복 확인: 중복이 있으면 UNIQUE 대신 이 청크를 중단하고 보고(PLAN ERROR).
- `etl_watermark`에 키 `'stowplan_delta'` 행은 코드가 시딩(마이그레이션에서 넣지 않음).

### 3-2. `crates/extractor/src/stowplan.rs` 델타 모드
- env `STOWPLAN_MODE=delta|snapshot` (기본 delta, `.env`에 추가 — 킬스위치).
- delta 틱: 기존 `__VOYAGES__` IN-list는 유지하고 WHERE에 `AND v.UPD_DT >= TO_DATE('{wm}','YYYYMMDDHH24MISS')` 추가,
  `VSP_SHP_COMPDATE IS NULL` 필터는 **델타에서 제거**(완료행을 봐야 거울에서 지운다).
  SELECT에 `VSP_SHP_COMPDATE AS compdate`, `TO_CHAR(v.UPD_DT,'YYYYMMDDHH24MISS') AS upd` 추가.
- 병합: compdate IS NULL → UPSERT(UNIQUE 키 기준, planseq·queuename 갱신), NOT NULL → DELETE.
- 워터마크: `max(upd)` − 안전랙 120초, `GREATEST` 역진 방지 — `handover.rs`의 워터마크 취급을 본떠라.
- 첫 델타 틱(워터마크 없음)은 스냅샷 경로로 한 번 돌고 워터마크 시딩.
- snapshot 모드 = 기존 전체교체 코드 그대로(삭제 금지).

### 3-3. 화해(reconcile) — `tt-stowplan-recon.{service,timer}` 신설 (1시간)
`extractor stowplan --target oracle-prod --reconcile` 서브플래그: 기존 전체 스냅샷을 받아
거울과 diff → 불일치 행수 로그(`recon drift=N fixed=N`) 후 스냅샷으로 교체.
유닛 파일은 `deploy/systemd/tt-stowplan.service`를 본떠 작성(EnvironmentFile 동일).

### 3-4. 주기 2분 + 배포
`deploy/systemd/tt-stowplan.timer`: `OnUnitActiveSec=5min` → `OnCalendar=*:0/2:15`(분산 초 :15), AccuracySec=5s.
빌드·mig·유닛 설치·재시작.

**검증**:
1. `journalctl --user -u tt-stowplan --since '10 min ago' -o cat` → 델타 틱 rows가 **300 미만**(직전까지 6,000대).
2. recon 수동 1회: `systemctl --user start tt-stowplan-recon` → `drift=N` 로그 출력, N 값을 보고.
3. 행수 대조(허용 오차 = 틱 사이 변화):
   `psql: SELECT count(*) FROM live_stow_plan;` vs Oracle
   `SELECT COUNT(*) FROM TOSADM.VSP_SHIP WHERE VSP_SHP_DISLOAD IN ('D','L') AND VSP_SHP_PLANST='P' AND VSP_SHP_COMPDATE IS NULL AND (VSP_SHP_VESSEL,VSP_SHP_VOYAGE) IN (<현재 항차들>)`
   — 두 숫자를 보고(±2% 이내 기대).
4. **배차 소비자 무손상**: `dispatch_pred_sample`에 새 행이 계속 쌓이는지(`max(logged_at)` 10분 내).

---

## CHUNK 4 — 핸드오버 확장 + scengen collect/yard 로컬화 (Oracle 중복 제거)

### 4-1. mig — 0136은 적용됐으나 타입이 틀렸다. **후속 mig(다음 빈 번호)로 교정**
★1차 실행에서 확정된 Oracle 실제 타입(ALL_TAB_COLUMNS 실측):
`LNDN_TRV_RNG = NUMBER(8,1)`(소수 1자리 → JSON float), `CRNT_PSN_IDX_NO1~4 = VARCHAR2`(→ JSON string).
0136이 만든 BIGINT 컬럼들은 한 번도 안 채워졌으므로 **ALTER TYPE(테이블 재작성·잠금) 금지** —
DROP+ADD(메타데이터만·즉시)로 교정한다:
```sql
ALTER TABLE tos_handover_label DROP COLUMN IF EXISTS trv_rng;
ALTER TABLE tos_handover_label ADD  COLUMN IF NOT EXISTS trv_rng DOUBLE PRECISION; -- NUMBER(8,1)
ALTER TABLE rtg_move_log DROP COLUMN IF EXISTS pos1;
ALTER TABLE rtg_move_log DROP COLUMN IF EXISTS pos2;
ALTER TABLE rtg_move_log DROP COLUMN IF EXISTS pos3;
ALTER TABLE rtg_move_log DROP COLUMN IF EXISTS pos4;
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos1 TEXT;  -- VARCHAR2 원형 보존(디코드는 scengen 몫)
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos2 TEXT;
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos3 TEXT;
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos4 TEXT;
```
(vessel·voyage TEXT는 0136 그대로 유효.) CHECK 금지(RULES 2). 적용 전 pg_stat_activity 확인(RULES 8).

### 4-2. 본 추출기 SELECT 확장 (부하 0 전례: mig0109 3컬럼)
- `crates/extractor/src/handover.rs`: SELECT에 `JOB_HIST_VESSEL AS vessel, JOB_HIST_VOYAGE AS voyage, LNDN_TRV_RNG AS trv_rng`
  — trv_rng는 **`Option<f64>`** (NUMBER(8,1)이 JSON float로 옴 — 544.3 실측. serde f64는 정수 JSON도 받는다).
- `crates/extractor/src/rtg_moves.rs`: SELECT에 `CRNT_PSN_IDX_NO1..4 AS pos1..pos4` — **`Option<String>`**
  (VARCHAR2가 JSON string으로 옴 — "446" 실측). trim 후 빈 문자열은 NULL로.
★교훈(이 계획의 실책): 임계 SELECT를 넓히기 전 ALL_TAB_COLUMNS로 타입을 실측하라.
  1차 시도는 타입 단정 → 두 스트림이 매 틱 파싱 실패 → 2분 정지(워터마크 자가복구·유실 0) 후 롤백됐다.
빌드·배포·재시작 후 저널의 extract 소요가 직전과 ±20% 이내인지 확인.

### 4-3. scengen collect 로컬화 — `crates/scengen/src/collect.rs`
Oracle(JOB_ORDER_HISTORY) 대신 **로컬 `tos_handover_label`**을 읽어 `scenario.move_hist`에 같은 모양으로 착지:
- 소스 쿼리: `SELECT contno, jobtype, vessel, voyage, topos, armgc AS machno, to_char(comp_ts AT TIME ZONE 'Asia/Kuala_Lumpur','YYYYMMDDHH24MISS') AS evt FROM tos_handover_label WHERE comp_ts > $워터마크 ...`
  — 기존 워터마크(14자 MYT) 형식·`scenario.watermark` 그대로.
- Toolbox 호출 제거, `load_stats`에 `"oracle": false`.
- ⚠ 4-2 배포 이전 행은 vessel NULL — 정상(전방부터 채워짐). NULL 허용 경로 확인.

### 4-4. scengen yard 로컬화 — `crates/scengen/src/yard.rs`
`run()`의 소스를 Oracle MCH_OPERATION → **로컬 `rtg_move_log`**(pos1 IS NOT NULL)로 교체.
디코드·MAX_TIER 가드·REJECTED 로깅·워터마크(seqno 14자)·`yard_cell` 파생 전부 유지.
Toolbox 제거. `tt-scenario-yard.timer`는 유지(이제 Oracle 0).

### 4-5. 검증 (전 단계 통합)
1. 10분 뒤 `psql: SELECT count(*) FROM tos_handover_label WHERE vessel IS NOT NULL AND captured_at > now()-interval '10 min';` → >0
2. `psql: SELECT count(*) FROM rtg_move_log WHERE pos1 IS NOT NULL AND captured_at > now()-interval '10 min';` → >0
3. scengen: `SELECT kind, load_stats->>'queries' FROM scenario.gen_run WHERE kind IN ('collect','yard_moves') ORDER BY run_id DESC LIMIT 4;` → queries=0
4. 스트림 전진: `SELECT source, cursor_evt FROM scenario.watermark WHERE source IN ('move_hist','yard_move');` 10분 간격 2회 → 증가
5. 겹침 대조(1시간 뒤): 최근 1h `scenario.yard_move` 행수 vs `rtg_move_log`(RTG/ES, pos1 NOT NULL) 행수 — 두 숫자 보고, ±5% 기대
6. 시나리오 산출 무손상: `curl -s 'http://127.0.0.1:8899/api/scenario/status'`의 usable_range가 존재하고 watermark age < 15min

---

## CHUNK 5 — workpool 델타 (승인됨 2026-08-06 — 배차 에이전트 작업 종료 확인)

⚠ 이 청크의 파일은 `crates/extractor/src/workpool.rs`(Oracle 풀러)다.
`crates/api/src/workpool.rs`(배차 로직·현재 워킹트리에 남의 미커밋 변경 있음)는 **절대 접근 금지** —
이름만 같은 다른 파일이다. crates/api 전체 수정 금지(소비자는 읽기 조사만).

### 5-1. 소비자 계약 조사 (코드 수정 전, 보고 포함)
`crates/api/`에서 `live_workpool`·`live_workqueue`를 읽는 곳을 grep으로 찾아
**per-row `as_of`(또는 유사 신선도 컬럼)로 행을 필터링하는지** 확인한다.
- 필터링 안 하면: 델타 병합에서 갱신 행만 as_of 갱신 (기본).
- 필터링 하면: 델타 틱마다 전 행 as_of를 로컬 UPDATE로 일괄 갱신(로컬이라 무해) — 소비자 코드는 불변.
조사 결과(파일:행)를 보고에 담아라.

### 5-2. mig (다음 빈 번호) — 거울 키
- `live_workpool`·`live_workqueue`의 현 스키마를 해당 마이그레이션 파일에서 확인.
- UPSERT 무결성 키 후보: workpool은 `(contno, jobtype)` — ⚠`seqno`는 완료 시 TOS가 덮어쓰는
  가변 컬럼이라 키 금지(노트 실측). workqueue는 `(qc, queuename)`.
- **키 유일성을 현재 데이터로 검사**(로컬 표에서 GROUP BY 중복 카운트). 중복이 나오면 멈추고
  키 후보와 중복 예시를 보고(PLAN ERROR 처리). 유일하면 UNIQUE INDEX IF NOT EXISTS 생성.

### 5-3. `crates/extractor/src/workpool.rs` 델타 모드
- env `WORKPOOL_MODE=delta|snapshot` (기본 delta, `.env` 추가). snapshot = 기존 경로 무손 보존.
- 델타 틱(본체+카운터, 한 틱에 Oracle 2회):
  * pool: `SQL_WORKPOOL`의 WHERE에서 `COMPDATE IS NULL` 제거하고 `UPD_DT >= TO_DATE('{wm}',...)` 추가,
    SELECT에 `JOB_ODR_COMPDATE`·`TO_CHAR(UPD_DT,...)` 포함. COMPDATE 차면 DELETE, 아니면 UPSERT.
    jobtype 필터(DS,LD)는 유지 — assigned 는 5-4 참조.
  * workqueue: 같은 패턴, `DELT_FLG='Y'` 또는 완료조건이면 DELETE. (원본 WHERE의 UPD_DT 절 참조.)
  * 워터마크 2개(`workpool_delta`,`workqueue_delta`) — stowplan_delta 와 같은 취급(−120s 랙·GREATEST).
  * 첫 틱은 스냅샷 경로 1회 + 시딩 (stowplan.rs 3-2 패턴 재사용).
- assigned(`SQL_ASSIGNED`)는 **그대로 유지**(작고 싸다·MI/MO 트럭 포함 의미가 풀과 달라 병합 시
  소비자 위험 — REJECTED 참조).
- vessel_schedule 을 tick_workpool 에서 **분리**: 새 서브커맨드/플래그 + `tt-vessel-schedule.{service,timer}`
  (5분·OnCalendar 초 분산·기존 유닛 본떠 작성). tick_workpool 에서는 호출 제거.
- ETW(src_etw)는 로컬이므로 그대로.

### 5-4. 주기·화해·배포
- `tt-workpool.timer`: 90초 → `OnCalendar=*-*-* *:*:15`(60초·분산 초 :15), AccuracySec=1s.
- `tt-workpool-recon.{service,timer}` 신설(1시간): 스냅샷 질의로 거울 diff → `drift=N fixed=N` 로그 후 교체
  (stowplan recon 패턴). pool·workqueue 둘 다.
- mig → 빌드 → 유닛 설치 → 재시작 순서.

### 5-5. 검증 (전부 숫자로 보고)
1. 저널: 델타 틱의 pool/workqueue rows — 평균 150행 미만(직전 스냅샷 1,270+1,111 대비).
2. recon 수동 1회: drift=N 값.
3. 행수 대조: 로컬 live_workpool count vs Oracle `SELECT COUNT(*) ... COMPDATE IS NULL AND JOBTYPE IN ('DS','LD') AND JOBSTATUS IN ('A','Q')` (프로브 1회) — 두 숫자, ±3% 기대.
4. **배차 소비자 무손상**: ①`dispatch_pred_sample` max(logged_at) 5분 내 ②api 저널(`journalctl --user -u tt-api`) 최근 10분 error 0 ③live_workpool 행수가 정상 범위(900~1,600).
5. vessel_schedule 분리 후: live_vessel_schedule 갱신 시각이 5분 주기로 전진.
6. 60초 주기 확인: tt-workpool Starting 이 매분 :15.

---

## REJECTED APPROACHES (헤매지 마라)

- **qc/rtg/handover 주기 완화** — 배차 실시간 요구의 본체. 금지.
- **KPI 즉시 절체(Oracle 경로 제거)** — 병산 1~2주 게이트 전 금지. 이 런은 병산까지.
- **nightly 폐기·k_util_tt 삭제** — 병산 게이트 뒤의 일. 이 런 범위 밖.
- **VSB_VOYAGE(선박스케줄) 델타화** — 228행뿐, 이득 없음. CHUNK 5에서 5분 스냅샷으로만.
- **assigned를 pool 델타에 병합** — assigned 는 jobtype 무필터(MI/MO 트럭 포함)·B 상태 포함으로
  풀(DS/LD·A/Q)과 모집단이 다르다. 병합하려면 풀 거울에 MI/MO 행을 넣어야 하는데 live_workpool
  소비자(crates/api·수정 금지)가 DS/LD 전제일 위험 → 작고 싼(2s) 별도 쿼리 유지가 옳다.
- **contspec 미스장부** — 이미 구현됨(mig0122·`scenario.container_spec_miss`). 손대지 마라.
- **툴박스/브리지(Azure mcp-toolbox) 개선** — 공유 인프라. 접근 금지(ETW 게이트웨이와 동급).
- **Oracle측 인덱스/힌트 변경** — 읽기 전용 계정.
- **`DISTINCT ON`+`ORDER BY` 최신순 착각** — DISTINCT ON은 키 선두 정렬을 강제한다(1dcbcc2 교훈).
- **빈 집계를 0행으로 가정** — GROUP BY 없는 집계는 전-NULL 1행을 돌려준다(8ed4ea9 교훈). COUNT류는 `Option<i64>`.

## CHUNK 6 — KPI 절체 + nightly 로컬화 (★사용자 지시 2026-08-06: 병산 1~2주 게이트 면제 — "터무니없게 차이만 안 나면 바로 진행")

절체 근거(당일 병산 실측): K_MPH −0.1% · K_QC_Q 0.0% · K_TT_CYCLE −0.1% (즉시 절체 합격)
/ K_RTG_Q +6.0% = 원본 c08의 ~10중 계수 결함, 로컬이 옳음(게이트 판독 노트) — 절체하되 표시값 +6% 이동 고지
/ K_CYCLE = **이 런에서 손대지 않음**(표시값은 c10 정의 유지 — 재정의는 별건 결정)
/ ★K_EMPTY 만 예외: 재료 trv_rng 가 2026-08-06 15시(KST)부터만 착지 — 하루치가 차는 **08-08 이후** 전환,
  그때까지 t2·nightly 에서 Oracle 유지(e4 쿼리 그대로).

### 6-1. t1 절체 — `crates/extractor/src/shift.rs`
- env `KPI_T1_SRC=local|oracle` (기본 local, `.env` 추가 — 킬스위치. oracle 이면 종전 경로 그대로).
- local 모드: `src_mph_vessels`·`src_qcq`·`src_cycle` 이 Oracle fetch 를 **하지 않고** CHUNK 2 의 로컬 SQL
  결과로 `upsert_shift` 를 채운다. 주의: `src_mph_vessels` 는 무브수 외에 선박 패널도 만들었다 —
  선박 패널 재료가 Oracle 행에서만 나오는지 확인하고, 나온다면 로컬 대체가 가능한지(qc_move_log 의
  vessel·voyage 컬럼) 확인해 같이 로컬화하라. 불가능한 재료가 있으면 멈추고 보고.
- `voyage_plan`(VSS_STATISTICS)은 t1 에서 **t2 로 이동**(3분→15분).
- 병산 로깅(KPI_PARITY)은 코드 유지 — local 절체 후 oracle 행이 자연 소멸할 뿐.

### 6-2. t2 절체 — 같은 파일
- `src_craneq`: Oracle fetch 제거, `l_crane_q` 계열 로컬 산출로 K_RTG_Q upsert.
  k_crane_q_hour 도 같은 로컬 소스(tos_handover_label 시간 버킷)로.
- `src_empty`(K_EMPTY)는 **Oracle 그대로**(위 예외).
- ★★삭제된 지시(1차 실행이 잡은 계획 오류): ~~`K_CYCLE`을 `l_cycle`로 upsert~~ — **하지 마라.**
  `kpi_shift.K_CYCLE`은 t1 `src_cycle`이 c10 정의(MCH_OPERATION 트럭별 QC무브 간격)로 쓰는
  값이고, 그게 의도된 설계다(shift.rs 주석 "Displayed K_CYCLE is the REAL TT cycle").
  t2에서 다른 정의(tt_move_log)로 같은 키를 쓰면 3분/15분 두 writer가 서로 덮는 경합이 된다.
  **절체(원천 교체)와 재정의(지표 변경)는 다른 일이다.** 이 런은 절체만 한다.
  `l_cycle`은 **병산 전용으로 유지**(kpi_parity_log의 K_CYCLE 키 — 상대는 nightly raw_k_cycle).
  표시 지표 재정의는 별건으로 사용자 결정 사항.

### 6-3. nightly 로컬화 — `crates/extractor/src/main.rs::run_kpi` + `crates/extractor/src/kpis/*.rs`
- `k_util_tt` — **제거**: 프론트 미사용 확인됨(App.tsx:402 "TOS session value not shown").
  제거 전 raw 산출표 소비처를 저장소 전체 grep 으로 최종 확인(있으면 멈추고 보고).
- `k_util_crane`(e1c) → qc_move_log+rtg_move_log 로컬(st_ts..comp_ts 병합 구간 — l_qc_q 의 병합 기법 재사용).
- `k_mph_realtime`·`k_qc_q`·`k_tt_cycle` → CHUNK 2 로컬 SQL 재사용.
- `qc_move_time`(학습기 — learn_qc_move_time 은 scengen·배차가 소비) → 같은 산식을 qc_move_log 로.
- `k_crane_q`·`k_crane_q_hour` → 위 로컬 소스.
- **`k_cycle`(e3b→raw_k_cycle) → Oracle 유지**: 로컬 대체는 tt_move_log 기반이라 raw_k_cycle의
  의미가 바뀐다(= 재정의, 위 참조). nightly 하루 1회뿐이라 부하 기여가 무시할 수준이므로
  재정의 결정 전까지 그대로 둔다.
- `k_empty` → Oracle 유지(trv_rng 재료 부족 — 08-08 후속).
- 결과: nightly 의 Oracle 왕복 = **2회/일**(k_empty·k_cycle). 08-08에 k_empty가 빠지고,
  K_CYCLE 재정의를 채택하면 k_cycle도 빠져 0이 된다.

### 6-4. 검증 (숫자 보고)
1. **어제(08-05) 재계산 대조**: `extractor run --kpi <각각> 2026-08-05` 를 로컬 모드로 돌려
   kpi 표에 저장된 기존(Oracle산) 값과 나란히 — KPI별 (old, new, 차이%). k_empty 제외.
   이것이 절체의 최종 안전망이다. "터무니없는" 차이(설명 안 되는 >15%)가 나오면 절체 중단·보고.
2. t1 수동 1회: 저널에 Oracle/toolbox 호출 로그 0 + kpi_shift 갱신 확인.
3. t2 수동 1회: 동일 + K_RTG_Q·K_CYCLE 값이 로컬 계산과 일치.
4. 프론트 무손상: 대시보드 KPI API 응답 200·값 존재(엔드포인트는 crates/api/src/routes.rs 에서 확인).
5. 부하 장부 재실행: t1 이 Oracle 0 이 된 것을 왕복 수로 확인.

### 6-5. 고지(보고서에 포함, 코드 아님)
대시보드 표시값 이동 **1건**: K_RTG_Q ~+6%(원본 c08의 ~10중 계수 결함 교정 — 로컬이 옳음).
K_CYCLE은 이 런에서 **바뀌지 않는다**(재정의는 별건). 나머지 KPI는 0.0~0.1% 동일.

## SCOPE — 이 런에서 하지 않는 것

- ~~KPI 절체·nightly 폐기~~ → CHUNK 6 으로 승격(사용자 지시). 단 **K_EMPTY 절체만 08-08 이후**(재료 부족)
- CHUNK 5 구현 (사용자 결정 대기)
- k_empty 병산 (4-1 컬럼이 차오른 뒤에나 의미 — 다음 런)
- gate 15분화는 CHUNK 1-3의 OnCalendar 전환에 포함하지 **않는다** — 주기 자체는 5분 유지
  (컨테이너별 게이트 시각의 소비가 시나리오뿐이지만, 이번 런은 분산만)
- 시나리오/에뮬 산출 스키마 변경 일절 없음

## UNRESOLVED (오케스트레이터/사용자 몫)

1. **CHUNK 5 착수 시점** — 배차 에이전트의 workpool.rs 재측정 체크포인트(PLAN.md) 종료 후? 사용자 결정.
2. **stowplan 2분 주기** — 델타로 사실상 공짜지만 Oracle 왕복은 12→30회/h 증가(회당 무게는 1/20).
   총 왕복 감소분 안에서 흡수되나, "왕복 수 자체"가 민감하면 5분 유지로 변경 가능. 기본값: 2분.


---

## CHUNK 7 — 남은 4건 (사용자 승인 2026-08-06: "3가지 다 지금 진행" + workpool 재판정)

⚠ 사용자 고지: **지금은 고객 서비스 단계가 아니라 데이터가 일부 비어도 무방**하다.
따라서 08-08 대기 같은 시간 게이트는 해제한다. 단 라이브 배차·대시보드가 **죽는** 것은 여전히 불가.

### 7-1. workpool 왕복 줄이기 (델타 아님 — 실측 재판정)
★재판정 근거(2026-08-06 실측): workpool 전체 SELECT(1,270행·323KB) = **2.61초**,
COUNT(*)만 = 2.01초 ⇒ **왕복 고정비 ~2초가 지배하고 payload 기여는 0.6초뿐**.
그러므로 지렛대는 행 줄이기(델타)가 아니라 **왕복 수 줄이기**다. CHUNK 5 델타는 방향 자체가
빗나갔고 병합 키 위험만 떠안았다 — 재시도하지 마라.

(a) **assigned 를 본쿼리에 병합**: `sql/workpool.sql` 의 WHERE 를
    `JOB_ODR_JOBSTATUS IN ('A','B','Q')` 로 넓히고 jobtype 필터를 제거(또는 DS/LD/MI/MO 포함),
    SELECT 에 `JOB_ODR_YTNO` 유지. `SQL_ASSIGNED` 호출 제거.
    ⚠**로컬에서 갈라 담는다**: `live_workpool` 은 지금과 **완전히 동일한 모집단**
    (jobtype DS/LD + (status A 또는 (Q이고 ytno 빈))) 만 담고, `live_assigned_tt` 는
    ytno 가 있는 모든 행(기존 assigned 의미)으로. 소비자가 보는 내용이 달라지면 실패다.
    검증: 병합 전후 `live_workpool` 행수·jobtype 분포가 같은 범위인지 대조.
(b) **vessel_schedule 분리**: `tick_workpool` 에서 제거 → 새 서브커맨드 +
    `tt-vessel-schedule.{service,timer}`(5분·OnCalendar 초 분산). CHUNK 5 잔재 파일이
    untracked 로 남아 있으니 **재사용하되 내용을 검토 후** 쓸 것.
(c) 주기는 90초 유지(배차 신선도 — 60초로 당기는 것은 이번 범위 밖).
검증: 틱당 Oracle 쿼리 2회(pool·workqueue)로 감소 확인(저널/코드), 틱 벽시계 전후 비교,
`live_workpool` 행수 정상(±10%), `live_assigned_tt` 갱신 지속, dispatch_pred_sample 5분 내.

### 7-2. K_EMPTY 로컬 전환 (08-08 대기 해제)
`tos_handover_label.trv_rng` 는 2026-08-06 15시(KST)부터 착지 중.
`sql/e4_k_empty_decomposition.sql` 의 산식을 `sql/local/l_empty.sql` 로 이식
(원본 필터 `LNDN_TRV_RNG BETWEEN 0 AND 5000` 등 그대로).
`kpis/k_empty.rs` + `shift.rs::src_empty` 를 `KPI_NIGHTLY_SRC`/`KPI_T1_SRC` 기존 게이트에 맞춰 분기.
★검증은 **오늘 15시 이후 창**으로만 대조(그 이전은 재료가 NULL이라 비교 불가 — 정상).
차이가 크면 원인을 밝히되, 재료 부족 구간이면 "데이터 공백"으로 보고하고 진행.

### 7-3. K_CYCLE 표시 정의 재정의 + k_cycle 로컬화 (사용자 승인)
`kpi_shift.K_CYCLE` 과 nightly `raw_k_cycle` 을 **tt_move_log 기반(`l_cycle`)으로 재정의**한다.
근거: 기존 정의가 실제 사이클을 41% 과소 계상(노트 기록), 병산에서 두 값이 이미 관측됨
(예: 749.50 vs 1201.80). 절체가 아니라 **의도된 재정의**이므로:
- t1 `src_cycle` 의 K_CYCLE 쓰기를 `l_cycle` 산출로 교체(정의 변경). c10 기반 값은
  **키 `K_TT_CYCLE` 로 계속 쓴다**(별도 지표로 보존 — 지우지 마라).
- nightly `k_cycle`(e3b) → `l_cycle` 로컬 산출로 교체.
- 이제 t1(3분)만 K_CYCLE 을 쓰므로 CHUNK 6 이 우려한 경합은 없다. t2 는 쓰지 않는다.
검증: 어제 하루 old/new 대조(큰 차이가 **정상** — 재정의다), `/api/kpis` 200 + K_CYCLE 값 존재,
프론트가 K_CYCLE 을 읽는 지점(crates/api, web/src grep·읽기만)이 단위 가정(초)을 깨지 않는지 확인.

### 7-4. 잔재 정리
CHUNK 5 철회로 남은 untracked 중 **7-1(b)에서 쓰지 않는 것**만 삭제:
`crates/extractor/sql/workpool_delta.sql`, `workqueue_delta.sql`,
`db/migrations/0138_workpool_delta.sql`, `deploy/systemd/tt-workpool-recon.*`.
(`tt-vessel-schedule.*` 는 7-1(b)에서 사용.)
또한 고아 SQL `sql/e3a_k_util_tt_merged.sql` 삭제(k_util_tt 제거로 참조 0).

### 7-5. 최종 부하 측정
`bash scripts/oracle_load_ledger.sh` + 25분 창 직접 집계로 왕복/h·Oracle 시간/h 를 내고,
이 문서 "현재 부하 실측" 절의 기준선(522회/h)과 대조해 보고.
