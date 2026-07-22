//! TT work-cycle history API. Two endpoints power the Cycle History page:
//!   * `GET /api/tt-cycles/summary?hours=` — fleet overview: KPI totals, a per-bucket
//!     throughput series, and a per-truck aggregate leaderboard.
//!   * `GET /api/tt-cycles/detail?ytno=&hours=&limit=` — one truck's recent cycles with the
//!     GPS-reconstructed phase breakdown.
//! Pure Postgres reads; no Oracle.
//!
//! CYCLE = one PHYSICAL TRIP, defined by `tt_move_log` (dispatch YT_DIS_DT → last crane-free, both
//! TOS-authoritative; twins collapsed by (ytno, dispatch_ts)). The per-phase DRIVE/STOP/WAIT split
//! comes from `tt_cycle_recon` (GPS motion-segmented within the TOS boundaries). The legacy GPS cycle
//! logs (tt_cycle_log / tt_cycle_v2) are no longer read here.

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
    drive_km: Option<f64>,
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
    fleet_drive_km: f64,
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

    // Headline = one row per PHYSICAL TRIP (tt_move_log twin_leg_seq=1), full-history authoritative.
    let (total_cycles, trucks, fleet_median_s): (i64, i64, Option<f64>) = sqlx::query_as(
        "SELECT count(*), count(DISTINCT ytno),
                percentile_cont(0.5) WITHIN GROUP (ORDER BY cycle_s)
           FROM tt_move_log
          WHERE twin_leg_seq = 1 AND free_ts > now() - ($1::int * interval '1 hour')",
    )
    .bind(hours)
    .fetch_one(&pool)
    .await?;

    // Driven distance from the GPS reconstruction (recent window only — hifreq ~1-day retention).
    let (fleet_drive_km,): (Option<f64>,) = sqlx::query_as(
        "SELECT (coalesce(sum(e_drive_m + l_drive_m), 0) / 1000.0)::float8
           FROM tt_cycle_recon
          WHERE free_ts > now() - ($1::int * interval '1 hour')",
    )
    .bind(hours)
    .fetch_one(&pool)
    .await?;

    let buckets: Vec<TpBucket> = sqlx::query_as(
        "SELECT date_bin(($2::int * interval '1 minute'), free_ts, timestamptz '2000-01-01') AS t,
                count(*) AS n
           FROM tt_move_log
          WHERE twin_leg_seq = 1 AND free_ts > now() - ($1::int * interval '1 hour')
          GROUP BY t ORDER BY t",
    )
    .bind(hours)
    .bind(bucket_min)
    .fetch_all(&pool)
    .await?;

    // per-truck: cycle-time metrics per trip (tt_move_log); driven km left-joined from tt_cycle_recon.
    let trucks_list: Vec<TruckAgg> = sqlx::query_as(
        "SELECT m.ytno, m.cycles, m.median_s, m.avg_s,
                coalesce(r.drive_km, 0) AS drive_km,
                m.p25_s, m.p75_s, m.ds, m.ld, m.other, m.last_drop, m.first_drop
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
              WHERE twin_leg_seq = 1 AND free_ts > now() - ($1::int * interval '1 hour')
              GROUP BY ytno
           ) m
           LEFT JOIN (
             SELECT ytno, (sum(e_drive_m + l_drive_m) / 1000.0)::float8 AS drive_km
               FROM tt_cycle_recon
              WHERE free_ts > now() - ($1::int * interval '1 hour')
              GROUP BY ytno
           ) r ON r.ytno = m.ytno
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
        fleet_drive_km: fleet_drive_km.unwrap_or(0.0),
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

// One physical trip with the GPS-reconstructed 7-phase decomposition (durations, seconds) that
// reconciles exactly to cycle_s: dispatch_wait + e_drive + e_stop + pickup_dwell + l_drive + l_stop
// + drop_dwell = cycle_s. gps_covered=false ⇒ no drive segment observed (GPS-silent / aged out of
// hifreq): the split is unavailable and only cycle_s is meaningful.
#[derive(Serialize, sqlx::FromRow)]
struct CycleRow {
    dispatch_ts: DateTime<Utc>,
    pickup_ts: Option<DateTime<Utc>>,
    free_ts: DateTime<Utc>,
    jobtype: Option<String>,
    container: Option<String>,
    contnos: Vec<String>,
    is_twin: bool,
    n_containers: i32,
    cycle_s: i32,
    dispatch_wait_s: i32,
    e_drive_s: i32,
    e_stop_s: i32,
    pickup_dwell_s: i32,
    l_drive_s: i32,
    l_stop_s: i32,
    drop_dwell_s: i32,
    e_drive_m: i32,
    l_drive_m: i32,
    gps_covered: bool,
    n_fix: i32,
    long_gap_s: i32,
    pickup_crane: Option<String>,
    free_crane: Option<String>,
    // Twin intermediate waypoints (A→B→C): non-final drops (free_ts < the trip's last free) and
    // non-first pickups (pickup_ts > the trip's first pickup). Empty for single-container trips.
    // Parallel arrays aligned by time (sqlx has no json feature; this codebase passes arrays/text).
    waypoint_ts: Vec<DateTime<Utc>>,
    waypoint_crane: Vec<Option<String>>,
    waypoint_kind: Vec<String>,
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
        "SELECT r.dispatch_ts, r.pickup_ts, r.free_ts, r.jobtype, r.contno AS container,
                coalesce(r.contnos, ARRAY[r.contno]) AS contnos,
                coalesce(r.is_twin,false) AS is_twin, coalesce(r.n_containers,1) AS n_containers,
                coalesce(r.cycle_s,0)         AS cycle_s,
                coalesce(r.dispatch_wait_s,0) AS dispatch_wait_s,
                coalesce(r.e_drive_s,0)       AS e_drive_s,
                coalesce(r.e_stop_s,0)        AS e_stop_s,
                coalesce(r.pickup_dwell_s,0)  AS pickup_dwell_s,
                coalesce(r.l_drive_s,0)       AS l_drive_s,
                coalesce(r.l_stop_s,0)        AS l_stop_s,
                coalesce(r.drop_dwell_s,0)    AS drop_dwell_s,
                coalesce(r.e_drive_m,0)       AS e_drive_m,
                coalesce(r.l_drive_m,0)       AS l_drive_m,
                coalesce(r.gps_covered,false) AS gps_covered,
                coalesce(r.n_fix,0)           AS n_fix,
                coalesce(r.long_gap_s,0)      AS long_gap_s,
                m.pickup_crane, m.free_crane,
                coalesce(wp.ts,    '{}'::timestamptz[]) AS waypoint_ts,
                coalesce(wp.crane, '{}'::text[])        AS waypoint_crane,
                coalesce(wp.kind,  '{}'::text[])        AS waypoint_kind
           FROM tt_cycle_recon r
           LEFT JOIN LATERAL (
             -- main pickup = FIRST pickup (matches recon pickup_ts=min); main drop = LAST drop (free_ts=max),
             -- so a twin reads A(pickup)→…waypoints…→C(final drop) consistently.
             SELECT (SELECT p.pickup_crane FROM tt_move_log p
                      WHERE p.ytno = r.ytno AND p.dispatch_ts = r.dispatch_ts ORDER BY p.pickup_ts ASC  LIMIT 1) AS pickup_crane,
                    (SELECT f.free_crane   FROM tt_move_log f
                      WHERE f.ytno = r.ytno AND f.dispatch_ts = r.dispatch_ts ORDER BY f.free_ts   DESC LIMIT 1) AS free_crane
           ) m ON true
           LEFT JOIN LATERAL (
             SELECT array_agg(w.ts ORDER BY w.ts)    AS ts,
                    array_agg(w.crane ORDER BY w.ts) AS crane,
                    array_agg(w.kind ORDER BY w.ts)  AS kind
               FROM (
                 -- intermediate DROP at a different block than the final drop (true extra destination)
                 SELECT tl.free_ts AS ts, tl.free_crane AS crane, 'drop'::text AS kind
                   FROM tt_move_log tl
                  WHERE tl.ytno = r.ytno AND tl.dispatch_ts = r.dispatch_ts
                    AND tl.free_ts < r.free_ts
                    AND tl.free_crane IS DISTINCT FROM
                        (SELECT f.free_crane FROM tt_move_log f
                          WHERE f.ytno = r.ytno AND f.dispatch_ts = r.dispatch_ts ORDER BY f.free_ts DESC LIMIT 1)
                 UNION ALL
                 -- intermediate PICKUP at a different block than the first pickup (skips same-crane staggered lifts)
                 SELECT tl.pickup_ts, tl.pickup_crane, 'pickup'::text
                   FROM tt_move_log tl
                  WHERE tl.ytno = r.ytno AND tl.dispatch_ts = r.dispatch_ts
                    AND tl.pickup_ts > r.pickup_ts
                    AND tl.pickup_crane IS DISTINCT FROM
                        (SELECT p.pickup_crane FROM tt_move_log p
                          WHERE p.ytno = r.ytno AND p.dispatch_ts = r.dispatch_ts ORDER BY p.pickup_ts ASC LIMIT 1)
               ) w
           ) wp ON true
          WHERE r.ytno = $1 AND r.free_ts > now() - ($2::int * interval '1 hour')
          ORDER BY r.free_ts DESC
          LIMIT $3",
    )
    .bind(&q.ytno)
    .bind(hours)
    .bind(limit)
    .fetch_all(&pool)
    .await?;
    Ok(Json(DetailResp { ytno: q.ytno, hours, cycles }))
}
