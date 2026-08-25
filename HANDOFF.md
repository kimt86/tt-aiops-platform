# HANDOFF — 다음 사이클

마지막 갱신 2026-08-25. 앞선 사이클: **측정 사이클**(머지 `f1bfaae`·코드 무변경) — pool_ver 7
재현율 **96.4%** 확정 + 적하 −4.4%p 진단(**하락 실재·풀 축소 인과 기각·회수할 결함 없음**).

> 상시 사실·기준선·함정은 `~/.claude/notes/tt-aiops-platform.md`(150줄 상한). 이 파일은 **다음에 할 일**만.

---

## DONE CRITERIA (작업을 끝냈다고 말하기 전에 전부 돌릴 것)

```bash
cargo build --release -p tt-api && cargo build --release -p tt-extractor
cargo test --workspace                      # 전부 통과·실패 0 (직전 기록 70)
systemctl --user is-active tt-api           # active
# 유닛 드리프트 — ⚠tt-scenario-* 는 제외한다(별도 저장소가 배포 주체)
for f in deploy/systemd/*.service deploy/systemd/*.timer; do b=$(basename "$f");
  case "$b" in tt-scenario-*) continue;; esac; i="$HOME/.config/systemd/user/$b"
  [ -f "$i" ] || { echo "미설치: $b"; continue; }; diff -q "$f" "$i" >/dev/null || echo "차이: $b"; done
curl -s -o /dev/null -w '%{http_code}\n' localhost:8080/kc/dispatch/            # 200
```
```bash
# 풀 재현율 (⑮절 헤드라인 · pool_ver 로 갈린다 — 현행 7 · 직전 96.4%)
psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/pull_model_coverage.sql
```
```sql
SELECT wake_src, count(*) 틱, round(avg(workpool_age_s),1) 나이 FROM stage2_solver_shadow
 WHERE ts > now()-interval '2 hours' GROUP BY 1;                 -- 55~62틱/시간·전부 landing
SELECT source, subject, severity, occurrences FROM ops_alert WHERE last_ts > now()-interval '3 hours';
```

---

## 지금 참인 것 (전에는 아니었던 것)

- **pool_ver 7 재현율 96.4%**(150초·주간 6h31m 창·6,359건) — ver6(96.6%) 동등·합격. **야간 미측정.**
- **발행량 = 마감 도래 슬롯(작업 수요)이 정한다** — ~250대 체제에서 트럭 절단선은 안 물린다
  (슬롯 못 받은 트럭 상시 188~228·최솟값 41). "풀 축소→발행 감소" 인과는 기각.
- **적하 −4.4%p 는 결함이 아니다**: 하락은 그 창에서 실재(올바른 경계로 −4.9%p 재현)하나 날짜(수요)
  효과와 구분 불가·ver6 3일 창에서 자연 회복(23.6%). **pull 2/2 의 "커버리지 회수" 명분은 소멸** —
  남는 것은 DS>LD 격차(일자별 5~13%p)가 슬롯 정책의 성질이라는 사실.
- **판 경계는 UTC 기록으로**: ver5 08-21 02:25Z~09:08Z · ver6 ~08-24 04:07Z · ver7 04:06:55Z~
  (`f1bfaae` HANDOFF RESULT). **프룬되는 표(`stage2_pool_truck_shadow` 3일)에서 경계 재독 금지.**
- KC 검증 절(§8)에 "추천 개수는 물동량이 정한다 — 출렁임은 고장이 아니다" 노트 추가.

## 일부러 안 한 것

- **야간 재현율** — 96.4%는 주간(12:08~18:39 MYT) 창. 다음 재현율 측정은 야간 포함 창으로.
- **DS>LD 커버리지 격차(5~13%p) 원인 심층** — 희소 슬롯 배정이 비용(거리)만으로 정해지는 슬롯
  정책의 성질로 보이나(해석), 파고들면 pull 2/2 설계 작업이라 진단 보고에서 멈췄다.
- **truck_n 분모 재설계** — 절단이 안 물린다는 실체만 확인. 재설계 여부는 pull 2/2 에서.

## 다음 후보 (한 줄 근거)

1. **pull 2/2 — 슬롯·배정 순서**: 명분이 바뀌었다. "커버리지 회수"가 아니라 ①`truck_n` 분모의 의미
   (풀 전체 vs 곧 물어볼 트럭) ②희소 슬롯이 마감 아닌 거리로 배정되는 것(격차 5~13%p) ③사용자 설계
   불변식(작업=트럭 수만큼 긴급 순)과 현 코드의 정합 — 을 설계 질문으로 다루는 사이클.
2. **B안 — 양하 커버 큐 단위 재설계**: 실단위 확정·탐지 ≤60초로 선행 조건 완비(변화 없음).
3. **TOS 기술 세션 준비**: `docs/tos-integration-handoff.md` 7문 + 양하 "트럭→QC 큐" 인터페이스 질문.
4. **(e) tt-handover 확장 + 프로브 P1~P9**: 60초보다 빠른 소비처가 실명으로 생길 때만(시급성 낮음).
5. **운영자 채택/기각 기록 장치**: 파일럿 Phase 1→2 기준인데 재는 장치가 없다.
6. **잡무 3건**: 평문 비밀번호(`PGPASSWORD=wp`·GitHub 원격) / 디스크 `/` 98% / 머지된 워크트리 2개
   (`kc-journal`·`ws-coverage-kc`) 정리.

## 이월된 미해소 항목 (지우지 말 것)

- **`tt-weather-live` 단발 실패**(08-21·재시도 붙일지 사용자 판단). ⚠`ops_alert` 는 MYT·저널은 KST — 1시간 함정.
- **`web/public/livemap-roadgraph.geojson` 은 매시 크론이 다시 쓴다** — 항상 modified·커밋 경로 지정.
- `dispatch_compare_shadow` 에 `(tos_ytno,t1_ts)` 인덱스 없음 · `etw_qc_ts` 죽은 컬럼 · `stage2_solver_shadow` DEADMAN 밖.
- **GPS 죽었는데 TOS 는 배차하는 트럭**(위치 나이 중앙 4.3h) — 재현율 3.2%p 상한. 위치 원천 붙일지 판단 대기.
- 화면 해석 문의 대비 2건: BoardFunnel `issued` 08-24 04:07Z 경계 ~120상자 점프(Q+트럭 편입·단차 아님) ·
  QC 카드 `active_moves`/지도 "작업 중"이 첫 픽업보다 최대 ~9분 일찍 켜짐(의도된 무해 판정).

## 사용자가 답해야 하는 것

- **`DISPATCH_MODE=active` 시점** — 코드 작업 없음·병목은 TOS 협의(변화 없음).
- **(e) 확장 시점** — churn 관측·요청 순간 정답지가 필요해질 때(B안 검증 또는 TOS 협의 자료).
- **야간 재현율을 따로 잴 것인가** — 주간 96.4%로 충분하다고 볼지, 다음 측정을 야간 포함으로 할지.
