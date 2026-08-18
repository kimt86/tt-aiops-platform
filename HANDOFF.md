# HANDOFF — 다음 사이클

마지막 갱신 2026-08-18. 앞선 사이클: ETW 가 배차에 쓰이는지 전수 확인 → **후보 2·3번을 코드 변경
없이 닫음**(사용자 결정: 유닛 분리 대신 기록).

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

- **ETW 는 표시 전용이다.** `work_eta`·마감·매칭 산식에 없다. 읽는 곳은 화면 정렬 2곳과 프론트 칩뿐.
  유일한 간접 경로(ETW 정렬 → 활성 선박 → 구역 정렬)는 **무브 기준 다선박 QC 가 0** 이라 안 열린다.
  ⇒ **후보 2번(ETW 가 착지 뒤에 온다) 소멸** — 낡아도 배차 숫자를 못 건드린다.
- **★후보 3번은 오측정이었다 — 철회.** `tt-workpool` 실행 소요는 **평균 6.49초**(저널 360회)이고
  시작 간격은 60.00초로 타이머를 정확히 지킨다. 매 분 ~53초 여유가 있다. 종전 기준선
  "평균 60.3초·주기를 못 지킨다"는 **착지 간격을 실행 소요로 읽은 것**이었다.
- **`dispatch_pred_sample.etw_qc_ts` 는 죽은 컬럼** — 삽입부가 `None` 하드코딩이라 7일 0/126,527행.
  mig0084 가 의도한 "ETW vs 우리 예측" 비교는 한 번도 가능한 적이 없었다.
- KC 는 손대지 않았다 — 확인 결과 ETW 를 "TOS 가 주는 값"으로만 설명하고 있어 틀린 서술이 없다
  (우리 마감 문서 `dispatch-deadline.html` 에는 ETW 언급 자체가 없다).

## 일부러 안 한 것

- **ETW 를 별도 유닛으로 떼지 않았다**(사용자 결정). 이득이 "여유 53초짜리 틱에서 평균 2.5초"로
  줄어 라이브 배선을 건드릴 명분이 없어졌다. 되살리려면 근거부터 다시 세울 것.
- **죽은 컬럼 `etw_qc_ts` 를 치우지 않았다** — 아래 후보 6번.
- **냉동 전원 해제가 트럭을 기다리게 하는지 재지 않았다**(사용자 결정: TOS 팀과 직접 회의에서 확인).

## 이번 사이클에서 나온 범위 밖 발견

- **노트의 함정 항목에 있던 `tt-etw` 유닛은 존재하지 않는다.** 우리 쪽은 `tt-workpool` 안의 ETW
  단계 + 터널 `wp-etw-bridge` 뿐이다. 노트를 고쳤다(사용자 지시 자체는 그대로 유효).
- **내가 상관 서브쿼리를 라이브 DB 에 던져 2분 넘게 안 끝나 취소했다**(`pg_cancel_backend`).
  파이프라인 피해는 없었다(취소 직후 ETW 38초 전·WORKPOOL 43초 전·솔버 정상). 노트에 함정으로 적음.

## 이월된 미해소 항목 (지우지 말 것)

- **미해소 래치 경보 `deadman/road_route_eval`** — 마지막 08-11 08:34. 아직 안 봤다.
- **`disk/filesystem` crit 이 계속 뛰고 있다** — 07-29 이후 212회, 마지막 08-18 07:31.
  "디스크 여유 17GiB 미만". root 권한자 몫이라 우리가 못 고친다(후보 8번).
- **`web/public/livemap-roadgraph.geojson` 은 매시 크론이 다시 쓴다** — `git status` 에 **항상**
  modified 로 뜬다. 저장소가 더럽다는 근거로 쓰지 말고, 커밋에 딸려 들어가지 않게 경로를 지정할 것.

## 다음 후보 (한 줄 근거)

1. **TOS 기술 세션** — `docs/tos-integration-handoff.md` 의 7개 질문. 기술 쪽 유일한 크리티컬 패스.
   ★이번 사이클로 물을 것이 둘 늘었다: **"기록에 안 남는 사람 규칙이 있는가"**(위험물 무전 승인 등)와
   **"적하 냉동의 전원 해제가 트럭을 기다리게 하는가"**(사흘 300건 관측·트럭 대기는 미측정).
2. **운영자 채택/기각 기록 장치** — 파일럿 Phase 1→2 통과 기준이 "운영자 수용"인데 재는 장치가 없다.
   지금의 채택률은 "TOS 와 같은 상자"이지 사람의 판단이 아니다.
3. **비교기 지표 재측정** — T1 절체(`t1_ver=1`) 후 평시 표본이 쌓였을 것이다.
4. **한산한 시간대 DEADMAN 오경보 해소** — `stage2_match_shadow` 가 "지금 할 일이 있는가"를 안 본다.
5. **`deadman/road_route_eval` 래치 경보 확인** — 08-11 이후 아무도 안 봤다.
6. **죽은 배선 정리 2건** — `etw_qc_ts`(`None` 하드코딩)와 `stage2_solver_shadow` 가 DEADMAN 밖인 것.
   둘 다 "있다고 착각하게 만드는" 종류라 지우거나 채우거나 둘 중 하나.
7. **평문 비밀번호 정리** — `scripts/*.sh` 의 `PGPASSWORD=wp`. GitHub 원격이 있다.
8. **디스크 root 영역** — 여유 17GiB 밑. root 권한자 몫.
9. **다 머지된 워크트리 2개 정리**(`kc-journal`·`ws-coverage-kc`) — 미머지 커밋 0·작업트리 깨끗.

## 사용자가 답해야 하는 것

- **`DISPATCH_MODE=active` 를 언제 켤 것인가?** 코드 작업은 없다. 켜는 순간의 유일한 효과는
  "직전 180초에 우리가 추천한 상자를 풀에서 뺀다"이고, TOS 소비 채널이 없는 지금은 얻는 것이 없다.
