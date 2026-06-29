#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# 장비 유효 작업시간 추정 — 실측 로그에서 (데이터시트 불요).
#
# 시뮬레이터가 쓸 "장비 스펙"을 운영 로그로 역추정한다. 핵심 발견:
#   · QC/YC 작업시간의 신뢰 가능한 산출물 = **유효시간 분포**(시뮬이 샘플).
#   · 위치(베이/행/단) 회귀로 갠트리/권상 *속도* 분해는 **신뢰 불가** — 무브간 gap이
#     truck-wait에 묻히고(QC 갠트리 ≈0, intercept 120s), 단변량 회귀가 교란됨
#     (YC hoist 회귀 음수). MCH_OPERATION의 COMP−ST도 권상 사이클을 못 잡음.
#     → 분해 대신 분포를 쓴다. (정밀 분해는 PLC 모션로그 같은 별도 계측 필요.)
#   · TT 속도는 GPS로 깨끗이 추정됨(공차 leg).
#
# 출처: rtg_move_log·learn_qc_move_time·learn_travel_sample·tt_cycle_v2 (전부 로컬 PG).
# 사용: bash scripts/estimate_equipment_specs.sh
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail
export PGPASSWORD=wp
PSQL="psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -P pager=off"

echo "### YC(RTG) 서비스시간 분포 — rtg_move_log, 최근 24h (시뮬: 이 분포를 샘플) ###"
$PSQL -c "
SELECT jobtype,
       count(*) n,
       round(percentile_cont(0.10) WITHIN GROUP (ORDER BY dur_s)) p10_s,
       round(percentile_cont(0.50) WITHIN GROUP (ORDER BY dur_s)) p50_s,
       round(percentile_cont(0.90) WITHIN GROUP (ORDER BY dur_s)) p90_s
  FROM rtg_move_log
 WHERE st_ts > now() - interval '24 hours' AND dur_s BETWEEN 5 AND 600
 GROUP BY jobtype ORDER BY n DESC LIMIT 8;"

echo "### QC 무브 cadence — learn_qc_move_time (컨테이너 1개 처리, shift별) ###"
$PSQL -c "
SELECT jobtype, shift, round(avg(med_sec)) med_sec, count(distinct qc) cranes
  FROM learn_qc_move_time GROUP BY jobtype, shift ORDER BY jobtype, shift;"

echo "### TT 주행속도 — learn_travel_sample. ⚠ 점대점 km/h(아래 'eff')는 ~270s/leg 오버헤드가 섞여 ###"
echo "###   너무 느리게 보임. 거리대별로 보면 진짜 주행속도가 드러남(짧은 leg=정렬·대기 지배).        ###"
$PSQL -c "
SELECT CASE WHEN dist_m<200 THEN '1.<200m' WHEN dist_m<500 THEN '2.200-500' WHEN dist_m<1000 THEN '3.500-1k'
            WHEN dist_m<2000 THEN '4.1-2k' ELSE '5.>2k' END bucket,
       count(*) n, round(percentile_cont(0.5) WITHIN GROUP (ORDER BY dist_m)) dist_m,
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY travel_s)) time_s,
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY dist_m/NULLIF(travel_s,0)*3.6)::numeric,1) eff_kmh
  FROM learn_travel_sample WHERE travel_s BETWEEN 5 AND 1800 AND dist_m > 20 GROUP BY 1 ORDER BY 1;"
echo "  → 라우터엔 주행속도를 쓰고, 정지 오버헤드는 에뮬레이터(핸드오버·큐·STALL)로 별도 모델."

echo "### ★ 순수 trip 추출 — GPS 모션분할 (empty_travel, 30초 구간: 움직임 vs 정지) ###"
$PSQL -c "
WITH s AS (
  SELECT ts, lat, lon, LAG(lat) OVER w plat, LAG(lon) OVER w plon, LAG(ts) OVER w pts
  FROM truck_pos_hist WHERE state='empty_travel' WINDOW w AS (PARTITION BY ytno ORDER BY ts)
),
seg AS (
  SELECT extract(epoch FROM ts-pts) dt,
    2*6371000*asin(sqrt(power(sin(radians(lat-plat)/2),2)+cos(radians(plat))*cos(radians(lat))*power(sin(radians(lon-plon)/2),2))) disp_m
  FROM s WHERE pts IS NOT NULL
)
SELECT count(*) seg30s,
  round(100.0*count(*) FILTER (WHERE disp_m<8)/count(*),1) stopped_pct,
  round(percentile_cont(0.5) WITHIN GROUP (ORDER BY disp_m/dt*3.6) FILTER (WHERE disp_m>=8)::numeric,1) moving_p50_kmh,
  round(percentile_cont(0.9) WITHIN GROUP (ORDER BY disp_m/dt*3.6) FILTER (WHERE disp_m>=8)::numeric,1) moving_p90_kmh
FROM seg WHERE dt BETWEEN 20 AND 45 AND disp_m < 2500;"
echo "  → 정지 35% 제외 = 순수 주행. 주행속도 ~24 km/h. (또는 empty_trip_m=실경로길이 ÷ 주행속도.)"
echo "  → 이걸로 '순수-trip OD'를 재학습하면 라우터 입력이 깨끗(현 learn_travel_zone225는 정지 포함, 같은셀도 228s)."

echo "### TT 적재 vs 공차 leg 시간 — tt_cycle_v2, 최근 2일 (시간차는 거리차 포함, 속도 아님) ###"
$PSQL -c "
SELECT round(percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM (empty_arrived_at-empty_travel_start_at)))) empty_leg_med_s,
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM (laden_arrived_at-pickup_left_at))))       laden_leg_med_s
  FROM tt_cycle_v2
 WHERE empty_travel_start_at IS NOT NULL AND empty_arrived_at IS NOT NULL
   AND laden_arrived_at IS NOT NULL AND pickup_left_at IS NOT NULL
   AND dropped_at > now() - interval '2 days';"

echo "### 해치커버 (덱↔홀드 전환, 베이당 1회) — research-log 실측 ###"
echo "  양하(덱→홀드) ~428s · 적하(홀드→덱) ~496s"

cat <<'NOTE'

# ── 위치 회귀(갠트리/권상 분해) 진단 — 신뢰 불가, 참고만 ──
# Oracle MCH_OPERATION (CRNT_PSN_IDX_NO1~3=[베이,행,단], 1일)로 시도한 결과:
#   YC: hoist_per_tier=-0.68(음수·비물리) · gantry_per_bay=0.96 · trolley_per_row=4.13 (교란)
#   QC: gantry_per_bay=-0.01(≈0, gap intercept 120s=truck cycle에 묻힘)
# → 분해 속도는 채택 안 함. 시뮬은 위 *유효시간 분포*를 샘플하고, 베이이동은
#   분포 안에 흡수(또는 라우터/레이아웃 거리로 별도 추정)한다.
NOTE
