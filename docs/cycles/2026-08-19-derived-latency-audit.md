# 라이브 판단 경로의 파생 표 지연 전수 조사

- 조사일 2026-08-19 (MYT 12:30~12:45 표본 채취) · 코드 기준 `main` a590e5d(작업 중 HEAD) · **코드·DB·유닛 변경 없음, 조회만**
- 범위: `spawn_stage2_shadow`(매처·후보 트럭 풀), `workpool::stage2_work_candidates`/`build_workpool`(작업 후보·work_eta·마감), `spawn_dispatch_compare`(비교기·60초), 이들이 부르는 `spawn_assignment_refresh`(30초)·`spawn_selfcal_refresh`(15분)·`roadgraph::RouteCost::load`. 스크립트·원샷·KPI·프론트 엔드포인트(`positions`, `/api/workpool`, `cycles.rs`)는 제외.

## 1. 한 줄 결론

라이브 판단 경로가 읽는 표 26개 중 **사건 시각을 파생 표에서 읽는 곳은 1개(`tt_move_log.free_ts`, `livemap.rs:4711` 인플라이트 앵커)뿐**이고, 그 지연은 사건 후 **DS 중앙 180초·p90 300초 / LD 중앙 192초·p90 314초**(원천 `tos_handover_label`·`qc_move_log`는 32~36초 / 56~60초)다. 그 밖의 파생 표 9개는 전부 **다일 창의 통계 파라미터**(매뷰·집계표, 15~30분·야간 갱신)라 사건 시각으로 쓰이지 않고, 비교기가 읽는 `truck_pos_hist`는 API 자신의 30초 GPS 스냅샷이다. **같은 유형의 두 번째 사례는 없다.**

## 2. 표

지연 열은 "사건 시각(comp_ts/free_ts) → 그 행이 우리 Postgres 에 착지(captured_at)"의 최근 48시간 분포(중앙/p90). 원천 열은 같은 사건에 대한 원천 표의 같은 값. 파라미터 표는 사건 시각을 담지 않으므로 지연 대신 갱신 주기·마지막 갱신을 적었다.

| 표 | 원천/파생 | 만드는 주체·주기 | 사건→착지 지연 중앙/p90 | 원천의 같은 값 | 라이브에서 읽는 곳 | 판정 |
|---|---|---|---|---|---|---|
| **tt_move_log** (`free_ts`) | **파생** (tos_handover_label ⋈ qc_move_log) | `tt-move-log.timer` 300초 자유주행 → `scripts/populate_tt_move_log.sql`(2일 창 재스캔·ON CONFLICT DO NOTHING) | **DS 180 / 300 s (p99 343) · LD 192 / 314 s (p99 350)** · n=23,285/16,826 | LD 자유 = `qc_move_log.comp_ts`(LD·F) 33 / 57 s · DS 자유 = `tos_handover_label.comp_ts`(DS) 32 / 56 s (= `rtg_move_log.comp_ts` DS·F 33 / 57 s, 값 차이 0 s) | `livemap.rs:4711` (`freed` CTE, `fr.f < l.pk` 로 "아직 일하는 중" 판정) → 4763-4764(무응답 트럭 보류 2배 연장)·4783(앵커 카운트다운)·4813(DS 보이는 트럭 duration) | **원천으로 바꿀 것** — 같은 값·같은 조인 키(ytno=trk_id)로 중앙 −147~−159초, p90 −243~−257초. 의미 차이 없음(아래 3.1). 단 판단에 미치는 효과는 "앵커 소속"이며 카운트다운 값은 대부분 이미 0 |
| qc_move_log | 원천 (Oracle) | `tt-qc-moves.timer` 매분 :05 → `extractor qc-moves` | DS 36 / 60 s · LD 33 / 57 s | — | `livemap.rs:4703`(DS 픽업 앵커) · `workpool.rs:1329`(크레인이 이미 처리한 DS 지시 제외) | 원천. 60초 폴링의 고유 지연 |
| rtg_move_log | 원천 (Oracle) | `tt-rtg-moves.timer` 매분 :25 | DS 33 / 57 s · LD 32 / 57 s | — | `livemap.rs:4706`(LD 픽업 앵커) | 원천 |
| tos_handover_label | 원천 (Oracle JOB_ORDER_HISTORY C) | `tt-handover.timer` 매분 :45 | DS 32 / 56 s · LD 33 / 57 s | — | 라이브 판단이 직접 읽지 않음(tt_move_log 의 재료) | 원천 — DS 자유의 직접 출처로 쓸 수 있음 |
| live_workpool / live_workqueue / live_candidate / live_assigned_tt | 원천 (Oracle, 한 트랜잭션에 함께 착지) | `tt-workpool.timer` 매분 :55 → `extractor workpool` (as_of 는 조회 **전** 시각·조회 4~20초) | 매처가 착지를 기다려 깬다(`wait_for_workpool_landing`) | — | `livemap.rs:4407, 2944-2947` · `workpool.rs:272-290, 458, 735, 1190, 1325, 1347` | 원천 |
| data_freshness (WORKPOOL) | 원천 (추출기 자체 기록) | 추출 성공 시 | — | — | `livemap.rs:4406-4408` | 원천 |
| live_vessel_schedule | 원천 (Oracle) | `tt-vessel-schedule.timer` 5분(:35) | 표 나이 4.5분 관측 | — | `workpool.rs:436`(ESTDEP/ESTWKC → 마감) | 원천 |
| live_stow_plan | 원천 (Oracle) | `tt-stowplan.timer` 2분(:15) | 표 나이 1.8분 관측 | — | `workpool.rs:1342`(planseq 순번) | 원천 |
| tos_etw_cntr | 원천 (Azure ETW 게이트웨이) | workpool 틱 내 | — | — | `workpool.rs:283`(표시 전용, 08-18 확인) | 원천·판단 미사용 |
| learn_cycle_remaining | 파생 (tt_move_log 14일 집계) | `tt-learn-cycle-remaining.timer` 30분 → `populate_learn_cycle_remaining.sql` | 파라미터(remaining_p50=DS 750/LD 648 s) · 마지막 04:22Z | — | `livemap.rs:4717` | 괜찮음 — 사건 시각 아님, 5분 원료 지연은 14일 중앙값에 무의미 |
| learn_dispatch_lead | 파생 매뷰 (qc_move_log 7일 + stage2_match_shadow 7일) | `spawn_dispatch_pred_logger` 120초 틱 × 10 = 20분 REFRESH (`workpool.rs:1084`) | 파라미터(realized_lead_s·extra_s) | — | `livemap.rs:4678` · `workpool.rs:1181` | 괜찮음 — 파라미터 |
| learn_qc_wall_cadence · learn_qc_slot_step | 파생 매뷰 (qc_move_log 3일 / dispatch_pred_sample 3일) | 같은 20분 REFRESH (`workpool.rs:1047, 1050`) · 마지막 04:30Z | 파라미터(구역 무브 리듬) | — | `workpool.rs:481, 1256, 1270` | 괜찮음 |
| learn_qc_move_time | 파생 표 (qc_move_log 3일) | `tt-nightly.timer` 01:30 → `extractor run --kpi all` (마지막 as_of 08-18 16:31Z) | 파라미터 · **하루 1회** | — | `workpool.rs:466, 1248` (wall_cadence 가 있으면 덮어씀) | 괜찮음(폴백 층) — 갱신이 야간 1회인 것은 별건으로 미확인 |
| learn_free_in_bias · learn_free_in_stationary · learn_soon_idle_gate | 파생 매뷰 (free_in_sample / tt_cycle_v2+truck_pos_hist / tt_soon_idle_pred) | `spawn_selfcal_refresh` 900초 REFRESH → 메모리 `free_in_bias`·`stationary_free`·게이트 | 파라미터(초 단위 중앙값) | — | `livemap.rs:4326-4346` → 매처 4808-4830 | 괜찮음 — 파라미터 |
| learn_topos_point | 파생 표 (GPS 학습) | 기동 시 로드 + `spawn_learn_persist` 5분 | 파라미터(블록·크레인 좌표) | — | `livemap.rs:1953` → centroids | 괜찮음 |
| road_node · road_edge · road_route_eval | 파생 표 (도로망 추론·평가) | `spawn_roadgraph_eval` 10분 등 | 파라미터(경로 비용) | — | `roadgraph.rs:67-73, 414` (매 틱 `RouteCost::load`) | 괜찮음 |
| stage2_match_shadow | 자기 출력 | 매처가 틱 끝에 기록 | 직전 틱 결과(안티스래시·자기추천 TTL) | — | `livemap.rs:4655, 4665` | 해당 없음(자기 상태) |
| truck_pos_hist | API 자체 스냅샷 (웹소켓 GPS → 30초) | `spawn_pos_hist` 30초, ts=기록 시각 | 30초 양자화 + 마지막 GPS 나이(≤120초, 미저장) | 원천은 메모리 `lm.devices`(=웹소켓) | `livemap.rs:5359` (비교기: T1=배차 시각의 트럭 위치 복원) | 괜찮음 — 비교기(계기)만 읽고 배차 판단은 메모리 GPS 를 씀. T1 나이 중앙 11.5분 대비 30초 양자화는 작음 |

## 3. 표별 상세

### 3.1 tt_move_log — 유일한 "사건 시각을 파생 표에서 읽는" 곳

**어떻게 만들어지나.** `deploy/systemd/tt-move-log.timer`(OnUnitActiveSec=300s, 자유주행) → `scripts/populate_tt_move_log.sql -v days=2`. `tos_handover_label t ⋈ qc_move_log q`(contno·ytno=trk_id·jobtype·q.comp_ts ∈ [t.dis_ts, +3h)). `free_ts` = LD 이면 `q.comp_ts`(QC 드랍), DS 이면 `t.comp_ts`(JOB_ORDER_HISTORY C = 야드 드랍). 즉 **값 자체는 원천 컬럼을 복사한 것**이다.

**착지 지연 실측(48h, 사건 30분 이상 지난 것만).**

```sql
BEGIN; SET LOCAL statement_timeout='60s';
SELECT 'tt_move_log free' AS what, jobtype, count(*) n,
  percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM captured_at-free_ts))::int p50,
  percentile_cont(0.9) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM captured_at-free_ts))::int p90,
  percentile_cont(0.99) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM captured_at-free_ts))::int p99
FROM tt_move_log WHERE free_ts > now()-interval '48 hours' AND free_ts < now()-interval '30 minutes' AND captured_at IS NOT NULL
GROUP BY jobtype
UNION ALL
SELECT 'qc_move_log comp', jobtype, count(*),
  percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM captured_at-comp_ts))::int,
  percentile_cont(0.9) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM captured_at-comp_ts))::int,
  percentile_cont(0.99) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM captured_at-comp_ts))::int
FROM qc_move_log WHERE comp_ts > now()-interval '48 hours' AND comp_ts < now()-interval '30 minutes' AND status='F' AND jobtype IN ('DS','LD')
GROUP BY jobtype
UNION ALL
SELECT 'rtg_move_log comp', jobtype, count(*), /* 동일 식 */ percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM captured_at-comp_ts))::int, percentile_cont(0.9) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM captured_at-comp_ts))::int, percentile_cont(0.99) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM captured_at-comp_ts))::int
FROM rtg_move_log WHERE comp_ts > now()-interval '48 hours' AND comp_ts < now()-interval '30 minutes' AND status='F' AND jobtype IN ('DS','LD')
GROUP BY jobtype
UNION ALL
SELECT 'tos_handover_label comp', jobtype, count(*), percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM captured_at-comp_ts))::int, percentile_cont(0.9) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM captured_at-comp_ts))::int, percentile_cont(0.99) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM captured_at-comp_ts))::int
FROM tos_handover_label WHERE comp_ts > now()-interval '48 hours' AND comp_ts < now()-interval '30 minutes' AND jobtype IN ('DS','LD')
GROUP BY jobtype ORDER BY 1,2;
ROLLBACK;
```

```
          what           | jobtype |   n   | p50 | p90 | p99
 qc_move_log comp        | DS      | 16974 |  36 |  60 |  68
 qc_move_log comp        | LD      | 10770 |  33 |  57 |  62
 rtg_move_log comp       | DS      | 16971 |  33 |  57 |  62
 rtg_move_log comp       | LD      | 10817 |  32 |  57 |  62
 tos_handover_label comp | DS      | 23576 |  32 |  56 |  62
 tos_handover_label comp | LD      | 16959 |  33 |  57 |  62
 tt_move_log free        | DS      | 23285 | 180 | 300 | 343
 tt_move_log free        | LD      | 16826 | 192 | 314 | 350
```

분모: 각 행은 "해당 표에서 최근 48시간에 사건이 있고 사건이 30분 이상 지난 행 전부"다. 원천 3표의 분포(중앙 32~36·p90 56~60)는 60초 폴링의 균등 대기 + 조회 시간이고, tt_move_log 는 그 위에 300초 타이머의 균등 대기가 얹힌 모양이다(p99 ≈ 원천 p99 + 300).

**같은 사건끼리 짝지은 지연 차(파생 착지 − 원천 착지).**

```sql
BEGIN; SET LOCAL statement_timeout='60s';
WITH t AS (SELECT ytno, contno, jobtype, free_ts, captured_at FROM tt_move_log
           WHERE free_ts > now()-interval '48 hours' AND free_ts < now()-interval '30 minutes' AND captured_at IS NOT NULL),
ld AS (SELECT t.captured_at d_cap, q.captured_at s_cap FROM t JOIN qc_move_log q
         ON q.contno=t.contno AND q.jobtype=t.jobtype AND q.trk_id=t.ytno AND q.comp_ts=t.free_ts WHERE t.jobtype='LD'),
ds_h AS (SELECT t.captured_at d_cap, h.captured_at s_cap FROM t JOIN tos_handover_label h
         ON h.ytno=t.ytno AND h.comp_ts=t.free_ts AND h.contno=t.contno AND h.jobtype='DS' WHERE t.jobtype='DS'),
ds_r AS (SELECT t.captured_at d_cap, r.captured_at s_cap, EXTRACT(epoch FROM r.comp_ts-t.free_ts) dcomp FROM t JOIN rtg_move_log r
         ON r.trk_id=t.ytno AND r.contno=t.contno AND r.jobtype='DS' AND r.comp_ts BETWEEN t.free_ts-interval '2 minutes' AND t.free_ts+interval '2 minutes' WHERE t.jobtype='DS')
SELECT 'LD: tt_move_log − qc_move_log' what, count(*) n,
  percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM d_cap-s_cap))::int p50,
  percentile_cont(0.9) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM d_cap-s_cap))::int p90,
  percentile_cont(0.99) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM d_cap-s_cap))::int p99, min(EXTRACT(epoch FROM d_cap-s_cap))::int mn FROM ld
UNION ALL SELECT 'DS: tt_move_log − tos_handover_label', count(*), percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM d_cap-s_cap))::int, percentile_cont(0.9) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM d_cap-s_cap))::int, percentile_cont(0.99) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM d_cap-s_cap))::int, min(EXTRACT(epoch FROM d_cap-s_cap))::int FROM ds_h
UNION ALL SELECT 'DS: tt_move_log − rtg_move_log(±2min)', count(*), percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM d_cap-s_cap))::int, percentile_cont(0.9) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM d_cap-s_cap))::int, percentile_cont(0.99) WITHIN GROUP (ORDER BY EXTRACT(epoch FROM d_cap-s_cap))::int, min(EXTRACT(epoch FROM d_cap-s_cap))::int FROM ds_r
UNION ALL SELECT 'DS: rtg.comp_ts − tt_move_log.free_ts (값 차이)', count(*), percentile_cont(0.5) WITHIN GROUP (ORDER BY dcomp)::int, percentile_cont(0.9) WITHIN GROUP (ORDER BY dcomp)::int, percentile_cont(0.99) WITHIN GROUP (ORDER BY dcomp)::int, min(dcomp)::int FROM ds_r;
ROLLBACK;
```

```
 LD: tt_move_log − qc_move_log (same event)               | 16835 | 160 | 288 | 300 |   0
 DS: tt_move_log − tos_handover_label (same event)        | 23284 | 140 | 260 | 299 |   0
 DS: tt_move_log − rtg_move_log (±2min match)             | 23202 | 150 | 272 | 319 | -41
 DS: rtg_move_log.comp_ts − tt_move_log.free_ts (값 차이) | 23202 |   0 |   0 |   0 |  -2
```

- LD 16,835/16,826(짝 99%+ — 중복 QC 행으로 1:多 약간), DS 23,284/23,285 짝지어짐. 파생이 원천보다 **중앙 140~160초·p90 260~288초 늦게** 착지하고, 최소 0(타이머가 착지 직후에 돈 경우), 최대 ≈300(한 주기).
- DS 자유 시각은 `tos_handover_label.comp_ts` 와 `rtg_move_log.comp_ts` 가 **같은 값**(차이 중앙·p90·p99 전부 0초, 최소 −2초). 둘 중 어느 쪽을 원천으로 써도 된다.

**배치 간격 확인(6시간).** tt_move_log 배치 70회·간격 중앙 300초(최소 300·최대 901). 원천 3표는 각 343~347회·중앙 60초(최대 840~1,020초 — 그 사이 한 번 추출 공백이 있었다. 원인 미조사).

**라이브 판단이 이 값을 어떻게 쓰나(`livemap.rs:4701-4720`).**

```
freed AS (SELECT ytno, max(free_ts) f FROM tt_move_log WHERE free_ts > now()-interval '3 hours' GROUP BY 1)
... WHERE fr.f IS NULL OR fr.f < l.pk
```

`freed` 가 없거나 마지막 픽업보다 앞이면 그 트럭은 "아직 일하는 중"(inflight)으로 남고 값은 `GREATEST(0, remaining_p50 − 픽업 후 경과)`. 소비처는 (a) 4763-4764 무응답 트럭의 보류 한도 `SILENT_HOLD_S`→2배, (b) 4783 보류 트럭의 카운트다운, (c) 4813 DS 보이는 트럭의 duration.

**지연이 지금 이 순간 만드는 차이(스냅샷 4회).** 인플라이트로 분류된 트럭 중 "원천은 자유를 봤는데 tt_move_log 는 못 본" 수:

```sql
-- 4703-4717 의 CTE 그대로 + 원천 기반 freed_src(LD=qc_move_log LD·F comp_ts / DS=tos_handover_label DS comp_ts)
SELECT count(*) FILTER (WHERE fr.f IS NULL OR fr.f < l.pk) inflight_by_tt_move_log,
       count(*) FILTER (WHERE fs.f IS NULL OR fs.f < l.pk) inflight_by_source,
       count(*) FILTER (WHERE (fr.f IS NULL OR fr.f < l.pk) AND NOT (fs.f IS NULL OR fs.f < l.pk)) stale_only_in_tt_move_log
FROM latest l LEFT JOIN freed fr ON fr.ytno=l.ytno LEFT JOIN freed_src fs ON fs.ytno=l.ytno;
```

```
167|133|34   04:38:08Z
178|144|34   04:38:28Z
178|141|37   04:38:48Z
(같은 식, 배치 직후) DS 4 / LD 13 = 17
```

분모: "최근 3시간 안에 픽업 기록이 있는 트럭 전부"(167~178대). 그중 34~37대(≈20%)가 원천으로는 이미 자유인데 tt_move_log 로는 아직 인플라이트였고, 그 34~43대의 원천 착지 나이는 **전부 5분 미만**(영구 결손 0 — 순수 배치 지연). 다만 그 트럭들의 앵커 카운트다운 값은 대부분 이미 0(스냅샷 17대 중 rem=0 이 10대, 양수는 최대 DS 35초/LD 295초)이라, **지연의 효과는 값보다 "앵커 소속"에 있다**(무응답 보류 2배 연장이 최대 5분 더 유지되고, 그 트럭은 자유 지점 좌표로 후보에 남는다). 이것이 매칭 결과를 얼마나 바꾸는지는 측정하지 않았다(미확인).

**원천으로 바꾸면.** `freed` 를 아래로 대체하면 착지 지연이 중앙 180~192 → 32~36초, p90 300~314 → 56~60초가 된다(측정값 그대로).

```sql
freed AS (SELECT ytno, max(f) f FROM (
   SELECT trk_id, comp_ts FROM qc_move_log  WHERE jobtype='LD' AND status='F' AND comp_ts > now()-interval '3 hours' AND trk_id IS NOT NULL
   UNION ALL
   SELECT ytno,   comp_ts FROM tos_handover_label WHERE jobtype='DS' AND comp_ts > now()-interval '3 hours' AND ytno IS NOT NULL
 ) u(ytno,f) GROUP BY 1)
```

조인 키·의미 차이:
- 키: `qc_move_log.trk_id` / `tos_handover_label.ytno` / `tt_move_log.ytno` 모두 `TT####` 형식(표본 확인). `rtg_move_log.trk_id` 에는 외부 트럭(ANI25·KTR02 등)이 섞이므로 DS·F 로 좁혀 쓰거나 handover 쪽을 쓴다.
- 트윈: `freed` 는 트럭별 max 라 트윈 처리가 필요 없다(원천도 동일).
- tt_move_log 는 dispatch↔pickup↔free 삼중 결합·3시간 상한·순서 검사를 통과한 행만 담는다(커버 98%). 원천 union 은 그 필터 없이 "마지막 자유 사건"만 보므로 오히려 결손이 적다. 반대로 tt_move_log 에만 있는 정보(cycle_s 등)는 이 자리에서 안 쓴다.
- 인덱스: `qc_move_log_comp_idx(comp_ts)`, `tos_handover_label_comp_idx(comp_ts)` 가 있어 3시간 창 스캔은 지금 `freed` 와 같은 비용 등급이다(`tt_move_log_free_idx(free_ts)`).

### 3.2 원천 표의 고유 지연(바꿀 것 없음, 기준선)

qc_move_log·rtg_move_log·tos_handover_label 은 각각 매분 :05/:25/:45 폴링이라 사건→착지 중앙 32~36초·p90 56~60초·p99 62~68초. `workpool.rs:1329` 의 "크레인이 이미 처리한 DS 지시 제외" 도 이 지연을 그대로 받는다(크레인이 든 뒤 최대 ~1분간 그 상자가 후보에 남는다). live_workpool 은 as_of 를 조회 전 찍고 조회에 4초(지금 관측: `last_success_at − max(as_of_ts)` = 4.1s; 코드 주석은 ~20초)가 든다.

### 3.3 파생 파라미터 표(사건 시각 아님)

| 표 | 정의 원료·창 | 갱신 | 마지막 갱신(관측) |
|---|---|---|---|
| learn_cycle_remaining | tt_move_log 14일 (jobtype,n_containers,dest bucket) p50/p90 | tt-learn-cycle-remaining 30분 | 04:22Z |
| learn_dispatch_lead | qc_move_log 7일 실현 선행 + stage2_match_shadow 7일 arrival | 20분(`workpool.rs:1084`) | autoanalyze 02:50Z |
| learn_qc_wall_cadence | qc_move_log 3일 같은 구역 연속 간격 평균 | 20분(`workpool.rs:1047`) | 04:30Z(as_of_ts) |
| learn_qc_slot_step | dispatch_pred_sample 3일 (elapsed/slot_idx) 중앙 | 20분(`workpool.rs:1050`) | 04:30Z |
| learn_qc_move_time | qc_move_log 3일 리프트 간격 중앙(shift별) | **야간 1회**(tt-nightly → `run --kpi all` → `kpis/qc_move_time`) | 08-18 16:31Z |
| learn_free_in_bias / _stationary / learn_soon_idle_gate | free_in_sample 7일 / tt_cycle_v2+truck_pos_hist 7일 / tt_soon_idle_pred 7일 | spawn_selfcal_refresh 900초 | 03:46Z / 04:02Z / 00:31Z(autovacuum 기준) |

이 표들은 값이 초 단위 통계라 원료 표의 5분 지연이 결과를 움직이지 않는다(14일·7일·3일 창의 중앙값). 판정 "괜찮음"의 근거는 정의 SQL(`pg_matviews.definition`)과 갱신 코드 위치다.

### 3.4 truck_pos_hist (비교기 전용)

`spawn_pos_hist`(30초)가 메모리 GPS 를 `ts=now()` 로 적재. 저장되는 것은 마지막 위치와 분류 상태이고 GPS 고정 시각은 저장하지 않는다(나이 상한 `STALE_AFTER_S`=120초 필터). `spawn_dispatch_compare` 는 T1(=`live_workpool.yt_dis_ts`, 배차 시각) 시점 트럭 위치를 이 표에서 되감는다(`livemap.rs:5359`). T1 은 비교 시점보다 중앙 11.5분 과거(코드 주석 실측)라 30초 양자화는 작다. 배차 판단(매처)은 이 표를 읽지 않고 메모리 `lm.devices` 를 직접 쓴다.

## 4. 미확인 목록

1. tt_move_log 지연이 **매칭 결과를 실제로 얼마나 바꾸는지**(앵커 소속 2배 보류가 추천을 몇 건 바꾸는지)는 측정하지 않았다. 값 차이는 대부분 0 이라는 것까지만 확인.
2. `truck_pos_hist` 에 저장된 위치의 GPS 고정 나이 분포 — 고정 시각이 저장되지 않아 잴 수 없다(상한 120초만 확실).
3. 최근 6시간에 원천 추출 3종·tt_move_log 모두 한 번 840~1,020초 공백이 있었다. 원인(추출기·Oracle·유닛)은 조사하지 않았다.
4. `data_freshness.STOWPLAN` 이 08-06 이후 갱신되지 않는데 `live_stow_plan` 자체는 1.8분 신선하다(STOWPLAN_DELTA 키가 따로 있는 것으로 보임). 라이브 판단은 `data_freshness.WORKPOOL` 만 보므로 영향 없으나 키 관리 문제일 수 있다 — 미조사.
5. `learn_qc_move_time` 이 야간 1회만 갱신되는 것이 의도인지(현재는 20분 갱신 `learn_qc_wall_cadence` 가 덮어쓰는 폴백 층) — 미확인.
6. Oracle 쪽 지연(TOS 가 사건을 커밋하기까지)은 여기서 분리해 잴 수 없다. 모든 지연은 `comp_ts`(TOS 사건 시각) 기준이라 그 몫이 포함돼 있다.
7. 프론트 엔드포인트(`/api/workpool`, `positions`, `dispatch_board`, `stage2_box_compare`, `cycles.rs`)는 범위 밖이라 보지 않았다. 그중 `dispatch_board`·`box_compare`·`dispatch_compare` 는 "TOS 가 배차했는가"를 `tt_move_log.dispatch_ts` 로 읽는다(`workpool.rs:2075, 2131, 2287`) — 표시·평가용이지만 같은 5분 지연을 받는다.
