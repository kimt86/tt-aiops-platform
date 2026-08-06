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

### 2-1. mig `0133_kpi_parity.sql`
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
| `l_qc_q.sql` | `sql/f2_k_qc_q.sql` | `qc_move_log` | 같은 필터. 크레인별 comp_ts 정렬 후 유휴갭 분포(원본의 idle_sec 버킷 재현) |
| `l_tt_cycle.sql` | `sql/c10_k_tt_cycle.sql` | `qc_move_log` | trk_id별 연속 comp_ts 갭, 120~1200s만 |
| `l_crane_q.sql` | `sql/c08_k_crane_q.sql` | `tos_handover_label` | dis_ts(=YT_DIS_DT)·actv_ts(=ACTV_DT) NOT NULL, 갭=actv−dis 상당 — **원본 SQL을 열어 산식을 그대로 옮길 것**(창은 comp_ts 기준) |
| `l_cycle.sql` | `sql/e3b_k_cycle_refined_v2.sql` | `tt_move_log` | dispatch_ts→free_ts 사이클. **의미가 원본과 다름을 안다**(원본은 HISTORY 전이) — 병산 목적이 바로 그 차이 측정 |

주의: 원본 SQL의 수치 산식(버킷 경계·캡·가중)을 임의 해석하지 말고 파일을 열어 옮긴다.
k_crane_q_hour(e5)는 c08과 같은 소스라 이번엔 **c08만**(대표) 병산한다. k_empty는 CHUNK 4 뒤에만 가능(컬럼 없음) — 이번 런 제외.

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

### 3-1. mig `0134_stow_plan_delta.sql`
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

### 4-1. mig `0135_handover_vessel_rtg_pos.sql`
```sql
ALTER TABLE tos_handover_label ADD COLUMN IF NOT EXISTS vessel TEXT;
ALTER TABLE tos_handover_label ADD COLUMN IF NOT EXISTS voyage TEXT;
ALTER TABLE tos_handover_label ADD COLUMN IF NOT EXISTS trv_rng BIGINT;  -- k_empty용(LNDN_TRV_RNG)
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos1 BIGINT;  -- CRNT_PSN_IDX_NO1(블록)
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos2 BIGINT;  -- NO2(베이)
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos3 BIGINT;  -- NO3(열)
ALTER TABLE rtg_move_log ADD COLUMN IF NOT EXISTS pos4 BIGINT;  -- NO4(단)
```
CHECK 금지(RULES 2). 적용 전 `pg_stat_activity`에서 두 표를 만지는 세션 확인(RULES 8).

### 4-2. 본 추출기 SELECT 확장 (부하 0 전례: mig0109 3컬럼)
- `crates/extractor/src/handover.rs`: SELECT에 `JOB_HIST_VESSEL AS vessel, JOB_HIST_VOYAGE AS voyage, LNDN_TRV_RNG AS trv_rng`
  (trv_rng는 NUMBER → `Option<i64>`, RULES 3), 구조체·INSERT·바인딩 추가.
- `crates/extractor/src/rtg_moves.rs`: SELECT에 `CRNT_PSN_IDX_NO1..4 AS pos1..pos4`(`Option<i64>`), 착지 추가.
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

## CHUNK 5 — workpool 델타 (⚠ 보류 — UNRESOLVED 1 해소 후)

설계 확정분만 기록한다(구현 금지):
JOB_ORDER_LIST를 UPD_DT 워터마크 델타(비인덱스지만 표가 작아 ~1s 유지)로 거울화,
assigned는 거울에서 파생(별도 쿼리 폐지), workqueue도 같은 패턴, vessel_schedule은 5분 별도 유닛,
본체 주기 90→60초, 시간당 화해, env `WORKPOOL_MODE=delta|snapshot`.
**배차 에이전트가 workpool.rs를 활성 수정 중**(PLAN.md CHUNK A/B, 반나절 재측정 대기)이라
파일 충돌 위험 — 사용자 승인 전 착수 금지.

---

## REJECTED APPROACHES (헤매지 마라)

- **qc/rtg/handover 주기 완화** — 배차 실시간 요구의 본체. 금지.
- **KPI 즉시 절체(Oracle 경로 제거)** — 병산 1~2주 게이트 전 금지. 이 런은 병산까지.
- **nightly 폐기·k_util_tt 삭제** — 병산 게이트 뒤의 일. 이 런 범위 밖.
- **VSB_VOYAGE(선박스케줄) 델타화** — 228행뿐, 이득 없음. CHUNK 5에서 5분 스냅샷으로만.
- **contspec 미스장부** — 이미 구현됨(mig0122·`scenario.container_spec_miss`). 손대지 마라.
- **툴박스/브리지(Azure mcp-toolbox) 개선** — 공유 인프라. 접근 금지(ETW 게이트웨이와 동급).
- **Oracle측 인덱스/힌트 변경** — 읽기 전용 계정.
- **`DISTINCT ON`+`ORDER BY` 최신순 착각** — DISTINCT ON은 키 선두 정렬을 강제한다(1dcbcc2 교훈).
- **빈 집계를 0행으로 가정** — GROUP BY 없는 집계는 전-NULL 1행을 돌려준다(8ed4ea9 교훈). COUNT류는 `Option<i64>`.

## SCOPE — 이 런에서 하지 않는 것

- KPI 절체·nightly 대조전용 전환·폐기 (시간 게이트: 병산 1~2주)
- CHUNK 5 구현 (사용자 결정 대기)
- k_empty 병산 (4-1 컬럼이 차오른 뒤에나 의미 — 다음 런)
- gate 15분화는 CHUNK 1-3의 OnCalendar 전환에 포함하지 **않는다** — 주기 자체는 5분 유지
  (컨테이너별 게이트 시각의 소비가 시나리오뿐이지만, 이번 런은 분산만)
- 시나리오/에뮬 산출 스키마 변경 일절 없음

## UNRESOLVED (오케스트레이터/사용자 몫)

1. **CHUNK 5 착수 시점** — 배차 에이전트의 workpool.rs 재측정 체크포인트(PLAN.md) 종료 후? 사용자 결정.
2. **stowplan 2분 주기** — 델타로 사실상 공짜지만 Oracle 왕복은 12→30회/h 증가(회당 무게는 1/20).
   총 왕복 감소분 안에서 흡수되나, "왕복 수 자체"가 민감하면 5분 유지로 변경 가능. 기본값: 2분.
