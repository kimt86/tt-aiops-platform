# HANDOFF — 배차 탐지 지연 줄이기 (A1 위상 고정 · A3 배차시각 실물화)

정의 확정 2026-08-11. 앞선 사이클(실배차 라이브 마무리 9건, `094a274`~`326f5cb`)은 완료·푸시됨.

> 상세 기준선·함정은 `~/.claude/notes/tt-aiops-platform.md` 2026-08-11 절에 있다.
> 이 파일은 **이번 사이클에 무엇을 할지**만 담는다.

---

## 배경 — 이번 작업이 왜 나왔나

"TOS가 배차한 것을 우리가 언제 아는가"를 실측했다(2026-08-11 05:47~05:53Z).

**정의(분모)**: TOS가 `JOB_ORDER_LIST` 행을 갱신한 시각(`UPD_DT`) → 그 행이 우리 Postgres
`live_workpool` 에 보이기까지. `live_workpool` 을 5초마다 관찰해 ytno가 새로 채워진 상자 **54건**.

```
최소 9s · 중앙 33s · p90 58s · 최대 67s     ← 9~67초 균등 분포 = 60초 폴링 격자의 서명
```

⚠ 한계 2건: ①`UPD_DT` 는 배차 전용 컬럼이 아니라 행 마지막 갱신 시각(배차 시각의 **대리값**)
②Oracle↔우리 서버 시계 차이를 배제 못 했다. **상한에 가까운 추정**으로 읽을 것.

지연은 두 토막인데, **둘째가 설계된 값이 아니다**:

| 토막 | 크기 | 원인 |
|---|---|---|
| ① TOS → 우리 DB | 중앙 33초 | `tt-workpool` 매분 :55 1회 |
| ② 우리 DB → 매칭이 사용 | 29초 | 매칭 틱 :29, 목록 착지 :00 |

②의 위상 = **`tt-api` 프로세스가 시작한 초**다(`tokio::time::interval`). 오늘 12:00:29 시작 →
틱 :29(실측). **재배포마다 0~59초에서 다시 굴러간다.**

---

## GOAL

매칭 틱이 **재배포와 무관하게 항상 목록 착지 직후에 돌고**, TOS 배차 시각을
대리값(`UPD_DT`)이 아니라 실물 컬럼으로 기록한다.

---

## IN SCOPE

### A1 — 매칭 틱 위상 고정 (`crates/api/src/livemap.rs:4405`)

`spawn_stage2_shadow` 의 틱을 **매분 :15 에 고정**한다. 첫 틱 전에 다음 :15 까지 재우고 이후 60초.

**:15 의 근거** — 착지 지연 실측(:55 시작 기준, 6시간·n=710):

```
최소 -9s · p50 +5s · p90 +6s · p99 +7s · 최대 +14s   → :00~:02 착지, 최악 :09
```

관측 최대 착지(:09)보다 6초 뒤라 느린 Oracle 틱(같은 질의 2~15초 편차)에도 새 목록을 받는다.
착지 전에 돌면 실패가 아니라 **한 세대 낡은 목록**을 쓴다(신선도 게이트 300초는 안 걸림) —
조용한 퇴화라서 마진을 둔다.

**값어치는 14초가 아니다.** 오늘 기준 ② 29초 → 15초(=14초 이득)지만, 본질은
**0~60초짜리 주사위를 15초 상수로 바꾸는 것**이다. 기대 이득 ~15초, 최악의 경우 ~45초.

- 위상 계산은 **순수 함수로 분리하고 테스트를 붙인다** — 신선도 게이트 `workpool_stale_reason`
  과 같은 방식. 라이브에서 위상을 시험하려면 재시작이 필요하므로 테스트로 고정한다.
- 다른 틱 루프는 건드리지 않는다(OUT OF SCOPE).

### A3 — `YT_DIS_DT` 채택 (프로브 선행 · 왕복 증가 0)

**먼저 프로브 1회.** 저장소 문서가 정면으로 어긋나 있다:

- `db/migrations/0048` → "`JOB_ORDER_LIST` 는 `YT_DIS_DT`=**after-arrival** 만 있다"
- `db/migrations/0092`·`0115` → "`YT_DIS_DT` = TOS가 이 트럭을 **배차한 순간**"

프로브(읽기 전용·범위 제한·이미 스캔하는 표):

1. `ALL_TAB_COLUMNS` 로 `YT_DIS_DT` **타입 확인** — DATE 면 `TO_CHAR` 필수.
   타입 단정으로 handover·rtg-moves 가 2분 정지한 전례가 있다.
2. `JOB_ODR_JOBSTATUS='A'` ∧ `YTNO` 채워짐 ∧ `UPD_DT` 최근 2분 이내(= 방금 배차된 행)에서
   `YT_DIS_DT` 가 **이미 채워져 있는가**. 채워짐 → 배차 시각 / 비어 있음 → 도착 후.

**"배차 시각"이면** 착지시킨다: `mig0148`(`live_workpool.yt_dis_ts`) +
`crates/extractor/sql/workpool.sql` SELECT 추가 + `MoveRow`/`PoolTickRow` 에 `Option<String>`
필드 + insert 배선. 이미 스캔하는 행이라 **Oracle 왕복 증가 0**.

**"도착 후"면** A3 는 버리고 `0092`/`0115` 서술을 정정하는 기록만 남긴다(코드 변경 없음).

---

## OUT OF SCOPE

- **A2 (workpool 폴링 60초 → 30초) — 다음 세션으로 확정 이월**(사용자 결정 2026-08-11).
  한 줄 변경이 아니다:
  - 1분 유닛 4개가 20초 격자에 있고(`:05` qc-moves / `:25` rtg-moves / `:45` handover /
    `:55` workpool) **30초 간격으로 비어 있는 슬롯 쌍이 없다.** 격자 재설계가 필요하다.
  - `ORACLE_LOCK` 은 **프로세스 안에서만** 도는 Mutex(`runner.rs:10`). 유닛들은 각각 별도
    프로세스라 **유닛 간 Oracle 직렬화가 없다** — 폴링 2배는 겹침 위험을 키우는데 겹침은
    측정된 적이 없다.
  - 부하 결정 필요(+60 왕복/시간 · 현재 총 ~336/시간은 2026-08-06 값, 오늘 재측정 안 함).
  - ★**A1·A3 뒤에 다시 판단할 것**: 매칭이 60초 주기라 목록이 30초마다 갱신돼도 매칭이 얻는
    이득은 30초가 아니라 평균 ~15초다. A1으로 위상이 고정되면 그 이득은 더 줄어든다.
- 매칭 틱 주기 자체(60초)는 안 바꾼다.
- `livemap.rs` 의 나머지 틱 루프 19개, `roadgraph.rs` 1개 — 같은 위상 문제를 가질 수 있으나
  이번 대상은 배차 매칭 하나다.
- `crates/scengen` — 다른 에이전트 담당.
- `DISPATCH_MODE` 는 `shadow` 그대로.

---

## DONE CRITERIA (끝났다고 말하기 전에 전부 돌릴 것)

```bash
cargo build --release -p tt-api && cargo build --release -p tt-extractor
cargo test --workspace          # 61 통과 · 실패 0  (현재 59 + 위상 테스트 2건)
systemctl --user is-active tt-api
```

**A1 — 위상이 고정됐는가 (핵심: 재시작 두 번)**
```sql
SELECT DISTINCT to_char(ts,'SS') FROM stage2_solver_shadow WHERE ts > now()-interval '10 minutes';
```
→ 단일값 `15`. **`tt-api` 를 아무 초에나 두 번 재시작해도 같은 값**이어야 한다.
지금은 재시작 시각의 초가 그대로 나온다(오늘 실측: 12:00:29 시작 → `29`).

**A1 — 목록 나이가 줄었는가**
```sql
SELECT EXTRACT(epoch FROM now()-last_success_at)::int FROM data_freshness WHERE kpi_key='WORKPOOL';
```
→ 매칭 시점(:15)에 **약 15초**. 종전 :29 시점 29초.

**A3 — 채웠는가** (프로브가 배차 시각으로 나온 경우만)
```sql
SELECT count(*) AS 배차행, count(yt_dis_ts) AS 채움 FROM live_workpool WHERE coalesce(ytno,'')<>'';
```
→ 채움률 **기대값은 프로브가 정한다**. 지금 못 박지 않는다 — 못 박으면 프로브 결과에 맞춰
기준을 사후 조정하게 된다.

**A3 — Oracle 부하가 안 늘었는가**
```bash
journalctl --user -u tt-workpool --since "30 min ago" -o short-iso | grep 'kpi="WORKPOOL"'
```
→ 착지 지연 p90 **+6초 근처 유지**(기준선 6시간·n=710: p50 +5 · p90 +6 · p99 +7 · 최대 +14).

**회귀 없음**
```sql
SELECT count(*) FROM stage2_solver_shadow WHERE ts > now()-interval '1 hour';   -- 60
SELECT count(*) FROM ops_alert WHERE last_ts > now()-interval '1 hour';         -- 0
```

**유닛 드리프트 0**
```bash
for f in deploy/systemd/*.service deploy/systemd/*.timer; do
  b=$(basename "$f"); i="$HOME/.config/systemd/user/$b"
  [ -f "$i" ] || { echo "미설치: $b"; continue; }
  diff -q "$f" "$i" >/dev/null || echo "차이: $b"
done
```

**탐지 지연 재측정** — A3 가 착지하면 `upd_ts` 대리값이 아니라 `yt_dis_ts` 실물로 다시 잰다.
기준선 = 오늘 실측 **중앙 33초 · p90 58초 · 최대 67초**(n=54, 05:47~05:53Z).

---

## UNKNOWNS

1. **`YT_DIS_DT` 가 배차 시각인지 도착 후인지** — 문서 2건이 어긋난다. 프로브 하나로 닫힌다.
   **A3의 존폐가 여기 달렸다.**
2. **`YT_DIS_DT` 의 Oracle 타입** — `ALL_TAB_COLUMNS` 로 확인. DATE 면 `TO_CHAR` 없이는
   배치째 디코드 실패한다.
3. **`tt-api` 재시작 중 매칭 공백** — 위상 고정을 위해 첫 틱을 최대 59초 늦추면 재시작 직후
   한 틱이 빌 수 있다. `stage2_match_shadow` DEADMAN(30분)에는 안 걸린다. 다른 방식(첫 틱은
   즉시, 그다음부터 정렬)이 나을지는 구현하며 판단한다. 어느 쪽이든 최종 결과는 같다.
4. **A1의 이득이 실배차 전에 실질적인가** — 중복 추천 피해는 `self_recent` TTL 180초가 이미
   막는다(중앙 62초 ≪ 180초). 즉각 이득은 "TOS가 방금 가져간 상자를 더 빨리 후보에서 뺀다"
   (`tos_assigned`) 정도이고 측정해 본 적 없다. **본질적으로 실배차 준비 작업이다.**

---

## 이 사이클 뒤로 남는 것 (앞 사이클에서 이월)

1. **A2** — 위 OUT OF SCOPE 참조. 격자 재설계 + 부하 결정.
2. **TOS 기술 세션** — `docs/tos-integration-handoff.md` 의 7개 질문. 기술 쪽 유일한 크리티컬
   패스. ★배차 탐지도 여기 걸려 있다: TOS가 우리 추천을 소비하면 "TOS 배차를 탐지"할 필요가
   줄고(우리가 보낸 것의 ack 를 받으면 되므로), Oracle push(CDC·트리거·AQ)는 우리가 못 만든다
   — 읽기 전용 폴링이 유일한 경로다(디스커버리 확인: CDC/Kafka/Debezium 0건).
3. **운영자 채택/기각 기록 장치** — 파일럿 Phase 1→2 통과 기준이 "운영자 수용"인데 재는 장치가
   없다. 지금의 46.9%("TOS와 같은 상자")는 사람의 판단이 아니다.
4. **평문 비밀번호 정리** — `scripts/*.sh` 의 `PGPASSWORD=wp`. GitHub 원격이 있다.
5. **`scripts/unit_failed_alert.sh` 를 scengen 유닛에도** — 담당이 달라 합의 필요.
6. **디스크 root 영역** — 08-09 에 여유 20GiB 밑까지 갔다. 2026-08-11 현재 94GiB 로 회복했고
   경보는 08-10 00:38 이후 재발 없음(`ops_alert` 행은 래치라 남아 있는 것). root 권한자 몫.

## 사용자가 답해야 하는 것

- **`DISPATCH_MODE=active` 를 언제 켤 것인가?** 켜는 순간의 유일한 효과는 "직전 180초에 우리가
  추천한 상자를 풀에서 뺀다"이고, TOS 소비 채널이 없는 지금은 얻는 것이 없다(최근 12시간 실측:
  후보 묶음의 80%가 자기 추천 적중 — 켜면 첫 틱에 그만큼 빠진다).
- **TOS 에 "냉동·위험물·OOG 배차에 사람이 지키는 절차가 있는가"를 물을 것인가?** 있다면
  "빼지 않는다" 결정을 뒤집어야 한다. 우리 자료로는 확인 불가.
