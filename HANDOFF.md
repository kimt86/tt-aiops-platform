# HANDOFF — 다음 사이클

마지막 갱신 2026-08-24. 앞선 사이클: **배차된 Q행 착지**(머지 `8b809c2`·pool_ver 7).
TOS 배차 인식이 픽업 후(양하 p50 544초) → **배차 ≤60초**(실측 DS 54/LD 44초)가 됐다.
Oracle 무접촉·SQL 무변경 — 이미 오던 행을 Rust 착지가 버리던 것을 고쳤다.

> 상시 사실·기준선·함정은 `~/.claude/notes/tt-aiops-platform.md`(150줄 상한). 이 파일은 **다음에 할 일**만.

---

## DONE CRITERIA (작업을 끝냈다고 말하기 전에 전부 돌릴 것)

```bash
cargo build --release -p tt-api && cargo build --release -p tt-extractor
cargo test --workspace                      # 70 통과 · 실패 0
systemctl --user is-active tt-api           # active
# 유닛 드리프트 — ⚠tt-scenario-* 는 제외한다(별도 저장소가 배포 주체)
for f in deploy/systemd/*.service deploy/systemd/*.timer; do b=$(basename "$f");
  case "$b" in tt-scenario-*) continue;; esac; i="$HOME/.config/systemd/user/$b"
  [ -f "$i" ] || { echo "미설치: $b"; continue; }; diff -q "$f" "$i" >/dev/null || echo "차이: $b"; done
curl -s -o /dev/null -w '%{http_code}\n' localhost:8080/kc/dispatch/            # 200
```
```bash
# 풀 재현율 (⑮절 헤드라인 · pool_ver 로 갈린다 — 현행 7, 2026-08-24 04:08Z~)
psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/pull_model_coverage.sql
```
```sql
SELECT wake_src, count(*) 틱, round(avg(workpool_age_s),1) 나이 FROM stage2_solver_shadow
 WHERE ts > now()-interval '2 hours' GROUP BY 1;                 -- 55~62틱/시간·전부 landing
SELECT source, subject, severity, occurrences FROM ops_alert WHERE last_ts > now()-interval '3 hours';
-- 탐지 지연(분모 규칙: t1_ts 경계·t1_ts 단위 dedup — mig0157 COMMENT): DS p50 ≤120초 유지
```

---

## 지금 참인 것 (전에는 아니었던 것)

- **`live_workpool` 이 Q+트럭(배차됨·픽업 전) 행을 담는다**(pool_ver 7·경계 2026-08-24 04:06:55Z).
  `tos_assigned` 가 배차 ≤60초에 참. jobstatus 원문('Q') 보존이라 하류가 A/Q 구분 가능.
- **탐지 지연 기준선**: DS p50 54/p90 79 · LD 44/73초(n=347/469). 측정 분모 규칙은 mig0157 COMMENT.
- **A안(pool_tick 에 JOB_ORDER_HISTORY 갈래 접기)은 기각 확정** — 멀티에이전트 리뷰 15개·BLOCKING 3
  (생사 결합·캐치업 폭주·중복 스트림). **JH 이력이 필요하면 (e) tt-handover 질의 확장**이 유일 경로
  (`handover.rs` 가 이미 매분 폴링·조건 11개·프로브 P1~P9 = 메모리 `reference_job_order_history.md`).
- **배차 실단위(사용자 확인)**: 양하 = (QC 큐, 트럭 수)·적하 = 상자. 양하 상자↔트럭 바인딩은 픽업까지
  유동(TOS 스왑 = 한 방향 재지향). KC `tos-db-reference.html` 에 문서화.
- **3일 프룬 실동작 확인**(이월 항목 해소) — `stage2_pool_truck_shadow`·`assigned_tt_hist` min(ts)이
  정확히 3일 경계. RETENTION 정상.

## 일부러 안 한 것

- **(e) tt-handover 확장** — 프로브 P1~P9(사람이 Oracle 에 1회씩)와 (b′) 효과 재측정이 선행 조건.
  이번 재측정 결과 잔여 지연이 ~54초(추출 주기 자체)라 **(e)의 시급성은 낮아졌다** — 60초보다 빠른
  소비처가 실명으로 생길 때만.
- **B안(양하 자기추천 커버를 상자 TTL → 큐 단위 카운트)** — active 전환 전 실질 작업. 실단위 확정으로
  설계 근거는 갖춰졌다.
- **착지 진리표 유닛 테스트**(리뷰 C6) — 분류 함수 추출이 필요해 다음 정리로.
- 자기추천 180초 값 — 적하는 실측상 충분(p90 78초 시절 기준)·양하는 B안이 대체할 것이라 그대로.

## 이번 사이클에서 나온 범위 밖 발견

- **BoardFunnel `issued` 게이지가 경계(08-24 04:07Z)에서 ~120상자 위로 점프** — Q+트럭 편입.
  계기는 옳아졌지만 시계열을 경계 너머로 이으면 단차로 보인다.
- **QC 카드 `active_moves`·지도 "작업 중" 판정이 첫 픽업보다 최대 ~9분 일찍 켜진다** — 의도한 무해
  판정(주석 기록)이나, 화면 해석 문의가 오면 이것이다.

## 다음 후보 (한 줄 근거)

1. **pull 2/2 — 슬롯·배정 순서** — 풀은 96.6%인데 "몇 개를 누구에게"는 그대로. 적하 커버리지 하락
   (−4.4%p)도 여기서 회수. pool_ver 7 재현율 재측정(반나절 창)을 겸한다.
2. **B안 — 양하 커버 큐 단위 재설계** — 실단위 확정·탐지 ≤60초 확보로 선행 조건이 다 갖춰졌다.
3. **TOS 기술 세션** — `docs/tos-integration-handoff.md` 7개 질문 + 새 질문(양하 지시 인터페이스를
   상자 계약이 아니라 "트럭 → QC 큐"로 — 스왑과 충돌하지 않게).
4. **(e) tt-handover 확장 + 프로브 P1~P9** — churn 관측·요청 순간 정답지가 필요해지면. 시급성은 낮아짐.
5. **운영자 채택/기각 기록 장치** — 파일럿 Phase 1→2 통과 기준인데 재는 장치가 없다.
6. **평문 비밀번호**(`scripts/*.sh` `PGPASSWORD=wp`·GitHub 원격 있음) / **디스크 `/` 98%**(root 몫) /
   **머지된 워크트리 2개 정리**(`kc-journal`·`ws-coverage-kc`·미머지 0 확인됨).

## 이월된 미해소 항목 (지우지 말 것)

- **`tt-weather-live` 단발 실패**(08-21·재시도 붙일지 사용자 판단). ⚠`ops_alert` 는 MYT·저널은 KST — 1시간 함정.
- **`web/public/livemap-roadgraph.geojson` 은 매시 크론이 다시 쓴다** — 항상 modified·커밋 경로 지정.
- `dispatch_compare_shadow` 에 `(tos_ytno,t1_ts)` 인덱스 없음 · `etw_qc_ts` 죽은 컬럼 · `stage2_solver_shadow` DEADMAN 밖.
- **GPS 죽었는데 TOS 는 배차하는 트럭**(위치 나이 중앙 4.3h) — 재현율 3.2%p 상한. 위치 원천 붙일지 판단 대기.

## 사용자가 답해야 하는 것

- **`DISPATCH_MODE=active` 시점** — 코드 작업 없음·병목은 TOS 협의(변화 없음).
- **적하 커버리지 −4.4%p** 를 pull 2/2 에서 되돌릴 것인가(이월).
- **(e) 확장을 언제 할 것인가** — 잔여 지연이 추출 주기(~54초)뿐이라, churn 관측·요청 순간 정답지가
  필요한 시점(B안 검증 또는 TOS 협의 자료)에 맞추면 된다.
