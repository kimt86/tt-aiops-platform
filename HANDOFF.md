# HANDOFF — 현재 사이클 (2026-08-25 확정)

> 상시 사실·기준선·함정은 `~/.claude/notes/tt-aiops-platform.md`(150줄 상한). 이 파일은 **이번에 할 일**만.

## GOAL

마감이 아직 안 온 발행 지시까지 **2계층 후보**로 매칭에 넣어, "TOS가 배차하는 순간 우리 유효 추천이
없음"(none·`reco_src`) 비율 — 현재 **양하 55.6% / 적하 65.9%** — 을 낮춘다. (pull 2/2 본체)

## 설계 (사용자 확정 2026-08-25)

- **1계층 = 마감 도래 슬롯(현행)** 을 먼저 매칭 → **남는 트럭**을 **2계층 = 나머지 전 발행 지시**
  (마감 이른 순·트럭 수까지)에 매칭. 층을 순차로 풀어 1계층이 항상 우선권.
- **각 층 비용 = 순수 이동시간. 튜닝 상수(λ) 없음.**
- 긴급도 시계는 **출항 페이스 유지**(사용자 확정 2026-08-25). "작업>트럭 금지" 절단 설계 유지.
- 재지향 갈래(pool_ver 8)와 결합 동작 유지 — 2계층이 있어야 스왑 실효가 커진다.

## IN SCOPE

- 2계층 매칭 구현(매칭 틱 경로·`pool_tick.sql` 이 라이브 경로임에 유의).
- **발행 계층 판별자** 추가 + 새 마이그레이션(mig0161·멱등) — 그림자에서 계층을 가를 수 있게.
- 배선 후 성적: **none 재측정(reco_src·야간 포함 긴 창)** + **⑮ 풀 재현율 회귀 확인**(트럭 축은 안
  건드리므로 회귀 체크용).
- 배차 로직 변경 → **KC 배차 문서 갱신**.

## OUT OF SCOPE

VS TOS 화면 전환(reco_* 24h 후 별도) · tt-handover 확장(e안) · 야드 적재 사각 측정 · 재지향
anti-thrash 결정 · no_coord 원인 조사 · 워크트리 정리 등 잡무.

## DONE CRITERIA (전부 돌릴 것)

```bash
cargo build --release -p tt-api && cargo build --release -p tt-extractor
cargo test --workspace                      # 전부 통과·실패 0 (직전 기록 70)
systemctl --user is-active tt-api           # active
# 유닛 드리프트 — ⚠tt-scenario-* 는 제외
for f in deploy/systemd/*.service deploy/systemd/*.timer; do b=$(basename "$f");
  case "$b" in tt-scenario-*) continue;; esac; i="$HOME/.config/systemd/user/$b"
  [ -f "$i" ] || { echo "미설치: $b"; continue; }; diff -q "$f" "$i" >/dev/null || echo "차이: $b"; done
psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f db/migrations/0161_*.sql   # 두 번 돌려도 안전(멱등)
curl -s -o /dev/null -w '%{http_code}\n' localhost:8080/kc/dispatch/    # 200
```
- **계층 2 발행이 라이브에서 실제 관측**: 새 판별자로 가른 계층 2 행 > 0 (그림자 표 질의).
- **none 비율 하락**: 배선 후 야간 포함 긴 창(≥24h 지향)에서 양하 55.6/적하 65.9% 대비 하락.
  적하 none 의 81%가 2계층 대상이므로 구조상 내려가야 정상 — 목표치는 실측으로 정한다(사전 약속은 방향만).
- **⑮ 풀 재현율 유지**: `scripts/pull_model_coverage.sql` ⑮절 95%대·회귀 없음(같은 창).

## UNKNOWNS

- **판별자 형태**: 트럭 풀 모집단은 안 변하므로(트럭 축 그대로) pool_ver 승격이 아니라 매칭/발행
  그림자의 tier 컬럼이 맞을 수 있음 — /build 에서 판별자 규율(의미 변화면 ver·COMMENT 경계)로 결정.
- 2계층 발행분을 소비자(보드·비교기 reco_*)가 어떻게 받는지 — 표시·채점에 계층 구분 필요 여부.

## 이월된 미해소 항목 (지우지 말 것)

- **(1차 리뷰 CONSIDER·08-25) 재지향 판별의 야드 작업 사각**: 픽업 가드가 DS/LD 로그만 봐서 MI/MO 로
  적재 중 + 선배정 Q 인 트럭이 통과 가능(크기 미측정). GPS 적재 라벨 보조는 latched 잔류로 과잉 제외
  위험(98.7→87.7% 전례) — **측정 먼저**.
- **(〃) 재지향 트럭 anti-thrash 소멸**: 상수 벌점은 버킷 간 상대 순서 불변 → 틱마다 튈 수 있음.
  발생 드물어 지금은 낮은 위험·실배차 전환 전 결정.
- **(〃) 표시 비대칭**: redirectable 낡은 GPS held 미포함(need_pos 경로는 포함)·TT페이지 free_in 0 라벨.
- **REDIRECT_PENALTY_S 180초 적정성** — 관찰 데이터로 후속 판단.
- **no_coord 22.9/틱(~20%) 작업이 좌표 없어 매칭서 조용히 탈락** — 원인 미조사.
- **`tt-weather-live` 단발 실패**(08-21·재시도 여부 사용자 판단). ⚠`ops_alert`는 MYT·저널은 KST.
- **`web/public/livemap-roadgraph.geojson` 은 매시 크론이 다시 쓴다** — 항상 modified·커밋 경로 지정.
- `dispatch_compare_shadow` 에 `(tos_ytno,t1_ts)` 인덱스 없음 · `stage2_solver_shadow` DEADMAN 밖.
- **GPS 죽었는데 TOS 는 배차하는 트럭**(위치 나이 중앙 4.3h) — 재현율 3.2%p 상한·위치 원천 판단 대기.
- 화면 해석 문의 대비: BoardFunnel issued 08-24 04:07Z 점프(Q+트럭 편입) · QC active_moves 최대 ~9분
  이른 점등(무해) · 보드 "재지향" 칩(08-25 신설·KC board.html 설명 있음).
- 머지된 워크트리 2개(`kc-journal`·`ws-coverage-kc`) 미정리 — 잡무 세션에서.

## 결정 기록 (이번 사이클 확정분)

- 세션 주제 = 전 작업 후보 확장(계층 2개) — 사용자 확정.
- 설계 = 순차 계층(1계층 소진 → 잔여 트럭 2계층)·층 내 비용 순수 이동시간·튜닝 상수 없음 — 사용자 확정.
- 성적 창 = **야간 포함 긴 창까지 이번 세션에서**(세션이 하루를 넘겨 잡음) — 사용자 확정.
- ~~원격 push 허락~~ → 이미 origin/main 과 동기화돼 소멸(2026-08-25 확인).

## 사용자가 답해야 하는 것 (남은 것)

- (e) tt-handover 확장 시점 — 변화 없음.
