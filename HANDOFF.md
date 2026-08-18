# HANDOFF — 다음 사이클

마지막 갱신 2026-08-18. 앞선 사이클: 특별취급 절차를 TOS 로 확인해 KC 에 기록
(머지 `3bd0220`, main 에 푸시됨).

> 상시 사실·기준선·함정은 `~/.claude/notes/tt-aiops-platform.md` 에 있다(150줄·상한 도달).
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
# KC 문서가 실제로 서빙되는가 (포트는 deploy/systemd/tt-api.service 의 API_ADDR)
curl -s -o /dev/null -w '%{http_code}\n' localhost:8080/kc/dispatch/dispatch-deadline.html   # 200
```

---

## 지금 참인 것 (전에는 아니었던 것)

- **특별취급(냉동·위험물·OOG)에 대한 답이 닫혔다.** TOS 배차 경로에는 특별취급이 **없다**
  (근거 3갈래). 절차는 실재하지만 **배차가 아닌 자리**에 있다 — 냉동은 전원 연결/해제,
  위험물은 야드 적치 격리. "빼지 않는다" 결정은 유지되고 근거만 늘었다.
- **배차와 맞닿는 지점이 하나 발견됐다** — 적하 냉동의 전원 해제가 우리 배차 시점보다 늦은 것이
  사흘에 **300건**. 이 건수는 짝짓기 창을 ±24h~±1h 로 바꿔도 불변이다(300/300/300/299).
  **트럭이 실제로 기다리는지는 재지 않았다.**
- KC 3건이 사실에 맞춰졌다: `dispatch-deadline`(5장에 2026-08-18 항목 신설) ·
  `tos-verification`(로드맵 E) · `stage2-rollout`("범위에서 빼야" → 빼지 않되 못 보는 절차 명시).
  `tos-db-reference` 에 **3.6 특별취급 원장** 절 신설(우리가 안 긁는 표들·조회 함정).

## 일부러 안 한 것

- **전원 해제 지연이 트럭 대기로 이어지는지 재지 않았다** — 아래 후보 1번.
- **ETW 순서 문제를 여전히 손대지 않았다**(사용자 ㉠ 결정) — 후보 2번.
- 노트가 **150줄 상한에 도달**했다. 다음에 뭔가 추가하려면 먼저 쳐내야 한다.

## 이번 사이클에서 나온 범위 밖 발견

- **TOS 야드 배치 규칙 29건 중 하나가 `Yard AI`(2026-06-30 갱신)** — 야드 쪽에 이미 자동화가
  들어와 있는 것으로 보인다. 우리 배차와의 관계는 확인하지 않았다.
- **이 워크트리는 실제로 공유된다** — 이번 사이클 중 다른 손이 `4b7dfaa`(CLAUDE.md)와
  `c100919`(scengen 머지)를 넣었다. 남이 같은 시간에 커밋한다는 전제로 일할 것.
- 다 머지된 워크트리 2개(`kc-journal` · `ws-coverage-kc`)가 아직 남아 있다.

## 이월된 미해소 항목 (지우지 말 것)

- **미해소 래치 경보 `deadman/road_route_eval`** — 마지막 08-11 08:34. 아직 안 봤다.
  (`disk/filesystem` 은 마지막 08-09, 후보 8번과 같은 건이다.)
- **`web/public/livemap-roadgraph.geojson` 은 매시 크론이 다시 쓴다** — `git status` 에 **항상**
  modified 로 뜬다. 저장소가 더럽다는 근거로 쓰지 말고, 커밋에 딸려 들어가지 않게 경로를 지정할 것.

## 다음 후보 (한 줄 근거)

1. **★냉동 전원 해제가 트럭을 기다리게 하는가** — 이번에 남긴 유일한 미측정이고, 참이면 적하
   냉동의 마감 계산에 선행 작업 시간을 넣어야 한다. ⚠ **"해제가 배차보다 늦었는가"로 층화하지 말 것**
   — 해제가 픽업에 붙어 있어 `픽업−배차 ≥ 32분`과 거의 같은 말이 되는 동어반복이다. 냉동↔일반으로
   갈라 **야드 대기시간**을 봐야 하고, GPS 도착 앵커(`truck_pos_hist`)가 2일치라 창이 좁다.
2. **★ETW 가 작업목록 착지 뒤에 온다 — 측정 후 판단**(사용자 ㉠ 결정으로 미룬 것).
   추출기가 `workqueue → workpool → etw` 순이라 WORKPOOL 착지가 ETW 단계 **전에** 커밋된다.
   옛 `:15` 위상에서는 틱이 착지 ~20초 뒤였는데 지금은 0~2초 뒤다.
   ⇒ **먼저 잴 것**: 착지 직후와 20초 뒤의 값 차이, 그 차이가 `work_eta` 를 얼마나 움직이는가.
3. **`tt-workpool` 이 1분 주기를 못 지킨다** — 평균 60.3초. ETW 를 별도 유닛으로 떼면 본체가
   20초대로 내려갈 것으로 보이나(가설), ETW 는 공유 인프라라 경계 확인이 먼저다.
4. **TOS 기술 세션** — `docs/tos-integration-handoff.md` 의 7개 질문. 기술 쪽 유일한 크리티컬 패스.
   ★특별취급 질문은 이번 조사로 **"기록에 안 남는 사람 규칙이 있는가" 하나로 좁혀졌다.**
5. **운영자 채택/기각 기록 장치** — 파일럿 Phase 1→2 통과 기준이 "운영자 수용"인데 재는 장치가 없다.
6. **비교기 지표 재측정** — T1 절체(`t1_ver=1`) 후 평시 표본이 쌓였을 것이다.
7. **한산한 시간대 DEADMAN 오경보 해소** — `stage2_match_shadow` 가 "지금 할 일이 있는가"를 안 본다.
8. **평문 비밀번호 정리** — `scripts/*.sh` 의 `PGPASSWORD=wp`. GitHub 원격이 있다.
9. **디스크 root 영역** — 08-09 에 여유 20GiB 밑까지 갔다. root 권한자 몫.
10. **다 머지된 워크트리 2개 정리** — 미머지 커밋 0·작업트리 깨끗이라 지워도 유실 없다.

## 사용자가 답해야 하는 것

- **`DISPATCH_MODE=active` 를 언제 켤 것인가?** 코드 작업은 없다. 켜는 순간의 유일한 효과는
  "직전 180초에 우리가 추천한 상자를 풀에서 뺀다"이고, TOS 소비 채널이 없는 지금은 얻는 것이 없다.
- **TOS 세션에서 "기록에 안 남는 사람 규칙"을 물을 것인가** — 예를 들어 위험물을 보낼 때 무전으로
  승인을 받는가. 우리 자료로는 원리상 확인이 불가능하고, 있다면 배차 추천의 전제가 바뀐다.
