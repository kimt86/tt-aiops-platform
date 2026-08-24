# HANDOFF — 현재 사이클 (scope 확정 2026-08-24)

앞선 사이클: **배차된 Q행 착지**(pool_ver 7·머지 `8b809c2`·마감 `09b810d`). 상시 사실·기준선·함정은
`~/.claude/notes/tt-aiops-platform.md`. 이 파일은 **이번에 할 일**만.

> 이번 사이클 착수 전 정정(노트 반영 완료): **슬롯 절단은 설계다** — 작업은 항상 후보 트럭 수만큼
> 긴급(마감 이른) 순으로 끌어오며 작업>트럭 금지(`livemap.rs:5336`·사용자 확인). "발행량이 판단이
> 아니다"던 이전 서술은 오독이었다.

---

## GOAL

pool_ver 7 구간의 풀 재현율을 같은 잣대로 재측정해 96.6% 수준 유지 여부를 확정하고,
적하 커버리지 −4.4%p 후퇴의 원인을 숫자로 규명한다 — 우리 코드 결함이고 수정이 작으면
회수까지(사용자 확정 2026-08-24: "진단 + 소규모 수정까지").

## IN SCOPE

- **재측정**: `scripts/pull_model_coverage.sql` ⑮절을 pool_ver 7 구간(2026-08-24 04:08Z 이후·
  반나절 이상 창)으로. 분모 = `assigned_tt_hist` 첫 등재 요청·**pool_ver 로 가름**(노트 규율).
- **적하 −4.4%p 진단**: 기준이 된 원측정(어느 창·어느 판 대비)을 기록에서 복원 → DS/LD 갈래별로
  어느 조건에서 빠지는지 원인 확정.
- 원인이 우리 착지/소속 코드 결함 + 수정이 작으면(±수십 줄) 수정까지. 풀 의미가 바뀌면
  판별자(pool_ver 8) 규율 적용.
- 결과를 노트 기준선(⑮절 줄)과 이 파일에 반영.

## OUT OF SCOPE

- 슬롯 절단 분모(`truck_n`) 재설계 — 이번 재측정으로 실체 확인 후 별도 사이클.
- B안(양하 큐 단위 커버)·(e) tt-handover 확장·active 전환·잡무 3건(비밀번호·디스크·워크트리).

## DONE CRITERIA

- 재현율 숫자 + **분모 한 문장**: "pool_ver 7 구간에서 트럭이 물어본(자유 뒤 `assigned_tt_hist`
  첫 등재) N건 중 그 순간 풀에 있던 비율". 합격선 = pool_ver 6 동등(≥96% 부근).
  미달이면 원인 규명까지가 이 세션의 몫.
- 적하 −4.4%p 에 대해 "원인은 X"를 측정 근거와 함께 진술(측정 사실과 해석은 문장 분리).
  수정했다면 수정 후 재측정으로 회수 확인.
- **코드를 고쳤을 경우에만** 아래 표준 블록 전부:

```bash
cargo build --release -p tt-api && cargo build --release -p tt-extractor
cargo test --workspace                      # 70 통과 · 실패 0
systemctl --user is-active tt-api           # active
# 유닛 드리프트 — ⚠tt-scenario-* 는 제외(별도 저장소가 배포 주체)
for f in deploy/systemd/*.service deploy/systemd/*.timer; do b=$(basename "$f");
  case "$b" in tt-scenario-*) continue;; esac; i="$HOME/.config/systemd/user/$b"
  [ -f "$i" ] || { echo "미설치: $b"; continue; }; diff -q "$f" "$i" >/dev/null || echo "차이: $b"; done
curl -s -o /dev/null -w '%{http_code}\n' localhost:8080/kc/dispatch/            # 200
```

## UNKNOWNS

- −4.4%p 의 정확한 출처(어느 측정·어느 창 대비) — 세션 첫 단계에서 복원.
- 원인이 구조(적하는 픽업 직후 작업 목록에서 사라지는 성질 등)일 가능성 — 그 경우 회수는
  설계 작업이라 **진단 보고까지만** 하고 다음 사이클로.

---

## 이월된 미해소 항목 (지우지 말 것)

- **`tt-weather-live` 단발 실패**(08-21·재시도 붙일지 사용자 판단). ⚠`ops_alert` 는 MYT·저널은 KST — 1시간 함정.
- **`web/public/livemap-roadgraph.geojson` 은 매시 크론이 다시 쓴다** — 항상 modified·커밋 경로 지정.
- `dispatch_compare_shadow` 에 `(tos_ytno,t1_ts)` 인덱스 없음 · `etw_qc_ts` 죽은 컬럼 · `stage2_solver_shadow` DEADMAN 밖.
- **GPS 죽었는데 TOS 는 배차하는 트럭**(위치 나이 중앙 4.3h) — 재현율 3.2%p 상한. 위치 원천 붙일지 판단 대기.
- 화면 해석 문의 대비 2건: BoardFunnel `issued` 가 08-24 04:07Z 경계에서 ~120상자 점프(Q+트럭 편입·단차
  아님) · QC 카드 `active_moves`/지도 "작업 중"이 첫 픽업보다 최대 ~9분 일찍 켜짐(의도된 무해 판정).

## 사용자가 답해야 하는 것 (이번 범위 밖)

- **`DISPATCH_MODE=active` 시점** — 코드 작업 없음·병목은 TOS 협의.
- **(e) 확장 시점** — churn 관측·요청 순간 정답지가 필요해질 때(B안 검증 또는 TOS 협의 자료).
- ~~적하 −4.4%p 회수 여부~~ → **답변됨(2026-08-24): 이번 사이클에서 진단+소규모 수정까지.**

## 이후 후보 (이 사이클 다음)

1. pull 2/2 잔여 — `truck_n` 분모의 실체 확인 결과에 따라 재설계 여부 판단.
2. B안 양하 큐 단위 커버 · 3. TOS 기술 세션 · 4. (e)+프로브 P1~P9 · 5. 운영자 채택/기각 기록 장치 ·
6. 잡무 3건(평문 비밀번호·디스크 `/` 98%·머지된 워크트리 2개 정리).
