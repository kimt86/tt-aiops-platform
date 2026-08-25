# HANDOFF — 다음 사이클

마지막 갱신 2026-08-26. 앞선 사이클(발행 2계층·전부 main): **남는 트럭에 마감 미도래 발행 지시를
"다음 일"로 미리 배정**(mig0161·`09b297f`~`40f7599`·리뷰 2회 FIX FIRST→SHIP 전부 반영).
**RESULT: none(TOS 배차 순간 유효 추천 없음) DS 59.0→29.5 / LD 67.9→32.4%** — 목표 달성.

> 상시 사실·기준선·함정은 `~/.claude/notes/tt-aiops-platform.md`(150줄 상한). 이 파일은 **다음에 할 일**만.

---

## DONE CRITERIA (작업을 끝냈다고 말하기 전에 전부 돌릴 것)

```bash
cargo build --release -p tt-api && cargo build --release -p tt-extractor
cargo test --workspace                      # 전부 통과·실패 0 (직전 기록 74)
systemctl --user is-active tt-api           # active
# 유닛 드리프트 — ⚠tt-scenario-* 는 제외(별도 저장소가 배포 주체)
for f in deploy/systemd/*.service deploy/systemd/*.timer; do b=$(basename "$f");
  case "$b" in tt-scenario-*) continue;; esac; i="$HOME/.config/systemd/user/$b"
  [ -f "$i" ] || { echo "미설치: $b"; continue; }; diff -q "$f" "$i" >/dev/null || echo "차이: $b"; done
curl -s -o /dev/null -w '%{http_code}\n' localhost:8080/kc/dispatch/            # 200
psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/pull_model_coverage.sql     # ⑮ pool_ver 8 = 96%대
```
```sql
SELECT count(*) FROM stage2_match_shadow WHERE match_tier=2 AND ts>now()-interval '1 hour'; -- 2계층 생존(>0)
SELECT reason, count(*) FROM stage2_pool_truck_shadow WHERE pool_ver=8
  AND ts > now()-interval '2 hours' GROUP BY 1;   -- redirectable 갈래 생존
SELECT source, subject, severity, occurrences FROM ops_alert WHERE last_ts > now()-interval '3 hours';
```

## 지금 참인 것 (전에는 아니었던 것)

- **발행 2계층 라이브(match_tier·08-25 15:20 MYT~)**: 1계층=마감 도래(항상 우선·순차 솔브) →
  남는 트럭을 2계층=마감 미도래 발행 지시에. 틱당 2계층 105~249건·슬롯 못 받은 트럭 상시 0.
- **none 절반 이하**(사건 단위·같은 질의·야간 포함 16h): DS 29.5/LD 32.4%. 하락분은 `reco_tier=2`
  표식으로 2계층에 직접 귀속. ⑮ 재현율 96.4%(n=15,150) 유지. `docs/cycles/2026-08-25-two-tier-issue.md`.
- **기존 계기는 전부 1계층만 보게 게이트**(지도 선·30분 요약·헬스·채택률·prev/self_cover/rank·분석
  스크립트) — reco_*(순간 비교)만 2계층 포함. ⚠**오염 창 08-25 15:20~15:41 MYT 의 1계층 switched 는
  시계열 비교에서 제외**(prev 버그 구간·수정 완료·현행 0.16%).
- 보드 "미리" 칩 신설·보드 풀 통계의 "작업 수"가 85→245대로 뛴 것은 2계층 편입(정상).

## 일부러 안 한 것

- **남은 none ~30% 진단** — 가설만 세움(작업목록 밖 유형·no_coord 탈락·발행 지시 없는 큐), 근거
  미확보. 이번 사이클은 배선+측정까지.
- **커버 사건의 트럭 일치 0.6~2.8%** — 과제로 안 잡음(양하 실단위는 (QC큐,트럭수)라 상자×트럭
  일치가 맞는 잣대 아님·적하만 추후 관찰).
- **REDIRECT_PENALTY_S·재지향 anti-thrash** — 종전 이월 그대로(발생 드묾).

## 다음 후보 (한 줄 근거)

1. **남은 none ~30% 진단**: 2계층이 덮은 뒤에도 3건 중 1건은 답이 없다 — 구성(목록 밖 유형 /
   no_coord / 미발행 큐 / 예측 꼬리)을 가르면 다음 지렛대가 정해진다.
2. **VS TOS 화면 ② 전환 + reco_* 24h 재집계**: reco_* 데이터가 이제 하루를 넘겼다 — none 비율을
   헤드라인으로, 상한 계기(our_ytno)는 강등 표기.
3. **재지향 관찰**: 발생 빈도·품질 + TOS 자체 스왑과 대조(redirected_from 이 정답지).
4. **TOS 기술 세션 준비**: `docs/tos-integration-handoff.md` 7문 + 양하 "트럭→QC 큐" 인터페이스 질문.
5. **운영자 채택/기각 기록 장치**: 파일럿 성적표 — 재는 장치가 아직 없다.
6. **잡무**: 평문 비밀번호(PGPASSWORD=wp·GitHub 원격) / 디스크 / 머지된 워크트리 2개
   (`kc-journal`·`ws-coverage-kc` — ⚠kc-journal 은 kc-keeper 에이전트 소유일 수 있어 확인 후 정리).

## 이월된 미해소 항목 (지우지 말 것)

- **(1차 리뷰 CONSIDER·08-25) 재지향 판별의 야드 작업 사각**: 픽업 가드가 DS/LD 로그만 봐서 MI/MO 로
  적재 중 + 선배정 Q 인 트럭이 통과 가능(크기 미측정). GPS 적재 라벨 보조는 latched 잔류로 과잉 제외
  위험(98.7→87.7% 전례) — **측정 먼저**.
- **(〃) 재지향 트럭 anti-thrash 소멸**: 상수 벌점은 버킷 간 상대 순서 불변 → 틱마다 튈 수 있음.
- **(〃) 표시 비대칭**: redirectable 낡은 GPS held 미포함(need_pos 경로는 포함)·TT페이지 free_in 0 라벨.
- **no_coord 22.9/틱(~20%) 작업이 좌표 없어 매칭서 조용히 탈락** — 원인 미조사·none 잔여 가설과 겹침.
- **`tt-weather` 단발 실패**(08-21·08-25 재발·재시도 여부 사용자 판단). **08-25 13:53 MYT 추출기 3유닛
  단발 warn**(rtg/qc-moves·handover·2계층 배포 전) — 재발 시 조사. ⚠`ops_alert`는 MYT·저널은 KST.
- **`web/public/livemap-roadgraph.geojson` 은 매시 크론이 다시 쓴다** — 항상 modified·커밋 경로 지정.
- `dispatch_compare_shadow` 에 `(tos_ytno,t1_ts)` 인덱스 없음 · `stage2_solver_shadow` DEADMAN 밖.
- **GPS 죽었는데 TOS 는 배차하는 트럭**(위치 나이 중앙 4.3h) — 재현율 3.2%p 상한·위치 원천 판단 대기.
- 화면 해석 문의 대비: 보드 "미리" 칩(08-25 신설·KC board.html 설명 있음) · 보드 작업 수 245대(2계층
  편입) · BoardFunnel issued 08-24 점프 · QC active_moves 이른 점등(무해).

## 사용자가 답해야 하는 것

- (e) tt-handover 확장 시점 — 변화 없음.
- 다음 사이클 주제(위 후보 1~6) — /scope 에서.
