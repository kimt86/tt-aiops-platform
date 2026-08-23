# HANDOFF — 다음 사이클

마지막 갱신 2026-08-24. 앞선 사이클: **후보 풀을 pull 구조로 재정의**(pull 1/2·머지 `4886d7a`).
요청 순간 재현율 **20% → 96.6%**. 슬롯·배정 순서는 손대지 않았다(pull 2/2 몫).

> 상시 사실·기준선·함정은 `~/.claude/notes/tt-aiops-platform.md`(150줄·상한). 이 파일은 **다음에 할 일**만.

---

## DONE CRITERIA (작업을 끝냈다고 말하기 전에 전부 돌릴 것)

```bash
cargo build --release -p tt-api && cargo build --release -p tt-extractor
cargo test --workspace                      # 70 통과 · 실패 0 (tt-api 단독 36)
systemctl --user is-active tt-api           # active
# 유닛 드리프트 — ⚠tt-scenario-* 는 제외한다(별도 저장소가 배포 주체)
for f in deploy/systemd/*.service deploy/systemd/*.timer; do b=$(basename "$f");
  case "$b" in tt-scenario-*) continue;; esac; i="$HOME/.config/systemd/user/$b"
  [ -f "$i" ] || { echo "미설치: $b"; continue; }; diff -q "$f" "$i" >/dev/null || echo "차이: $b"; done
curl -s -o /dev/null -w '%{http_code}\n' localhost:8080/kc/dispatch/            # 200
```
```bash
# 풀 재현율 + 파이프라인 (⑮절이 헤드라인 · pool_ver 로 갈린다)
psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f scripts/pull_model_coverage.sql
```
```sql
SELECT wake_src, count(*) 틱, round(avg(workpool_age_s),1) 나이, count(*) FILTER (WHERE workpool_age_s>45) 초과45
  FROM stage2_solver_shadow WHERE ts > now()-interval '2 hours' GROUP BY 1;   -- 55~62틱/시간
SELECT source, subject, severity, occurrences FROM ops_alert WHERE last_ts > now()-interval '3 hours';
```

---

## 지금 참인 것 (전에는 아니었던 것)

- **후보 풀은 GPS 상태 라벨로 정해지지 않는다.** ①원천 드랍 로그(적하 `qc_move_log`·양하 `tos_handover_label`)에
  자유가 찍혔고 그 뒤 픽업·새 배차·배차목록 등재가 없으면 **GPS 와 무관하게** 풀에 넣는다 ②배차 중 트럭은 예측
  자유 ≤`POOL_FREE_HORIZON_S`(900초)만 ③위치는 신선 GPS > 낡은 픽스 > `truck_pos_hist`(≤`POS_MAX_AGE_S` 3시간).
  `SILENT_HOLD_S`("20분 침묵=퇴근")는 폐기됐다.
- **측정 도구가 생겼다.** 매 틱 풀 전체 = `stage2_pool_truck_shadow`(mig0154·판별자 `pool_ver`·현행 **6**),
  배차 명단 스냅샷 = `assigned_tt_hist`(mig0155). 둘 다 3일 보관·`db.rs` RETENTION 등록.
- **요청 순간의 정의**: `assigned_tt_hist` 에서 **자유 뒤 처음 실린 틱**. `tt_move_log.dispatch_ts` 는 최종 배차만
  남아 못 쓴다.
- **화면**: `/api/livemap/positions` 의 `in_pool` 이 매처의 실제 풀 소속을 알려준다(발행이 낡으면 `None`).
  TtPage 는 이제 라벨이 아니라 이 값으로 후보를 센다.
- **낡은 위치 계기**: 풀이 쓴 위치의 30분/1시간 초과 비율을 매 틱 로그, 30분 초과가 절반을 넘으면 경보
  (`stage2_pool`/`stale_positions`) — GPS 피드 사망의 첫 신호.

## 일부러 안 한 것

- **슬롯 수·배정 순서·간선 상한 1,800초** — pull 2/2. 풀 크기가 슬롯을 직접 정하는 커플링(`truck_n = vehicles.len()`)은
  **사용자 결정으로 그대로 뒀다**(2026-08-21).
- **자유 시각 예측 모델 개선** — 남은 재현율 3.4% 의 93%가 이 꼬리인데, 이번엔 기존 값을 그대로 썼다.
- **`classify_tt`/`latched_*` 수정** — 새 풀 규칙이 라벨을 우회하므로 건드리지 않았다.
- 2차 리뷰 지적 중 **거절 3건**: 어휘 유사(`free_gps`/`gps_free`·COMMENT 로 구분됨) · 측정이 매처와 생사를 공유
  (매처가 멈추면 분자·분모가 함께 사라져 재현율이 안 떨어진다) · `POOL_FREE_HORIZON_S` env 가 `pool_ver` 를 안 바꿈
  (주석에 경고만 넣음).

## 이번 사이클에서 나온 범위 밖 발견

- **GPS 단말이 죽었는데 TOS 는 계속 배차하는 트럭** — 위치 나이 중앙 4.3h·최대 34h. 상한 3시간이면 요청의
  **1.3~1.8%** 가 재현율에서 빠진다(무제한 대비 3.2%p). 위치가 없어 쓸모 있는 추천을 못 만든다 — 별도로 세거나
  위치 원천(마지막 알려진 블록·TOS 위치 컬럼)을 붙일지 판단할 것.
- **적하 커버리지가 사이클 시작 전보다 낮다**(24h 전 LD 23.9% → ver5 19.5%). 풀이 줄며 슬롯도 줄어든 결과 — pull 2/2.
- **MI/MO(야드 내부 이송)는 픽업 가드의 사각지대** — `picked` 가 적하 RTG·양하 QC 만 본다. 실측 피해 0.06%
  (`live_assigned_tt` 가 우연히 덮는 중)라 두었다.
- **프룬이 아직 한 번도 안 지웠다** — 두 표 최초 행이 08-19, 보관 3일이라 08-22 경 경계에 닿았다.
  **다음 세션에서 최초 행 시각으로 실제 삭제 여부를 확인할 것**(`db.rs` RETENTION 이 no-op 이면 경보).

## 다음 후보 (한 줄 근거)

1. **pull 2/2 — 슬롯·배정 순서** — 풀은 고쳤지만 "몇 개를 누구에게"는 그대로다. 자유까지 시간 짧은 순 배정 +
   출항 페이스를 캡이 아니라 순위로. 적하 커버리지 하락도 여기서 회수한다.
2. **TOS 기술 세션** — `docs/tos-integration-handoff.md` 7개 질문. 기술 쪽 유일한 크리티컬 패스이고,
   이번에 물을 것이 늘었다: **적하 작업이 야드 픽업 직후 A/Q 에서 사라지는 이유**(생애주기·COMPDATE 시점).
3. **자유 시각 예측의 꼬리** — 남은 미스의 93%. 앵커(`learn_cycle_remaining`)를 배선으로 승격할지.
4. **운영자 채택/기각 기록 장치** — 파일럿 Phase 1→2 통과 기준인데 재는 장치가 없다.
5. **`deadman/road_route_eval` 래치 경보** — 08-11 이후 아무도 안 봤다.
6. **평문 비밀번호** — `scripts/*.sh` 의 `PGPASSWORD=wp`. GitHub 원격이 있다.
7. **디스크** — `/` 98%·여유 24GiB. root 권한자 몫.
8. **다 머지된 워크트리 2개 정리**(`kc-journal`·`ws-coverage-kc`) — 미머지 0·깨끗.

## 이월된 미해소 항목 (지우지 말 것)

- **`tt-weather-live` 단발 실패**(08-21 02:51 KST·Tomorrow.io 응답 실패·결손 0). 재시도(`Restart=on-failure`)를
  붙일지는 사용자 판단. ⚠교훈: `ops_alert` 를 MYT 로 출력하고 저널(KST)을 같은 숫자로 뒤지면 1시간 어긋난 창을 본다.
- **`web/public/livemap-roadgraph.geojson` 은 매시 크론이 다시 쓴다** — 항상 modified. 커밋에 딸려 들어가지 않게
  경로를 지정할 것.
- `dispatch_compare_shadow` 에 `(tos_ytno,t1_ts)` 인덱스 없음 · `etw_qc_ts` 죽은 컬럼 · `stage2_solver_shadow` DEADMAN 밖.

## 사용자가 답해야 하는 것

- **`DISPATCH_MODE=active` 를 언제 켤 것인가?** 코드 작업은 없다. TOS 소비 채널이 없는 지금은 얻는 것이 없다.
- **적하 커버리지 하락(−4.4%p)을 pull 2/2 에서 되돌릴 것인가**, 아니면 풀 축소가 옳으니 그대로 둘 것인가.
