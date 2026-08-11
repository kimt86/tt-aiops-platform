# HANDOFF — 이번 사이클

마지막 갱신 2026-08-12. 앞선 사이클: 배차 탐지 지연(머지 `412901c`).

> 상세 기준선·함정은 `~/.claude/notes/tt-aiops-platform.md` 2026-08-11~12 절에 있다.

---

## GOAL

매칭 틱이 고정 초(`:15`)가 아니라 **작업목록이 실제로 착지한 것을 신호로** 돌아서, 매칭이
쓰는 목록의 나이가 착지 지연·추출 소요와 무관하게 항상 몇 초 안쪽이 된다.

## 왜 (C안을 고른 근거)

`:15` 고정 위상의 전제는 "착지가 :00~:09 에 몰린다"였다. 2026-08-12 07~08시 재측정에서
그 전제가 깨졌다:

- `tt-workpool` 실행 소요(68회·70분): p50 **21초** · p90 **65초** · 최대 **84초**
  (Oracle 풀+Postgres 착지 평균 20.2초 / 착지 이후 ETW 등 평균 10.5초)
- 60초를 넘긴 실행 뒤에는 systemd 가 같은 초에 `Finished`→`Starting` 을 찍는다 = **착지 초가
  자유주행**한다. 실제 착지 초 분포는 **15~59 로 흩어져** 있다.
- 그 결과 매칭이 쓴 목록 나이: 하루 1,436틱 중 **20틱이 45초 초과**, 그 20틱이 06:43~07:15 MYT
  한 덩어리에 몰려 있다(그 시간대만 보면 19틱 중 13틱 · 평균 42.3초).

⇒ 고정 초를 어디로 옮겨도 자유주행하는 착지를 따라갈 수 없다. 상수를 없앤다.

**Oracle 부하는 늘지 않는다**(확인): `crates/api` 에 Oracle 의존성 0건·호출부 0건이고,
C안의 신호는 로컬 Postgres 표 한 행이다(`data_freshness` 조회 실측 **0.109ms · buffers 3**).
`tt-workpool.timer` 의 주기도 안 건드린다.

---

## IN SCOPE

**코드 — `crates/api/src/livemap.rs`**

1. `MATCH_TICK_SEC` · `phase_delay_ms` · 동반 `const _: () = assert!` 제거(참조 15곳).
2. `spawn_stage2_shadow` 루프 머리를 **대기 루프**로 교체:
   - `data_freshness(WORKPOOL)` 를 **2초마다** 조회, `last_success_at` 이 앞으로 가면 깨어난다.
   - 최대 **60초** 기다려도 안 바뀌면 깨어난다(폴백). 추출이 죽어 매칭이 조용히 서는 것을 막는다.
   - 폴백에서도 매칭을 **돌린다** — 목록이 같아도 트럭 GPS 가 바뀌어 결과가 달라진다.
   - 기동 직후 첫 회는 즉시 돈다.
3. 신선도 질의에 `last_success_at` 원시값 한 컬럼 추가 — **같은 질의**. 대기 루프의 마지막
   조회 결과를 게이트(`workpool_stale_reason`)·`workpool_age_s` 가 재사용 → 틱당 추가 질의 0.
4. 순수 함수 `should_wake(직전_착지, 이번_착지, 기다린_ms) -> Landing | Fallback | KeepWaiting`
   으로 판정을 분리·테스트. **시각을 통째로 받아** 인자 뒤바꿈이 테스트를 통과하지 못하게 한다.
5. `wake_src` 를 `stage2_solver_shadow` INSERT 에 바인딩(`landing` / `fallback`).
6. `match_tick_phase_tests` 6건 삭제, `should_wake` 테스트 신설.
   **돌연변이로 실효성 확인**(폴백 분기를 항상 참으로 바꿔 잡히는지).
7. "`tt-workpool.timer` 의 `:55` 와 한 쌍" 경고 주석 제거 — C안에서 짝이 사라진다.

**DB — `db/migrations/0153_solver_wake_src.sql`** (멱등)

- `stage2_solver_shadow.wake_src text` 추가
- `workpool_age_s` 대역 경계 COMMENT: NULL=~08-11 / 6~15초=`:15` 고정 위상 / **C안 이후 0~3초**.
  `mig0150` 의 "`MATCH_TICK_SEC` 만 바꾸고 타이머를 안 맞춘다" 서술이 낡았음도 기록.

**KC — 2줄**

- `kc/dashboard/board.html` "1분마다 계산한" → "새 작업 목록이 들어올 때마다(보통 1분에 한 번)"
- `kc/dashboard/learning.html` "2단계 매칭 그림자(60초)" → "(작업목록 착지마다)"

## OUT OF SCOPE

- **`tt-workpool` 실행이 60초를 넘기는 문제** — 위 분해까지만 기록하고 손대지 않는다. 추출기 별건.
- `deploy/systemd/tt-workpool.timer` 의 `:55` — 안 건드린다(다른 유닛과의 격자 회피는 그대로 유효).
- **LISTEN/NOTIFY** — 더 깔끔하나 `crates/extractor` 와 프로세스 간 계약이 생기고 알림 유실
  대비 폴백이 어차피 필요하다. 폴링 실측 비용이 시간당 0.2초 DB 시간이라 지금은 값어치가 없다.
- A2(추출 폴링 60→30초) · 비교기(`spawn_dispatch_compare`, 별도 60초 루프) · 경보 임계 신설.
- 경계 이전 `workpool_age_s` 소급 재해석 — 판별자로 가른다.

## DONE CRITERIA

```bash
cargo build --release -p tt-api && cargo test --workspace   # 실패 0
psql -f db/migrations/0153_solver_wake_src.sql             # 두 번 돌려도 안전
systemctl --user restart tt-api && systemctl --user is-active tt-api   # active
```
배포 **2시간 뒤**:
```sql
SELECT wake_src, count(*) AS 틱,
       round(avg(workpool_age_s),1) AS 평균,
       percentile_cont(0.99) WITHIN GROUP (ORDER BY workpool_age_s) AS p99,
       count(*) FILTER (WHERE workpool_age_s > 45) AS 초과45
  FROM stage2_solver_shadow WHERE ts > now()-interval '2 hours' GROUP BY 1;
```

| 기대 | 값 |
|---|---|
| `wake_src='landing'` 틱의 p99 | **≤ 5초** (지금 15~71 · 최근 1h 평균 26.6) |
| `wake_src='landing'` 틱의 >45초 | **0건** (지금 하루 20건) |
| `wake_src` NULL | 0건 (경계 이후 전부 채워짐 = 배선 정상) |
| `wake_src='fallback'` 비율 | **계기로 기록만 · 임계 안 건다.** 이 값이 곧 "추출이 분당 못 따라온 비율"이라 0 이 아닐 것으로 예상 |
| 시간당 총 틱 | 55~62 (지금 58~60에서 안 떨어져야 함) |

추가: `stage2_reco/stale_workpool` 경보 0건 · 추천 생산 0 경보 0건.

## UNKNOWNS

1. **폴백 비율이 얼마나 나올지 모른다.** 오늘 실행 p90 65초·최대 84초라 낮지 않을 수 있다.
   크게 나오면 C안의 실패가 아니라 **추출 소요 문제의 계기**다 — 그래서 임계를 안 건다.
2. `live_workpool` 커밋 → `last_success_at` 갱신 순서는 확인됨(`kpis/common.rs:38-45` 가 작업
   클로저의 `tx.commit()` 이후 `finish_run` 호출). **부분 읽기 위험 없음.**
3. 폴링 간격 2초는 선택값(1초면 대역 0~1초·시간당 3,600회≈0.4초 DB 시간). 둘 다 무시할 수준.
