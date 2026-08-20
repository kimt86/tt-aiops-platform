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

## 진행 상황 (2026-08-19 14:00 KST 배포)

- 커밋 `c90f114`(풀 재정의+mig0154) → `f2af6a2`(Q 상태 배차 누락 수정) → `f1d2256`(KC·측정 절). tt-api 재기동 13:59 KST.
- 첫 틱: 풀 320대(free_tos 247·inflight 63·gps_free 10) → **Q 배차 트럭 67대가 빈 트럭으로 새는 것 발견**
  (`live_workpool` 은 A+(Q∧트럭 없음)만) → `live_assigned_tt`(A+Q·전 유형) 보강 후 풀 **247대**(free_tos 182·inflight 65).
- 위치 출처: 후보의 ~75%가 `pos_hist`(장치 목록은 10분 침묵이면 지운다 — 빈 트럭 대부분이 침묵). 예상대로.
- 슬롯(n_works)은 48→58 로 약간 늘었다(Stage-1 이 트럭 수를 보는 듯 — 이번 범위 밖, 관찰만).
- **풀 재현율은 요청이 `tt_move_log` 에 착지한 뒤(완료+5분)에야 잴 수 있다** → 6시간 뒤(20:00 KST~) 측정.
- 14:29 예비값(30분·83건): 양하 100% / **적하 33.9%** → 추적 결과 두 가지를 발견·수정(14:40~14:42 KST 재배포):
  ① **적하 작업은 야드 픽업 직후 A/Q 에서 사라진다**(싣고 가는 동안 작업목록·배차목록에 없음) → 자유·배차 신호만
     보면 싣고 가는 트럭이 `free_tos` 로 오판(TT1272 12:57·TT1145 13:10 실증) → **픽업 가드**(`d7ffa56`): 마지막 픽업
     (qc DS·rtg LD comp_ts) > 마지막 자유면 싣고 있음.
  ② **`tt_move_log.dispatch_ts` 는 최종 배차만 남긴다** — 트럭이 비자마자 Q 로 배차됐다 재배정되면 첫 배차가 사라져
     "몇 분 빈 채 있었는데 풀에 없었다"는 착시(TT1272: 12:58:42 자유 → 12:58:5x 배차 → 회수 → 13:02:30 최종). Oracle
     실측: Q 상태 트럭 110대 전부 `YT_DIS_DT` 있음(=Q 가 배차 결정 시점, 대부분 양하 크레인 대기), 7분 사이 3건 재배정.
     → 요청 순간 = **자유 뒤 배차 목록에 처음 실린 틱** 으로 재정의. `live_assigned_tt` 스냅샷을 `assigned_tt_hist`
     (mig0155·`3add664`)에 남기고 측정 ⑮⑯ 추가(`2117277`). **DONE 창은 14:42 KST 부터 6시간 → 20:42 KST.**
- Q 상태 배차 트럭을 배차 중으로 보는 것(`live_assigned_tt` 보강·`f2af6a2`)은 **옳았다** — Q = 양하 크레인 대기 중.
- 14:42~15:07 첫 25분(바로잡은 잣대·349건): **82.2%** — 놓친 62건 전부 "직전 틱엔 작업 중인데 예측이 못 넣음" →
  ③ **앵커 질의가 픽업을 `status='F'`(실컨)만 봐 빈 컨테이너(M·픽업의 ~35%) 트립에 앵커가 없었다**(재현율 F 95~100% vs
     M 54~55%) → 필터 제거(`12961a2`) · **pool_ver=2**(`8c455ca`·15:09 KST 배포·경계 15:09:30 백필) · 측정을 최신 판으로 한정(`4004e23`).
- **16:26 KST 예비값(pool_ver 2 · 1h15m · 1,215건): 풀 재현율 98.4%** (free_tos 12.9 · inflight 85.5). 풀 중앙 269·최대 291.
  놓친 19건(1.6%) 전부 "자유 뒤 90초 안 등재·직전 틱엔 예측이 못 넣음". 파이프라인 120틱/2h·landing·나이 1.0·경보 0·틱 간격
  최대 63초. 기존 커버리지(추천 있었음) 배포 전 24h 28.6/26.0 → 배포 후 31.8/23.6(표본 321/369·슬롯 불변이라 잡음 범위).
  **확정은 21:15 KST(6h).**

## RESULT — 2026-08-19 21:30 KST 확정 (pool_ver 2 · 15:09 KST 배포 · 6h17m)

| DONE CRITERIA | 결과 | 판정 |
|---|---|---|
| `cargo build` · `cargo test --workspace` | 빌드 OK · **70 통과 · 0 실패** | ✅ |
| 마이그레이션 0154·0155 두 번 실행 | 오류 0 (IF NOT EXISTS) | ✅ |
| tt-api active · 유닛 동일 · 배포 mtime | active · 차이 0 · `target/release/api` 15:10:14 | ✅ |
| **풀 재현율**(트럭이 물어본 순간 직전 틱 풀에 있었나) | **98.7%** — 분모 = 자유 뒤 배차 목록에 처음 실린 사건 **5,900건**(14:10~20:27 MYT) · free_tos 13.3 · inflight 85.4 · gps_free 0.0 | ✅ ≥95 |
| 풀 크기 중앙 ≤ 300 | **중앙 274** · 최대 412(순간 스파이크) · free_tos 54 · inflight 216 · pos_hist 위치 85/틱 | ✅ |
| 파이프라인 생존 | 379틱 · **60.2/h** · 전부 landing · 목록 나이 1.1 · 45초 초과 0 · **경보 0** · 틱 간격 중앙 60.8·최대 63초 | ✅ |
| 기존 커버리지(추천 있었음) 비하락 | 배포 전 24h DS 28.0 / LD 24.7 → 배포 후 6h **33.8 / 24.5**(표본 2,136 / 2,449) | ✅ |

놓친 78건(1.3%): 76건 = 자유 뒤 90초 안 배차·직전 틱엔 예측(앵커·GPS)이 못 넣음(자유 예측 꼬리) · 2건 = 내내 풀 밖.
자유→첫 등재 중앙 **35초**(pull 구조 재확인). 배정 평균 48~58 → 77(Stage-1 이 트럭 수를 보는 듯·범위 밖).

## UNKNOWNS

- H=15분에서 실제 예측 오차로 풀 재현율이 몇 %가 나오는지 — 첫 6시간 측정이 답. 모자라면 H 20~30으로
  (정답 가정 풀 ~300~370).
- 원천 로그를 60초 틱 안에서 직접 읽는 비용 — `qc_move_log_trk_idx (trk_id, comp_ts)`는 있고 `rtg_move_log`
  쪽 인덱스는 미확인(`pg_indexes` 먼저).
- 백그라운드 파생 지연 조사가 "이 표도 원천으로"를 더 내면 이 범위에 합칠지 — 결과 나오면 판단.

---

## 파생 표 지연 전수조사 — 결과 (백그라운드 에이전트 · 2026-08-19 · `docs/cycles/2026-08-19-derived-latency-audit.md`)

**한 줄**: 라이브 판단 경로가 읽는 표 26개 중 **사건 시각을 파생 표에서 읽는 곳은 1개뿐** —
`tt_move_log.free_ts`(`livemap.rs:4711` 인플라이트 앵커). 지연 DS 180/300초 · LD 192/314초(중앙/p90) vs 원천
32~36/56~60초. **같은 유형의 두 번째 사례는 없다.**

| 표 | 원천/파생 | 만드는 주체·주기 | 사건→착지 중앙/p90 | 원천 같은 값 | 라이브 읽는 곳 | 판정 |
|---|---|---|---|---|---|---|
| **tt_move_log**(free_ts) | 파생(handover_label ⋈ qc_move_log) | tt-move-log.timer 300초 | **180/300 · 192/314** | LD `qc_move_log.comp_ts` 33/57 · DS `tos_handover_label.comp_ts` 32/56 | `livemap.rs:4711`→4763·4783·4813 | **원천으로 바꿀 것** — 이번 사이클 IN SCOPE 1 과 같은 자리 |
| qc_move_log · rtg_move_log · tos_handover_label | 원천 | :05/:25/:45 매분 | 32~36/56~60 | — | 4703·4706 · workpool 1329 | 원천 |
| live_workpool·workqueue·stow_plan·vessel_schedule·etw | 원천 | :55 매분 / 2~5분 | 착지 대기로 깸 | — | livemap 4407·2944 · workpool 272~1347 | 원천 |
| learn_* 매뷰 9개 · road_* | 파생 파라미터 | 15~30분 / 야간 | 사건 시각 아님 | — | livemap 4678·4717·4321~ · workpool 466~1270 | 괜찮음 |
| truck_pos_hist | API 30초 스냅샷 | spawn_pos_hist | 30초 양자화 | 메모리 | 5359(비교기) | 괜찮음 |

부수 실측: 인플라이트 트럭 167~178대 중 **34~43대(≈20%)** 가 원천으로는 이미 자유인데 `tt_move_log` 로는 아직
진행 중(전부 5분 미만·영구 결손 0). 그 효과는 카운트다운 값이 아니라 **"앵커 소속"**(무응답 보류 2배 연장).
미확인: 매칭 결과 변화량 · 최근 6시간 추출 공백 840~1,020초 원인 · `data_freshness.STOWPLAN` 12일 정지(표 자체는
신선) · `learn_qc_move_time` 야간 1회 의도 여부.

## 이번 사이클에서 나온 범위 밖 발견 (다음 세션 후보)

- **적하 작업은 야드 픽업 직후 JOB_ORDER_LIST 의 A/Q 에서 사라진다**(싣고 안벽으로 가는 동안 작업목록·배차목록에
  없음). TOS 의 적하 생애주기(상태가 무엇으로 가나·COMPDATE 가 야드에서 찍히나)는 **TOS 세션 질문**으로 추가.
- **Q 상태 + 트럭 = 배차 결정 완료**(`YT_DIS_DT` 있음·대부분 양하 크레인 대기). 7분 사이 Q 행 3/31건이 **다른 트럭으로
  재배정** — 재배정 빈도·사유는 미측정. `tt_move_log.dispatch_ts` 는 최종 배차만 남기므로 **"요청 시각" 분석에 쓰면 안 된다**
  (이제 `assigned_tt_hist` 로 잰다). 측정 ①·⑭ 절은 이 한계를 가진 옛 잣대 — 읽을 때 주의.
- **빈 컨테이너(M) 트립이 픽업의 ~35%** — `status='F'` 로 거르는 곳이 또 있는지(`qc_moves`·KPI 등) 점검 가치.
- 장치 목록 프룬 `LOST_AFTER_S=600`: 후보의 ~1/3 이 `truck_pos_hist` 로 위치를 보충받는다(정지 트럭). 틱당 질의 1개라
  비용은 작지만 메모리에 마지막 픽스를 남기는 게 더 단순.
- 풀 크기 순간 최대 412 — 교대 직후 등 스파이크. 슬롯 단계(다음 세션)에서 같이 볼 것.
- H=15분은 그대로(재현율 98.7%로 충분). 남은 1.3%는 자유 예측의 꼬리 — 모델 개선 영역.

## 이월된 미해소 항목 (지우지 말 것)

- **`tt-weather-live` 단발 실패**(2026-08-21 02:51:57 KST · `Error: tomorrow.io fetch failed (curl status / key / quota)`
  · exit 1 · OnFailure 훅이 `ops_alert` warn 1건). 앞뒤 틱은 성공·`weather_1min` 결손 0(3분 주기가 1분 격자를 11행씩
  겹쳐 넣어 다음 틱이 메움)·이후 지금까지 OK. **외부 원인**이고 후보 풀 변경과 무관. 재시도(`Restart=on-failure`)를
  붙일지는 사용자 판단 — 지금은 결손이 없었지만 다음엔 한 틱이 빌 수 있다.
  ⚠교훈: `ops_alert` 를 MYT 로 출력하고 저널(KST)을 같은 숫자로 뒤지면 **1시간 어긋난 창**을 본다.

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
