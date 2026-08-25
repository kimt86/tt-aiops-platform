# HANDOFF — 진행 중 사이클: 재지향 가능 공차를 후보 풀에 (pool_ver 8)

마지막 갱신 2026-08-25. 앞선 같은 날 작업(머지됨): DISPATCH_MODE 제거(`1279d03`·mig0158) ·
실제 추천 채점 reco_* 배선(`19e591b`·mig0159) · none 60% 진단(발행 페이스 vs 크레인 리듬).

> 상시 사실·기준선·함정은 `~/.claude/notes/tt-aiops-platform.md`. 이 파일은 이번 사이클 정의.

## GOAL

배차받고 픽업 전인 공차(재지향 가능)를 TOS 정본 기준으로 판별해 후보 트럭 풀에 넣고,
긴급 작업이 전환 벌점을 물고 그 트럭을 집을 수 있게 한다. 판별 오판율은 소급 측정으로 확정한다.

## IN SCOPE (순서 고정)

1. **선행 측정**: `assigned_tt_hist`(Q/A 라벨·3일) × 픽업 로그(양하 픽업=`qc_move_log` DS ·
   적하 픽업=`tos_handover_label` LD) 사후 대조 → ①Q인데 실제 픽업 후 오판율 ②Q→A 전환 지연.
   **중단 규칙: 오판율이 한 자릿수 %를 훌쩍 넘으면 배선 전 멈추고 보고.**
2. **풀 배선**: pool_tick 새 갈래 `reason='redirectable'` = live_workpool **Q+트럭** 행(정본)
   ∩ 픽업 로그 가드(마지막 픽업 ≤ 마지막 자유) ∩ 픽업 지점 근접+정지 제외(임계는 1이 정함).
   **POOL_VER 7→8**. ⚠재지향 트럭은 `truck_n`(Stage-1 슬롯 수)에 **세지 않는다** — 발행량 불변.
   ⚠inflight 갈래와 중복 시 redirectable 이 대체(한 트럭 한 행).
3. **행렬 인코딩**: redirectable 은 미배정=현상 유지. 발행 작업(마감 도래분)에만
   이동시간+`REDIRECT_PENALTY_S`(출발 180초)로 참여. 재지향 추천은 `stage2_match_shadow` 에
   표식 컬럼(`redirected_from`·mig0160)으로 구분. 보드에 최소 칩.
4. **검증**: 빌드·테스트·배포·드리프트 · ⑮ 재현율(pool_ver 8 로 갈림·96%대 유지) ·
   재지향 후보/추천 수 쿼리. **재지향 추천 0 이어도 합격**(트럭 잉여 체제 — 실효는 다음
   사이클 작업 확장과 결합할 때).

## OUT OF SCOPE

전 작업 후보 확장(계층 2개 — none 60% 본체·다음 사이클) · 긴급도 시계 변경(출항 페이스 유지·
2026-08-25 사용자 확정) · VS TOS 화면 ② 전환(reco_* 24h 후) · no_coord 22.9/틱 조사 · 스왑 실행.

## DONE CRITERIA

```bash
cargo build --release -p tt-api && cargo build --release -p tt-extractor
cargo test --workspace                      # 전부 통과 (직전 70)
systemctl --user is-active tt-api
for f in deploy/systemd/*.service deploy/systemd/*.timer; do b=$(basename "$f");
  case "$b" in tt-scenario-*) continue;; esac; i="$HOME/.config/systemd/user/$b"
  [ -f "$i" ] || { echo "미설치: $b"; continue; }; diff -q "$f" "$i" >/dev/null || echo "차이: $b"; done
curl -s -o /dev/null -w '%{http_code}\n' localhost:8080/kc/dispatch/            # 200
psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/pull_model_coverage.sql     # ⑮ pool_ver 8
```
```sql
SELECT reason, count(*) FROM stage2_pool_truck_shadow WHERE pool_ver=8
  AND ts > now()-interval '1 hour' GROUP BY 1;   -- redirectable 행 존재
SELECT count(*) FROM stage2_match_shadow WHERE redirected_from IS NOT NULL
  AND ts > now()-interval '1 hour';              -- 재지향 추천 수(0 합격·이유 설명)
```

- 오판율·전환 지연 숫자가 docs/cycles/2026-08-25-redirectable-pool.md 에 있다.
- ⑮ 재현율 96%대 유지(배포 직후라 창 짧음 — 짧은 창임을 명기).

## RESULT (2026-08-25 · 구현 완료·배포 13:45 MYT)

- **오판율 확정**: Q 라벨만 믿으면 7.7%가 실제 적재 중 — 오류가 아니라 **TOS 선배정**(적재
  운행 중 다음 작업 Q 배정·라이브 70대 중 25대). "A행 없음"+픽업 가드로 걸러 잔여 ~1.1%
  (미러 지연 창·Q→A 전환 p50 35~42초) → 게이트 통과. `docs/cycles/2026-08-25-redirectable-pool.md`.
- **배선 라이브**(pool_ver 8·mig0160·커밋 a01f986): 틱당 재지향 후보 ~27~43대 편입·발행량
  불변(truck_n 불산입)·⑮ 재현율 95.2%(21분 창·놓침 구성 동일=회귀 신호 없음·야간 포함 재확인 필요).
- **재지향 추천 실발생 2건**(양하·도착 69/84초·마감 초과 큐 포함) — 벌점을 물고도 이긴 사례.

## UNKNOWNS (남은 것)

벌점 180초의 적정성(후속 측정) · 재지향 추천의 장기 빈도·품질(reco_* 계기로 관찰) ·
⑮ 를 긴 창(야간 포함)으로 재확인.

## 이월된 미해소 항목 (지우지 말 것)

- **(1차 리뷰 CONSIDER·2026-08-25) 재지향 판별의 야드 작업 사각**: 픽업 가드가 DS/LD 로그만 봐서
  MI/MO 로 적재 중 + 선배정 Q 인 트럭이 통과할 수 있다(크기 미측정). GPS 적재 라벨 보조는
  latched 잔류로 과잉 제외 위험(98.7→87.7% 전례)이라 **측정 먼저**.
- **(〃) 재지향 트럭은 anti-thrash 가 사실상 꺼진다**: 상수 벌점은 버킷 간 상대 순서를 못 바꿔
  틱마다 다른 작업으로 튈 수 있다 — 발생 자체가 드물어 지금은 낮은 위험, 실배차 전환 전 결정.
- **(〃) 표시 비대칭**: 직접 경로 redirectable 낡은 GPS 는 held 미포함(need_pos 경로는 포함)·
  TT 페이지가 재지향 트럭을 free_in 0 으로 라벨.

- **pull 2/2 본체 — 전 작업 후보(계층 2개) 확장**: none 55.6/65.9%(DS/LD) 해소. 이번 사이클 뒤.
- **VS TOS 요약·헤드라인을 reco_*(제품 성적)로 전환** — 24h+ 쌓인 뒤. `our_ytno`=상한 계기임을 화면에도.
- **no_coord 22.9/틱(~20%) 작업이 좌표 없어 매칭서 조용히 탈락** — 원인 미조사.
- **`tt-weather-live` 단발 실패**(08-21·재시도 여부 사용자 판단). ⚠`ops_alert`는 MYT·저널은 KST.
- **`web/public/livemap-roadgraph.geojson` 은 매시 크론이 다시 쓴다** — 항상 modified·커밋 경로 지정.
- `dispatch_compare_shadow` 에 `(tos_ytno,t1_ts)` 인덱스 없음 · `stage2_solver_shadow` DEADMAN 밖.
- **GPS 죽었는데 TOS 는 배차하는 트럭**(위치 나이 중앙 4.3h) — 재현율 3.2%p 상한·위치 원천 판단 대기.
- 화면 해석 문의 대비: BoardFunnel issued 08-24 04:07Z 점프(Q+트럭 편입) · QC active_moves 최대 ~9분 이른 점등(무해).
- 잡무 3건: 평문 비밀번호(PGPASSWORD=wp·GitHub 원격) / 디스크 / 98% / 머지된 워크트리 2개 정리.
- **야간 풀 재현율 미측정**(96.4%는 주간 창) — 다음 재현율 측정은 야간 포함으로.

## 사용자가 답해야 하는 것

- (e) tt-handover 확장 시점 — churn 관측·요청 순간 정답지가 필요해질 때.
- 야간 재현율을 따로 잴 것인가.
