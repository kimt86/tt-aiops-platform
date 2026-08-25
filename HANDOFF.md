# HANDOFF — 다음 사이클

마지막 갱신 2026-08-25. 앞선 사이클(하루 3묶음·전부 main): ①**DISPATCH_MODE 제거**(`1279d03`·mig0158)
②**순간 비교에 실제 추천 성적(reco_*) 병기**(`19e591b`·mig0159) + **none 60% 진단** ③**재지향 갈래**
(`874127e`~`0c01904`·pool_ver 8·mig0160·리뷰 SHIP·SHOULD_FIX 2건 반영).

> 상시 사실·기준선·함정은 `~/.claude/notes/tt-aiops-platform.md`(150줄 상한). 이 파일은 **다음에 할 일**만.

---

## DONE CRITERIA (작업을 끝냈다고 말하기 전에 전부 돌릴 것)

```bash
cargo build --release -p tt-api && cargo build --release -p tt-extractor
cargo test --workspace                      # 전부 통과·실패 0 (직전 기록 70)
systemctl --user is-active tt-api           # active
# 유닛 드리프트 — ⚠tt-scenario-* 는 제외(별도 저장소가 배포 주체)
for f in deploy/systemd/*.service deploy/systemd/*.timer; do b=$(basename "$f");
  case "$b" in tt-scenario-*) continue;; esac; i="$HOME/.config/systemd/user/$b"
  [ -f "$i" ] || { echo "미설치: $b"; continue; }; diff -q "$f" "$i" >/dev/null || echo "차이: $b"; done
curl -s -o /dev/null -w '%{http_code}\n' localhost:8080/kc/dispatch/            # 200
psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/pull_model_coverage.sql     # ⑮ pool_ver 8
```
```sql
SELECT reason, count(*) FROM stage2_pool_truck_shadow WHERE pool_ver=8
  AND ts > now()-interval '2 hours' GROUP BY 1;   -- redirectable 갈래 살아있나
SELECT source, subject, severity, occurrences FROM ops_alert WHERE last_ts > now()-interval '3 hours';
```

## 지금 참인 것 (전에는 아니었던 것)

- **재지향 갈래 라이브(pool_ver 8·08-25 13:45 MYT~)**: 배차됨·픽업 전 공차(틱당 ~27~43대)가 풀에
  들어오고, 발행 작업이 이동+벌점 180초로 집으면 재지향 추천(`redirected_from` 표식·보드 칩).
  첫 3시간에 실발생 3건(양하·초근접). truck_n 불산입이라 발행량 불변.
- **선배정 확정**: Q+트럭만으로 "픽업 전 공차" 판별 금지 — Q틱의 7.7%가 실제 적재 중(A행 규칙+픽업
  가드로 잔여 ~1.1%). `docs/cycles/2026-08-25-redirectable-pool.md`.
- **none 진단**: TOS 배차 순간 우리 유효 추천 없음 DS 55.6/LD 65.9% — 트럭 풀 탓 아님(76~84% 풀에
  있었음), **발행 페이스(출항 균등) vs 크레인 실제 리듬** 괴리. 적하 none 81%는 그 큐에 발행 자체가
  없던 것(출항 여유 3~22h 선박).
- **reco_*(mig0159) 쌓이는 중**(08-25 ~10:00 MYT~): T1 에 보드에 떠 있던 실제 추천을 같은 자로 채점.
  `reco_src` 로 가른다(none 도 성적·NULL=평가불능). 기존 `our_ytno` 는 **상한 계기**(사후 최근접 가용).
- **⑮ ver8 첫 46분: 95.3%**(n=773·놓침 구성 종전과 동일 97%=예측 꼬리) — 회귀 신호 없음, 긴 창 재확인 필요.
- pool_ver COMMENT 주인 = 최신 경계 mig(현재 0160·0157 재실행 안전 검증됨).

## 일부러 안 한 것

- **전 작업 후보 확장(계층 2개)** — 사용자가 "1단계 = 스왑 가능 공차부터"로 순서를 정함. 이번 사이클은
  트럭 축까지. **비용 설계(계층 2개·각 층 순수 이동)는 제안 상태 — 시작 전 확정 필요.**
  긴급도 시계는 **출항 페이스 유지**로 사용자 확정(2026-08-25).
- **야간 재현율** — ver8 은 46분 주간 창뿐. 다음 측정은 긴 창(야간 포함)으로.
- **VS TOS 화면 ② 전환** — reco_* 24h+ 쌓인 뒤.

## 다음 후보 (한 줄 근거)

1. **pull 2/2 본체 — 전 작업 후보(계층 2개)**: none 55.6/65.9% 해소의 본체. 1계층=마감 도래(현행)
   우선 매칭 → 남는 트럭을 2계층=나머지 전 발행 지시(마감 이른 순·트럭 수까지)에. 각 층 비용은 순수
   이동(튜닝 상수 없음). 재지향 갈래와 결합해야 스왑 실효도 커진다. 판별자 필요(발행 계층 표식).
2. **reco_* 24h 재집계 + VS TOS 화면 전환**: none 비율을 헤드라인으로, 상한 계기(our_ytno)는 강등 표기.
3. **⑮ 긴 창(야간 포함) 재측정**: ver8 95.3%가 96%대로 수렴하는지.
4. **재지향 관찰**: 발생 빈도·품질 + TOS 자체 스왑(한 방향 재지향)과 대조 — redirected_from 이 정답지.
5. **TOS 기술 세션 준비**: `docs/tos-integration-handoff.md` 7문 + 양하 "트럭→QC 큐" 인터페이스 질문.
6. **운영자 채택/기각 기록 장치**: 파일럿 성적표 — 재는 장치가 아직 없다.
7. **잡무**: 평문 비밀번호(PGPASSWORD=wp·GitHub 원격) / 디스크 / 98% / 머지된 워크트리 2개 정리.

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

## 사용자가 답해야 하는 것

- **원격 push**: 로컬 main 이 origin/main 보다 8커밋 앞. 밀어도 되는가.
- **계층 2개 설계 확정**(다음 사이클 1번 시작 전) — 제안: 1계층 우선 매칭 후 잔여 트럭을 2계층에,
  각 층 비용은 순수 이동시간(λ 튜닝 없음).
- **야간 재현율을 따로 잴 것인가**(⑮ 긴 창 시점).
- (e) tt-handover 확장 시점 — 변화 없음.
