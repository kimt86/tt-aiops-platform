//! TT work-cycle history API. Two endpoints power the Cycle History page:
//!   * `GET /api/tt-cycles/summary?hours=` — fleet overview: KPI totals, a per-bucket
//!     throughput series, and a per-truck aggregate leaderboard.
//!   * `GET /api/tt-cycles/detail?ytno=&hours=&limit=` — one truck's recent cycles
//!     (the timeline rows: phase timestamps, legs, job metadata).
//! Pure Postgres reads; no Oracle/GPS. Each "cycle" = one delivered container.
//!
//! CYCLE TIME is sourced from `tt_move_log` (dispatch YT_DIS_DT → crane-free, both TOS-authoritative;
//! validated ±5s vs tt_cycle_v2). The older GPS-mover `tt_cycle_log` undercounts by ~41% (starts its
//! clock after dispatch + drops the longest ~42% of cycles), so it is NO longer the cycle-time source.
//! Laden DISTANCE (laden_km) is still read from `tt_cycle_log` — tt_move_log carries no per-leg meters.

use axum::extract::{Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::routes::AppError;

fn clamp_hours(h: Option<i32>) -> i32 {
    h.unwrap_or(12).clamp(1, 24 * 14)
}

// ───────────────────────── summary ─────────────────────────

#[derive(Deserialize)]
pub struct SummaryQ {
    hours: Option<i32>,
}

#[derive(Serialize, sqlx::FromRow)]
struct TruckAgg {
    ytno: String,
    cycles: i64,
    median_s: Option<f64>,
    avg_s: Option<f64>,
    laden_km: Option<f64>,
    p25_s: Option<f64>,
    p75_s: Option<f64>,
    ds: i64,
    ld: i64,
    other: i64,
    last_drop: DateTime<Utc>,
    first_drop: DateTime<Utc>,
}

#[derive(Serialize, sqlx::FromRow)]
struct TpBucket {
    t: DateTime<Utc>,
    n: i64,
}

#[derive(Serialize)]
pub struct SummaryResp {
    hours: i32,
    total_cycles: i64,
    trucks: i64,
    fleet_median_s: Option<f64>,
    fleet_laden_km: f64,
    cycles_per_hr: f64,
    bucket_min: i64,
    buckets: Vec<TpBucket>,
    trucks_list: Vec<TruckAgg>,
}

pub async fn summary(
    State(pool): State<PgPool>,
    Query(q): Query<SummaryQ>,
) -> Result<Json<SummaryResp>, AppError> {
    let hours = clamp_hours(q.hours);
    // bucket width scales with the window so the throughput chart stays ~legible (≈48 bars)
    let bucket_min: i64 = match hours {
        0..=6 => 10,
        7..=24 => 20,
        25..=72 => 60,
        _ => 180,
    };

    // cycle time + counts: tt_move_log (dispatch→free, authoritative). laden km: tt_cycle_log (only
    // source with per-leg meters) — separate query so the two sources stay independent.
    let (total_cycles, trucks, fleet_median_s): (i64, i64, Option<f64>) = sqlx::query_as(
        "SELECT count(*), count(DISTINCT ytno),
                percentile_cont(0.5) WITHIN GROUP (ORDER BY cycle_s)
           FROM tt_move_log
          WHERE free_ts > now() - ($1::int * interval '1 hour')",
    )
    .bind(hours)
    .fetch_one(&pool)
    .await?;

    let (fleet_laden_km,): (Option<f64>,) = sqlx::query_as(
        "SELECT coalesce(sum(laden_leg_m), 0) / 1000.0
           FROM tt_cycle_log
          WHERE dropped_at > now() - ($1::int * interval '1 hour')",
    )
    .bind(hours)
    .fetch_one(&pool)
    .await?;

    let buckets: Vec<TpBucket> = sqlx::query_as(
        "SELECT date_bin(($2::int * interval '1 minute'), free_ts, timestamptz '2000-01-01') AS t,
                count(*) AS n
           FROM tt_move_log
          WHERE free_ts > now() - ($1::int * interval '1 hour')
          GROUP BY t ORDER BY t",
    )
    .bind(hours)
    .bind(bucket_min)
    .fetch_all(&pool)
    .await?;

    // per-truck: cycle-time metrics from tt_move_log; laden km left-joined from tt_cycle_log.
    let trucks_list: Vec<TruckAgg> = sqlx::query_as(
        "SELECT m.ytno,
                m.cycles,
                m.median_s,
                m.avg_s,
                coalesce(k.laden_km, 0) AS laden_km,
                m.p25_s,
                m.p75_s,
                m.ds,
                m.ld,
                m.other,
                m.last_drop,
                m.first_drop
           FROM (
             SELECT ytno,
                    count(*) AS cycles,
                    percentile_cont(0.5) WITHIN GROUP (ORDER BY cycle_s) AS median_s,
                    avg(cycle_s)::float8 AS avg_s,
                    percentile_cont(0.25) WITHIN GROUP (ORDER BY cycle_s) AS p25_s,
                    percentile_cont(0.75) WITHIN GROUP (ORDER BY cycle_s) AS p75_s,
                    count(*) FILTER (WHERE jobtype = 'DS') AS ds,
                    count(*) FILTER (WHERE jobtype = 'LD') AS ld,
                    count(*) FILTER (WHERE jobtype IS NULL OR jobtype NOT IN ('DS','LD')) AS other,
                    max(free_ts) AS last_drop,
                    min(free_ts) AS first_drop
               FROM tt_move_log
              WHERE free_ts > now() - ($1::int * interval '1 hour')
              GROUP BY ytno
           ) m
           LEFT JOIN (
             SELECT ytno, coalesce(sum(laden_leg_m), 0) / 1000.0 AS laden_km
               FROM tt_cycle_log
              WHERE dropped_at > now() - ($1::int * interval '1 hour')
              GROUP BY ytno
           ) k ON k.ytno = m.ytno
          ORDER BY m.cycles DESC, m.ytno",
    )
    .bind(hours)
    .fetch_all(&pool)
    .await?;

    let cycles_per_hr = total_cycles as f64 / hours as f64;
    Ok(Json(SummaryResp {
        hours,
        total_cycles,
        trucks,
        fleet_median_s,
        fleet_laden_km: fleet_laden_km.unwrap_or(0.0),
        cycles_per_hr,
        bucket_min,
        buckets,
        trucks_list,
    }))
}

// ───────────────────────── detail (per truck) ─────────────────────────

#[derive(Deserialize)]
pub struct DetailQ {
    ytno: String,
    hours: Option<i32>,
    limit: Option<i64>,
}

#[derive(Serialize, sqlx::FromRow)]
struct CycleRow {
    dropped_at: DateTime<Utc>,
    jobtype: Option<String>,
    vessel: Option<String>,
    voyage: Option<String>,
    container: Option<String>,
    qc: Option<String>,
    cycle_s: Option<i32>,
    laden_leg_s: Option<i32>,
    laden_leg_m: Option<f64>,
    empty_leg_s: Option<i32>,
    empty_leg_m: Option<f64>,
    container_to_container: bool,
    // Current 5-event model (tt_cycle_v2, same ytno+dropped_at). NULL where v2 has no row or the
    // event was unobserved. dropped_at (⑤) is shared with v1's validated drop above. The retired
    // v1 4-phase timestamps and the v2 "빈차 출발" event are no longer served.
    //   ①배차 opened · ②픽업도착 empty_arrived · ③픽업떠남 pickup_left · ④부하도착 laden_arrived · ⑤드롭 dropped
    v2_opened_at: Option<DateTime<Utc>>,
    v2_empty_arrived_at: Option<DateTime<Utc>>,
    v2_pickup_left_at: Option<DateTime<Utc>>,
    v2_laden_arrived_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
pub struct DetailResp {
    ytno: String,
    hours: i32,
    cycles: Vec<CycleRow>,
}

pub async fn detail(
    State(pool): State<PgPool>,
    Query(q): Query<DetailQ>,
) -> Result<Json<DetailResp>, AppError> {
    let hours = clamp_hours(q.hours);
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    // cycle_s and the leg TIMES come from tt_move_log (dispatch→free, same source as the summary
    // KPI) so the drill-down matches the headline; leg METERS stay from v1 (tt_move_log has no
    // meters). Falls back to v1 where tt_move_log has no matching row. empty_s (dispatch→pickup)
    // includes the pre-pickup wait — consistent with the timeline's 배차(opened)→픽업 span shown here.
    let cycles: Vec<CycleRow> = sqlx::query_as(
        "SELECT v1.dropped_at, v1.jobtype, v1.vessel, v1.voyage, v1.container, v1.qc,
                coalesce(m.cycle_s, v1.cycle_s)      AS cycle_s,
                coalesce(m.laden_s, v1.laden_leg_s)  AS laden_leg_s, v1.laden_leg_m,
                coalesce(m.empty_s, v1.empty_leg_s)  AS empty_leg_s, v1.empty_leg_m,
                v1.container_to_container,
                v2.opened_at        AS v2_opened_at,
                v2.empty_arrived_at AS v2_empty_arrived_at,
                v2.pickup_left_at   AS v2_pickup_left_at,
                v2.laden_arrived_at AS v2_laden_arrived_at
           FROM tt_cycle_log v1
           LEFT JOIN tt_cycle_v2 v2 ON v2.ytno = v1.ytno AND v2.dropped_at = v1.dropped_at
           LEFT JOIN LATERAL (
             SELECT m.cycle_s, m.empty_s, m.laden_s
               FROM tt_move_log m
              WHERE m.ytno = v1.ytno
                AND m.free_ts >= v1.dropped_at - interval '5 min'
                AND m.free_ts <  v1.dropped_at + interval '5 min'
              ORDER BY (m.contno IS DISTINCT FROM v1.container),
                       abs(EXTRACT(EPOCH FROM (m.free_ts - v1.dropped_at)))
              LIMIT 1
           ) m ON true
          WHERE v1.ytno = $1 AND v1.dropped_at > now() - ($2::int * interval '1 hour')
          ORDER BY v1.dropped_at DESC
          LIMIT $3",
    )
    .bind(&q.ytno)
    .bind(hours)
    .bind(limit)
    .fetch_all(&pool)
    .await?;
    Ok(Json(DetailResp { ytno: q.ytno, hours, cycles }))
}
