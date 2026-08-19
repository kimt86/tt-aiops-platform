# HANDOFF — 이번 사이클: 후보 풀 재정의 (pull 구조 1/2)

마지막 갱신 2026-08-19. 앞선 사이클(측정)의 결과는 `docs/cycles/2026-08-19-pull-coverage-findings.md` 로 옮겼다.
요지: 현장은 **차량이 작업을 고르는 pull 구조**. 트럭이 물어본 순간 우리에게 답이 있던 비율(커버리지)이
**19.9% / 17.8%**. 놓친 건의 90%는 "풀에 없어서", 그중 대부분이 실제로는 이미 빈 트럭(GPS 라벨·침묵 탓).
남은 10%는 슬롯 부족. 사용자 결정: **둘로 자른다** — 이번 = 풀, 다음 = 슬롯·배정 순서.

> 상시 사실·기준선·함정은 `~/.claude/notes/tt-aiops-platform.md`(150줄·상한). 이 파일은 **이번 사이클의 정의**만.

---

## GOAL

트럭이 배차를 요청하는 순간, 그 트럭이 **직전 틱의 우리 후보 풀에 들어 있던 비율(풀 재현율)** 을 지금의
~20%(A 층만)에서 **95% 이상**으로 올린다 — 그림자, 배차 로직·슬롯은 그대로.

## IN SCOPE

1. **후보 풀 조립 재정의**(`crates/api/src/livemap.rs` 4729~ 근방 · 후보 트럭 `vehicles` 만드는 블록):
   - **명단** = TOS 활동(배차 또는 자유) ≤3h ∪ GPS 출현 ≤30분인 트럭. `SILENT_HOLD_S=1200`("20분 침묵=퇴근")
     가정 폐기 — 30분+ 침묵 트럭이 요청의 8.8%를 낸다.
   - **무조건 포함** = 원천 로그(`qc_move_log`·`rtg_move_log`의 `trk_id`+`comp_ts`)에 드랍 완료가 찍혔고 그 뒤
     `live_workpool`에 새 `ytno` 배차가 없는 트럭. **GPS 상태 라벨과 무관.** 시각은 원천을 직접 읽는다
     (조립본 `tt_move_log.free_ts`는 185초 늦다 — 원천은 33초).
   - **예측 포함** = 예측 자유까지 시간 ≤ H(초기 **15분**)인 트럭. 예측 = 기존 무브로그 픽업 앵커(`inflight`)와
     기존 학습값(`st_free`/`fi_bias`) 그대로. 그 밖("명백히 작업 중")은 제외.
   - 위치 = 마지막 GPS(정지한 트럭은 그 자리). 자유까지 시간 = 빈 채 0 / 예측값.
     `soon_idle_held`의 "짐 실음 + 드랍 120m 안" 조건은 위 규칙에 흡수(별도 가지 삭제).
2. **측정용 기록**: 틱마다 풀에 든 트럭을 표로 남긴다 — 새 마이그레이션 1개(`ytno, ts, 포함 사유
   [free_signal|predicted|idle_gps 등], free_in_s, 위치 출처, pool_ver`) · 3일 보관 프룬 · 판별자 `pool_ver`.
   지금은 미배정 후보가 어디에도 안 남아 재현율을 못 잰다.
3. `scripts/pull_model_coverage.sql`에 **풀 재현율** 절 추가(요청 순간 직전 틱 풀에 있었나 · 사유별 분해 ·
   풀 크기 분포).
4. KC 배차 문서에 후보 풀 정의 변경을 쉬운 말로 반영(`kc/dispatch/` 해당 페이지).
5. HANDOFF 결과 절 · 노트 기준선 갱신.

## OUT OF SCOPE (다음 세션 또는 별도)

- **Stage-1 슬롯 수 · 배정 순서(자유까지 시간 짧은 순) · 간선 상한 1,800초 폴백 · 출항 페이스 풀** — 다음 세션.
- 자유 시각 예측 모델 개선(`learn_cycle_remaining` 배선 승격 포함) — 기존 값 그대로.
- `classify_tt` / `latched_*` 수정 — 새 풀 규칙이 라벨을 우회하므로 이번엔 안 건드림.
- `tt_move_log` 조립 배치 자체 고치기 — 백그라운드 **파생 표 지연 전수조사** 결과에 따라 별도.
- 실배차 채널 · `DISPATCH_MODE` · TOS 쓰기.

## DONE CRITERIA

```bash
cargo build --release -p tt-api && cargo test --workspace         # 70+ 통과 · 0 실패
psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -f db/migrations/<새 파일>.sql   # 두 번 실행해도 안전
cp deploy/systemd/tt-api.service ~/.config/systemd/user/ 2>/dev/null; systemctl --user daemon-reload
systemctl --user restart tt-api && systemctl --user is-active tt-api      # active · 배포 확인은 target/release/api mtime
```
- 배포 후 **최소 6시간** 쌓인 뒤 `psql ... -f scripts/pull_model_coverage.sql`의 새 절:
  분모 = 그 창의 트럭 요청(DS/LD 왕복 1건). **직전 틱 풀에 있었음 ≥ 95%**. 사유별(무조건/예측/명단 밖) 분해 보고.
- **풀 크기 중앙 ≤ 300대**(정답 가정 H=15에서 ~260 — 크게 넘으면 "명백히 작업 중"을 못 빼는 것).
- 파이프라인 생존: 솔버 틱 55~62/시간 · `wake_src=landing` · 경보 0(디스크 제외) · 매칭 틱 소요 60초 미만.
- 기존 커버리지(추천 있었음)는 **떨어지지 않음**(≥ 19.9 / 17.8%) — 슬롯 그대로라 오를 필요는 없음.

```sql
-- 파이프라인 생존 (틱 55~62/시간 · 경보 0)
SELECT wake_src, count(*) AS 틱, round(avg(workpool_age_s),1) AS 평균, count(*) FILTER (WHERE workpool_age_s > 45) AS 초과45
  FROM stage2_solver_shadow WHERE ts > now()-interval '2 hours' GROUP BY 1;
SELECT source, subject, severity, occurrences FROM ops_alert WHERE last_ts > now()-interval '3 hours';
```

## UNKNOWNS

- H=15분에서 실제 예측 오차로 풀 재현율이 몇 %가 나오는지 — 첫 6시간 측정이 답. 모자라면 H 20~30으로
  (정답 가정 풀 ~300~370).
- 원천 로그를 60초 틱 안에서 직접 읽는 비용 — `qc_move_log_trk_idx (trk_id, comp_ts)`는 있고 `rtg_move_log`
  쪽 인덱스는 미확인(`pg_indexes` 먼저).
- 백그라운드 파생 지연 조사가 "이 표도 원천으로"를 더 내면 이 범위에 합칠지 — 결과 나오면 판단.

---

## 진행 중 조사 (백그라운드 에이전트 · 2026-08-19)

- **파생 표 지연 전수조사** — `tt_move_log.free_ts` 가 원천과 같은 값인데 조립 배치 탓에 185초 늦게 보였다
  (원천 33초). 라이브가 읽는 표마다 "원천 대비 지연"을 잰다. 결과:
  `/home/tkadmin/.claude/jobs/d9618bb4/tmp/derived_latency_audit.md`. 끝나면 여기 표로 옮길 것.

## 이월된 미해소 항목 (지우지 말 것)

- **미해소 래치 경보 `deadman/road_route_eval`** — 마지막 08-11 08:34. 아직 안 봤다.
- **`disk/filesystem` crit** — `/` 98%·여유 24GiB. root 권한자 몫.
- **`web/public/livemap-roadgraph.geojson` 은 매시 크론이 다시 쓴다** — 항상 modified. 커밋에 딸려 들어가지
  않게 경로를 지정할 것.
- 다 머지된 워크트리 2개(`kc-journal`·`ws-coverage-kc`) — 미머지 0·깨끗. 정리 여부는 사용자 결정.
- 양하 첫 추천의 12%는 TOS 가 우리보다 90초+ 먼저 배차한 상자였다(적하 2건) — 원인 미조사.
- `dispatch_compare_shadow` 에 `(tos_ytno, t1_ts)` 인덱스 없음 · `etw_qc_ts` 죽은 컬럼 · `stage2_solver_shadow`
  DEADMAN 밖.

## 사용자가 답해야 하는 것

- (다음 세션) 슬롯·배정 순서 — 출항 페이스를 캡이 아니라 순위로 쓰는 것, 간선 상한 폴백.
- **`DISPATCH_MODE=active`** 는 TOS 소비 채널이 없는 지금은 의미 없음(변동 없음).
