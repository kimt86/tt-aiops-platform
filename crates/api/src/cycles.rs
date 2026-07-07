//! TT work-cycle history API — reads the accumulated `tt_cycle_log` (written by the
//! live-map cycle flusher). Two endpoints power the Cycle History page:
//!   * `GET /api/tt-cycles/summary?hours=` — fleet overview: KPI totals, a per-bucket
//!     throughput series, and a per-truck aggregate leaderboard.
//!   * `GET /api/tt-cycles/detail?ytno=&hours=&limit=` — one truck's recent cycles
//!     (the timeline rows: phase timestamps, legs, job metadata).
//! Pure Postgres reads; no Oracle/GPS. Each "cycle" = one validated container delivery.

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

    let (total_cycles, trucks, fleet_median_s, fleet_laden_km): (i64, i64, Option<f64>, Option<f64>) =
        sqlx::query_as(
            "SELECT count(*), count(DISTINCT ytno),
                    percentile_cont(0.5) WITHIN GROUP (ORDER BY cycle_s),
                    coalesce(sum(laden_leg_m), 0) / 1000.0
               FROM tt_cycle_log
              WHERE dropped_at > now() - ($1::int * interval '1 hour')",
        )
        .bind(hours)
        .fetch_one(&pool)
        .await?;

    let buckets: Vec<TpBucket> = sqlx::query_as(
        "SELECT date_bin(($2::int * interval '1 minute'), dropped_at, timestamptz '2000-01-01') AS t,
                count(*) AS n
           FROM tt_cycle_log
          WHERE dropped_at > now() - ($1::int * interval '1 hour')
          GROUP BY t ORDER BY t",
    )
    .bind(hours)
    .bind(bucket_min)
    .fetch_all(&pool)
    .await?;

    let trucks_list: Vec<TruckAgg> = sqlx::query_as(
        "SELECT ytno,
                count(*) AS cycles,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY cycle_s) AS median_s,
                avg(cycle_s)::float8 AS avg_s,
                coalesce(sum(laden_leg_m), 0) / 1000.0 AS laden_km,
                percentile_cont(0.25) WITHIN GROUP (ORDER BY cycle_s) AS p25_s,
                percentile_cont(0.75) WITHIN GROUP (ORDER BY cycle_s) AS p75_s,
                count(*) FILTER (WHERE jobtype = 'DS') AS ds,
                count(*) FILTER (WHERE jobtype = 'LD') AS ld,
                count(*) FILTER (WHERE jobtype IS NULL OR jobtype NOT IN ('DS','LD')) AS other,
                max(dropped_at) AS last_drop,
                min(dropped_at) AS first_drop
           FROM tt_cycle_log
          WHERE dropped_at > now() - ($1::int * interval '1 hour')
          GROUP BY ytno
          ORDER BY cycles DESC, ytno",
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
    let cycles: Vec<CycleRow> = sqlx::query_as(
        "SELECT v1.dropped_at, v1.jobtype, v1.vessel, v1.voyage, v1.container, v1.qc,
                v1.cycle_s, v1.laden_leg_s, v1.laden_leg_m, v1.empty_leg_s, v1.empty_leg_m,
                v1.container_to_container,
                v2.opened_at        AS v2_opened_at,
                v2.empty_arrived_at AS v2_empty_arrived_at,
                v2.pickup_left_at   AS v2_pickup_left_at,
                v2.laden_arrived_at AS v2_laden_arrived_at
           FROM tt_cycle_log v1
           LEFT JOIN tt_cycle_v2 v2 ON v2.ytno = v1.ytno AND v2.dropped_at = v1.dropped_at
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
