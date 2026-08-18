# HANDOFF — 다음 사이클

마지막 갱신 2026-08-18. 앞선 사이클: 매칭을 착지 신호로 깨운다(머지 `2df856e`, main 에 푸시됨).

> 상세 기준선·함정은 `~/.claude/notes/tt-aiops-platform.md` 에 있다(상시 사실만·136줄).
> 이 파일은 **다음에 무엇을 할지**만 담는다.

---

## DONE CRITERIA (작업을 끝냈다고 말하기 전에 전부 돌릴 것)

```bash
cargo build --release -p tt-api && cargo build --release -p tt-extractor
cargo test --workspace                      # 70 통과 · 실패 0 이 기준
systemctl --user is-active tt-api           # active
```
```sql
-- 매칭이 착지를 따라가고 있는가. ★반드시 wake_src 로 가른다(나이로 이유를 추정하면 동어반복)
SELECT wake_src, count(*) AS 틱, round(avg(workpool_age_s),1) AS 평균,
       percentile_cont(0.99) WITHIN GROUP (ORDER BY workpool_age_s) AS p99,
       count(*) FILTER (WHERE workpool_age_s > 45) AS 초과45
  FROM stage2_solver_shadow WHERE ts > now()-interval '2 hours' GROUP BY 1;

-- 파이프라인 생존 (틱 55~62/시간 · 경보 0)
SELECT EXTRACT(epoch FROM now()-max(ts))::int AS 마지막틱_초전,
       (SELECT count(*) FROM stage2_solver_shadow WHERE ts > now()-interval '1 hour') AS 최근1h_틱,
       (SELECT count(*) FROM ops_alert WHERE last_ts > now()-interval '3 hours') AS 최근3h_경보
  FROM stage2_solver_shadow;
```
```bash
# 유닛 드리프트 0 (저장소 == 설치본)
for f in deploy/systemd/*.service deploy/systemd/*.timer; do
  b=$(basename "$f"); i="$HOME/.config/systemd/user/$b"
  [ -f "$i" ] || { echo "미설치: $b"; continue; }
  diff -q "$f" "$i" >/dev/null || echo "차이: $b"
done
```
2026-08-18 실측: 70/0 · active · 틱 60.0/h · 경보 0 · 드리프트 0 ·
**landing 8,373틱(6일) 전부 평균 1.0초·최대 2초·45초 초과 0·하트비트 0.**

---

## 지금 참인 것 (전에는 아니었던 것)

- **매칭 틱이 작업목록 착지마다 돈다.** 신호 = `data_freshness(WORKPOOL).last_success_at` 전진
  (폴링 2초). 새 목록이 2분 30초 안 오면 하트비트로 한 번 돈다.
  **고정 초(`MATCH_TICK_SEC`)와 `tt-workpool.timer` 의 `:55` 짝 관계는 사라졌다** —
  이제 타이머 초를 옮겨도 매칭이 따라간다.
- **판별자 `stage2_solver_shadow.wake_src`**(landing/fallback/startup, mig0153). 집계는 반드시
  이걸로 먼저 가른다. `workpool_age_s` 는 **목록 나이가 아니라 착지 이후 경과**다(내용은 ~20초 더 오래됨).
- **안티스래시 `prev` 창이 `PREV_WINDOW_S` 상수**(하트비트+60초). 하트비트와 같은 값이면
  하트비트 틱에서 전환 벌점이 통째로 꺼진다 — 테스트가 관계를 고정한다.
- 헬스 `up` 임계 120 → **180초**(하트비트보다 커야 한다).
- `tt-workpool` 실행이 **1분 주기를 못 지킨다**(평균 60.3초·최대 89초)는 것이 실측으로 확정됐다.

## 일부러 안 한 것

- **ETW 순서 문제를 손대지 않았다**(사용자 ㉠ 결정) — 아래 후보 1번.
- **`stage2_solver_shadow` 를 DEADMAN 에 넣지 않았다.** 이번에 생긴 문제가 아니고 범위 밖이라
  주석으로만 명시했다. 그 표만 INSERT 가 깨지면 경보 없이 warn 로그만 남는다.
- **하트비트 비율에 경보를 안 달았다.** 지금 0%라 정상 대역이 아직 없다.
- **`tt-workpool.timer` 의 `:55` 를 안 건드렸다** — 다른 유닛과의 20초 격자 회피 이유는 그대로 유효.

## 이번 사이클에서 나온 범위 밖 발견

- **★`stage2_match_shadow` DEADMAN(30분)이 한산한 시간대에 오경보한다.** "지금 할 일이 있는가"를
  안 보기 때문. 실측 2026-08-16 23:20 경보: 그 50분간 솔버 틱은 **51회 정상**이었고 그중 **39틱이
  `n_works=0`** 이라 추천 행이 안 생긴 것뿐이었다. `zero_production` 은 올바르게 안 떴다.
- **`tt-workpool` 실행 소요의 약 1/6이 ETW**(착지 이후 평균 10.5초·최대 35초). ETW 실패가 잦다
  (30분 창 47건 `etw snapshot fetch failed`). ETW 는 Azure HTTP 라 **Oracle 부하가 아니다.**
- 기존 경보 2건이 미해소 래치로 남아 있다: `disk/filesystem`(마지막 08-09), `deadman/road_route_eval`
  (마지막 08-11 08:34). `unit/tt-qc-moves`·`unit/tt-stowplan` 경보는 툴박스 SSH 타임아웃이 원인이었다.
- `web/public/livemap-roadgraph.geojson` 은 매시 크론이 다시 써서 항상 modified 로 뜬다.

## 다음 후보 (한 줄 근거)

1. **★ETW 가 작업목록 착지 뒤에 온다 — 측정 후 판단**(사용자 ㉠ 결정으로 미룬 것).
   추출기가 `workqueue → workpool → etw` 순이라 WORKPOOL 착지가 ETW 단계 **전에** 커밋된다.
   옛 `:15` 위상에서는 틱이 착지 ~20초 뒤라 ETW 가 대개 끝난 뒤였는데, 지금은 0~2초 뒤라
   **ETW 갱신 중에 읽는다.** 없애려던 "한 세대 낡음"이 다른 표로 옮겨갔을 가능성.
   ⇒ **먼저 잴 것**: 착지 직후와 20초 뒤의 `tos_etw_cntr` 값이 실제로 얼마나 다른가, 그 차이가
   `work_eta` 를 얼마나 움직이는가. 차이가 작으면 아무것도 안 해도 된다.
2. **`tt-workpool` 이 1분 주기를 못 지키는 것** — 평균 60.3초. C안 덕에 매칭은 따라가지만
   **목록 자체가 1분에 한 번 안 온다**(24시간 p50 60.0·p90 66.2초). ETW 를 별도 유닛으로 떼면
   본체가 20초대로 내려갈 것으로 보이나(가설), ETW 는 공유 인프라라 경계 확인이 먼저다.
3. **TOS 기술 세션** — `docs/tos-integration-handoff.md` 의 7개 질문. 기술 쪽 유일한 크리티컬 패스.
   Oracle push(CDC·트리거·AQ)는 우리가 못 만든다 — 읽기 전용 폴링이 유일한 경로다(디스커버리 확인).
4. **운영자 채택/기각 기록 장치** — 파일럿 Phase 1→2 통과 기준이 "운영자 수용"인데 재는 장치가 없다.
   지금의 채택률은 "TOS 와 같은 상자"이지 사람의 판단이 아니다.
5. **비교기 지표 재측정** — T1 절체(`t1_ver=1`) 후 평시 표본이 쌓였을 것이다. 갈라서 다시 볼 것.
6. **한산한 시간대 DEADMAN 오경보 해소** — 위 발견. `n_works>0` 을 게이트로 걸거나 솔버 표를 보게.
7. **평문 비밀번호 정리** — `scripts/*.sh` 의 `PGPASSWORD=wp`. GitHub 원격이 있다.
8. **디스크 root 영역** — 08-09 에 여유 20GiB 밑까지 갔다. root 권한자 몫.

## 사용자가 답해야 하는 것

- **`DISPATCH_MODE=active` 를 언제 켤 것인가?** 코드 작업은 없다. 켜는 순간의 유일한 효과는
  "직전 180초에 우리가 추천한 상자를 풀에서 뺀다"이고, TOS 소비 채널이 없는 지금은 얻는 것이 없다.
- **TOS 에 "냉동·위험물·OOG 배차에 사람이 지키는 절차가 있는가"를 물을 것인가?** 있다면
  "빼지 않는다" 결정을 뒤집어야 한다. 우리 자료로는 확인 불가이고, 배차 파이프라인 표에는
  특별취급 속성이 하나도 없어서 걸러야 하면 배선이 필요하다.
