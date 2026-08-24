# HANDOFF — 현재 사이클 (2026-08-24 확정)

## GOAL

TOS가 배차한 상자를 우리 시스템이 유형 무관 **≤120초** 안에 "배차됨"으로 인식한다 —
Oracle 무접촉, Rust 착지 수정만으로.

배경: 추출기가 (DS/LD ∧ 'Q' ∧ 트럭 있음) 행을 어느 갈래에도 안 싣고 버린다
(`workpool.rs` 착지 match·실측 순간 116행). 그래서 `tos_assigned`가 픽업 후 'A' 전환까지
거짓이고, 양하 배차 탐지가 p50 544초다(적하 52초). 행은 이미 매 틱 페이로드에 실려 온다 —
SQL·왕복·페이로드 변경 0. (A안=pool_tick에 JOB_ORDER_HISTORY 갈래 접기는 2026-08-24
멀티에이전트 리뷰가 기각 — BLOCKING 3건. 대체 경로의 1단계가 이 사이클.)

## IN SCOPE

1. **착지 갈래 추가** — `crates/extractor/src/workpool.rs` 착지 match에
   (DS/LD ∧ 'Q' ∧ ytno 있음) → `live_workpool` 갈래. 실제 `ytno`·`yt_dis_ts` 보존.
   SQL 세 파일(`pool_tick.sql`/`workpool.sql`/`workqueue.sql`) **무변경**.
   split 킬스위치 경로는 착지 코드 공유라 자동 동일.
2. **소비처 전수 감사** — `live_workpool` 읽는 곳 전부(extractor 1·api 12).
   "Q행 = 트럭 없음" 가정이 깨지는 곳을 고치거나 무해 판정 기록.
   특히 `tos_assigned` 집계(workpool.rs:1324)·비교기 T1·재매칭 15분 창·
   `live_candidate` 수요 집계 이중 계상 여부.
3. **판별자 규율** — `pool_ver` 6→7(멱등 마이그레이션+COMMENT 경계),
   `dispatch_compare_shadow` 경계 시각 COMMENT.
4. **재측정** — 배포 후 ≥1시간 창 탐지 지연(비교기 first_seen−t1_ts).
   이 수치가 (e) handover 확장이 갚을 잔여분의 정의.
5. **이월 확인**(코드 0줄) — `assigned_tt_hist`·`stage2_pool_truck_shadow` 3일 프룬 동작
   min(ts) 확인. 깨져 있으면 보고만.
6. **KC 갱신** — `live_workpool` 모집단 서술 현행화(추출 문서).

## OUT OF SCOPE

(e) tt-handover 질의 확장 · Oracle 프로브 P1~P9 · B안(양하 커버 큐 단위 재설계) ·
자기추천 180초 값 변경 · pull 2/2(슬롯·배정 순서) · `classify_tt`/`latched_*`.

## DONE CRITERIA

```bash
cargo build --release -p tt-extractor && cargo build --release -p tt-api
cargo test --workspace          # 70 통과·실패 0
# 배포 후 다음 틱:
psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -c "SELECT jobtype, count(*) FROM live_workpool
  WHERE jobstatus='Q' AND ytno IS NOT NULL AND ytno<>'' GROUP BY 1"
#  → 0이 아니어야 함 (실측 순간값 DS ~77 / LD ~39)
# ≥1h 후: DS 탐지 p50 544초 → ≤120초 (dispatch_compare_shadow first_seen−t1_ts·경계 이후만)
# 회귀 없음: wake_src 틱 55~62/h 유지 · ops_alert 신규 crit 0
```

## UNKNOWNS

- 소비처 중 실제 수정 필요한 곳 수 — 감사가 곧 작업, 빌드 중 확정.
- ≤120초는 예측값 — 안 떨어지면 그 잔여가 (e)의 근거이지 이 세션의 실패가 아님.
- Q+트럭 행의 트윈·`live_candidate` 상호작용 세부 — 빌드 중 확정.

## 수행 기록 (2026-08-24 빌드 중)

- **IN SCOPE 5 완료**: 3일 프룬 실동작 확인 — `stage2_pool_truck_shadow` min(ts) 08-21 03:49 ·
  `assigned_tt_hist` 08-21 04:04 = 정확히 3일 경계 부근. RETENTION 정상, 조치 불요.
- **재측정 분모 규칙**(1차 리뷰 SF3 반영): 경계는 `t1_ts`(배차 시각)로 걸고,
  `(qc,queuename,tos_ytno,t1_ts)` 단위 `min(ts)` 로 접는다. `ts` 로 거르면 배포 순간
  미픽업 재고(~116행)가 544초급으로 p50 을 오염시킨다. mig0157 COMMENT 에도 기록.
- **다음 정리 이월**(리뷰 C6): 착지 진리표가 인라인 match 라 유닛 테스트가 없다 —
  분류 함수 추출 + 돌연변이 검증은 리팩터링이라 이번 범위 밖.

## 참고 (전 사이클에서 이어짐)

- `web/public/livemap-roadgraph.geojson`은 매시 크론이 다시 쓴다 — 항상 modified·커밋 경로 지정.
- 리뷰 전문·프로브 P1~P9·(e)안 조건 11개: 워크플로 출력
  (`.claude/projects/.../workflows/wf_74760d45-cf6` 및 메모리 `reference_job_order_history.md`).
