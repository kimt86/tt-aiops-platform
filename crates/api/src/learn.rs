//! Learning center API. v1 model = block work-point coordinates (target ②): per topos code,
//! the learned (lat,lon) accumulated from TTs observed ARRIVED there (livemap centroids,
//! persisted by `spawn_learn_persist`). Exposes the model points, a summary, and a quality
//! time series so the dashboard can show accumulation + precision improving over time.
//! Pure Postgres reads.

use axum::extract::State;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;

use crate::routes::AppError;

#[derive(Serialize, sqlx::FromRow)]
struct ToposPoint {
    topos: String,
    is_crane: bool,
    lat: f64,
    lon: f64,
    n: i32,
    obs: i64,
    spread_m: Option<f64>,
    updated_at: DateTime<Utc>,
}

#[derive(Serialize, sqlx::FromRow)]
struct MetricPoint {
    captured_at: DateTime<Utc>,
    distinct_topos: i32,
    confident_topos: i32,
    total_obs: i64,
    median_spread_m: Option<f64>,
}

#[derive(Serialize)]
pub struct ToposResp {
    distinct_topos: i64,
    confident_topos: i64, // n ≥ 30
    block_points: i64,    // non-crane work-points (the focus)
    total_obs: i64,
    median_spread_m: Option<f64>,
    points: Vec<ToposPoint>,
    metric_series: Vec<MetricPoint>,
}

/// GET /api/learn/topos — the block work-point coordinate model + accumulation/quality series.
pub async fn topos(State(pool): State<PgPool>) -> Result<Json<ToposResp>, AppError> {
    let points: Vec<ToposPoint> = sqlx::query_as(
        "SELECT topos, is_crane, lat, lon, n, obs, spread_m, updated_at
           FROM learn_topos_point ORDER BY obs DESC LIMIT 1000",
    )
    .fetch_all(&pool)
    .await?;

    let metric_series: Vec<MetricPoint> = sqlx::query_as(
        "SELECT captured_at, distinct_topos, confident_topos, total_obs, median_spread_m
           FROM learn_topos_metric
          WHERE captured_at > now() - interval '30 days'
          ORDER BY captured_at",
    )
    .fetch_all(&pool)
    .await?;

    let (distinct_topos, confident_topos, block_points, total_obs, median_spread_m): (
        i64,
        i64,
        i64,
        i64,
        Option<f64>,
    ) = sqlx::query_as(
        "SELECT count(*),
                count(*) FILTER (WHERE n >= 30),
                count(*) FILTER (WHERE NOT is_crane),
                coalesce(sum(obs), 0)::bigint,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY spread_m) FILTER (WHERE n >= 30)
           FROM learn_topos_point",
    )
    .fetch_one(&pool)
    .await?;

    Ok(Json(ToposResp {
        distinct_topos,
        confident_topos,
        block_points,
        total_obs,
        median_spread_m,
        points,
        metric_series,
    }))
}

// ───────────────────────── lanes (③) ─────────────────────────

#[derive(Serialize, sqlx::FromRow)]
struct LaneCellOut {
    lat: f64,
    lon: f64,
    passes: i64,
    heading_deg: Option<f64>,
    directionality: Option<f64>,
    mean_speed: Option<f64>,
}

#[derive(Serialize, sqlx::FromRow)]
struct LaneMetricPoint {
    captured_at: DateTime<Utc>,
    cells: i32,
    road_cells: i32,
    total_passes: i64,
    oneway_frac: Option<f64>,
}

#[derive(Serialize)]
pub struct LanesResp {
    cells: i64,
    road_cells: i64, // passes ≥ 20
    total_passes: i64,
    oneway_frac: Option<f64>, // road cells with directionality ≥ 0.8
    grid: Vec<LaneCellOut>,
    metric_series: Vec<LaneMetricPoint>,
}

/// GET /api/learn/lanes — the learned driving-lane grid + accumulation/quality series.
pub async fn lanes(State(pool): State<PgPool>) -> Result<Json<LanesResp>, AppError> {
    let grid: Vec<LaneCellOut> = sqlx::query_as(
        "SELECT lat, lon, passes, heading_deg, directionality, mean_speed
           FROM learn_lane_cell WHERE passes >= 5 ORDER BY passes DESC LIMIT 4000",
    )
    .fetch_all(&pool)
    .await?;

    let metric_series: Vec<LaneMetricPoint> = sqlx::query_as(
        "SELECT captured_at, cells, road_cells, total_passes, oneway_frac
           FROM learn_lane_metric
          WHERE captured_at > now() - interval '30 days'
          ORDER BY captured_at",
    )
    .fetch_all(&pool)
    .await?;

    let (cells, road_cells, total_passes, oneway_frac): (i64, i64, i64, Option<f64>) =
        sqlx::query_as(
            "SELECT count(*),
                    count(*) FILTER (WHERE passes >= 20),
                    coalesce(sum(passes), 0)::bigint,
                    (count(*) FILTER (WHERE passes >= 20 AND directionality >= 0.8))::float8
                      / nullif(count(*) FILTER (WHERE passes >= 20), 0)
               FROM learn_lane_cell",
        )
        .fetch_one(&pool)
        .await?;

    Ok(Json(LanesResp { cells, road_cells, total_passes, oneway_frac, grid, metric_series }))
}

// ───────────────────────── travel time (①) ─────────────────────────

#[derive(Serialize, sqlx::FromRow)]
struct TravelOd {
    origin: String, // origin_zone (block+bay or quay GPS-grid)
    dest: String,   // dest_zone
    n: i64,
    median_s: Option<f64>,
    dist_m: Option<f64>,
    speed_kmh: Option<f64>,
}

#[derive(Serialize, sqlx::FromRow)]
struct TravelMetricPoint {
    captured_at: DateTime<Utc>,
    samples: i64,
    od_pairs: i32,
    confident_pairs: i32,
    median_speed_kmh: Option<f64>,
}

// "test result": recent completed trips vs what we'd have predicted (their OD median) — how far
// off were we. Answers "이동 완료마다 예측한 시간과 얼마나 차이나나". Honest (shows the ~50% ceiling).
#[derive(Serialize, sqlx::FromRow)]
struct TravelAccuracy {
    evaluated: i64,               // recent trips on a confident OD (n≥10), last 2 days
    mape_pct: Option<f64>,        // median |actual − pred| / actual
    median_abs_err_s: Option<f64>,
    within_30pct: Option<f64>,    // % of trips predicted within ±30%
}

#[derive(Serialize)]
pub struct TravelResp {
    samples: i64,
    od_pairs: i64,                  // distinct ZONE pairs (block+bay / quay GPS-grid)
    confident_pairs: i64,           // zone pairs with n ≥ 10 (the usable model)
    confident_pairs_fullcode: i64,  // raw-code pairs n ≥ 10 (before-zoning, for comparison)
    median_speed_kmh: Option<f64>,  // distance ÷ speed fallback for sparse pairs
    weather_wet_median_s: Option<f64>, // median empty/laden leg in precip > 0.1mm …
    weather_dry_median_s: Option<f64>, // … vs dry — quick weather-feature signal
    accuracy: TravelAccuracy,       // latest predicted-vs-actual test
    od: Vec<TravelOd>,
    metric_series: Vec<TravelMetricPoint>,
}

/// GET /api/learn/travel — per (origin_zone→dest_zone) travel-time model (v1: zone keys +
/// distance fallback) + a quick weather signal + the quality series. See research/travel-time.
pub async fn travel(State(pool): State<PgPool>) -> Result<Json<TravelResp>, AppError> {
    // zone-pair model: median travel + distance + implied speed, densest pairs first.
    let od: Vec<TravelOd> = sqlx::query_as(
        "SELECT origin_zone AS origin, dest_zone AS dest, count(*) AS n,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY travel_s) AS median_s,
                avg(dist_m) AS dist_m,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY (dist_m/1000.0)/nullif(travel_s/3600.0,0))
                  FILTER (WHERE dist_m IS NOT NULL AND travel_s > 0) AS speed_kmh
           FROM learn_travel_sample
          WHERE origin_zone IS NOT NULL AND dest_zone IS NOT NULL
          GROUP BY origin_zone, dest_zone
         HAVING count(*) >= 3
          ORDER BY count(*) DESC LIMIT 500",
    )
    .fetch_all(&pool)
    .await?;

    let metric_series: Vec<TravelMetricPoint> = sqlx::query_as(
        "SELECT captured_at, samples, od_pairs, confident_pairs, median_speed_kmh
           FROM learn_travel_metric
          WHERE captured_at > now() - interval '30 days'
          ORDER BY captured_at",
    )
    .fetch_all(&pool)
    .await?;

    let (samples, od_pairs, confident_pairs, confident_pairs_fullcode, median_speed_kmh): (i64, i64, i64, i64, Option<f64>) =
        sqlx::query_as(
            "SELECT count(*),
                    count(DISTINCT (origin_zone, dest_zone)) FILTER (WHERE origin_zone IS NOT NULL AND dest_zone IS NOT NULL),
                    (SELECT count(*) FROM (SELECT 1 FROM learn_travel_sample WHERE origin_zone IS NOT NULL AND dest_zone IS NOT NULL GROUP BY origin_zone, dest_zone HAVING count(*) >= 10) q),
                    (SELECT count(*) FROM (SELECT 1 FROM learn_travel_sample GROUP BY origin, dest HAVING count(*) >= 10) q),
                    percentile_cont(0.5) WITHIN GROUP (ORDER BY (dist_m/1000.0)/nullif(travel_s/3600.0,0))
                      FILTER (WHERE dist_m IS NOT NULL AND travel_s > 0)
               FROM learn_travel_sample",
        )
        .fetch_one(&pool)
        .await?;

    // weather signal: median travel in wet vs dry hours (joined by hour bucket of dropped_at).
    let (weather_wet_median_s, weather_dry_median_s): (Option<f64>, Option<f64>) = sqlx::query_as(
        "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY travel_s) FILTER (WHERE w.precip_mm > 0.1),
                percentile_cont(0.5) WITHIN GROUP (ORDER BY travel_s) FILTER (WHERE w.precip_mm <= 0.1)
           FROM learn_travel_sample s
           JOIN weather_hourly w ON w.ts = date_trunc('hour', s.dropped_at)
          WHERE s.travel_s BETWEEN 10 AND 3600",
    )
    .fetch_one(&pool)
    .await
    .unwrap_or((None, None));

    // predicted-vs-actual test: recent trips on a confident OD (n≥10), error vs the OD median.
    let accuracy: TravelAccuracy = sqlx::query_as(
        "WITH od AS (
           SELECT origin, dest, percentile_cont(0.5) WITHIN GROUP (ORDER BY travel_s) AS pred
             FROM learn_travel_sample WHERE travel_s BETWEEN 10 AND 3600
            GROUP BY origin, dest HAVING count(*) >= 10
         ),
         e AS (
           SELECT abs(s.travel_s - o.pred) AS abs_err,
                  abs(s.travel_s - o.pred) / nullif(s.travel_s, 0)::float8 AS ape
             FROM learn_travel_sample s JOIN od o ON o.origin = s.origin AND o.dest = s.dest
            WHERE s.travel_s BETWEEN 10 AND 3600 AND s.captured_at > now() - interval '2 days'
         )
         SELECT count(*) AS evaluated,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY ape) * 100)::float8 AS mape_pct,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY abs_err) AS median_abs_err_s,
                (avg(CASE WHEN ape <= 0.30 THEN 1.0 ELSE 0.0 END) * 100)::float8 AS within_30pct
           FROM e",
    )
    .fetch_one(&pool)
    .await
    .unwrap_or(TravelAccuracy { evaluated: 0, mape_pct: None, median_abs_err_s: None, within_30pct: None });

    Ok(Json(TravelResp {
        samples, od_pairs, confident_pairs, confident_pairs_fullcode,
        median_speed_kmh, weather_wet_median_s, weather_dry_median_s, accuracy, od, metric_series,
    }))
}

// ───────────────────────── soon-idle accuracy (④, shadow) ─────────────────────────
// Match soon_idle predictions (tt_soon_idle_pred) to the authoritative idle moment
// (tos_handover_label.comp_ts) and report precision / recall / lead-time, split by firing
// signal — isolating the TOS-correction hook's contribution via the gps_would_fire flag.
// Matching: DS = (ytno, container); LD = (ytno, time-window), nearest-Δt 1:1. Pure Postgres.

#[derive(Serialize, sqlx::FromRow)]
struct SiSource {
    jobtype: String,
    source: String,
    predictions: i64,
    matched: i64,
    precision_pct: Option<f64>,
    lead_p10_s: Option<f64>,
    lead_p50_s: Option<f64>,
    lead_p90_s: Option<f64>,
}

#[derive(Serialize, sqlx::FromRow)]
struct SiRecall {
    jobtype: String,
    truth_idles: i64,    // censored ground-truth labels (M)
    predicted_any: i64,  // covered by any soon_idle prediction
    predicted_gps: i64,  // covered by a GPS/PLC-alone prediction (counterfactual)
    recall_pct: Option<f64>,
    recall_gps_pct: Option<f64>, // GPS-only recall; (recall_pct − this) = TOS hook's gain
}

// per-jobtype lead time: of predicted-soon-idle trucks that DID go idle, how long after the
// prediction did they actually become idle (comp_ts − predicted_at). Answers "몇 분 후 유휴".
#[derive(Serialize, sqlx::FromRow)]
struct SiLead {
    jobtype: String,
    matched: i64,
    lead_p10_s: Option<f64>,
    lead_p50_s: Option<f64>,
    lead_p90_s: Option<f64>,
    // minutes-to-idle prediction test: predict the learned median (= lead_p50_s) for every truck of
    // this jobtype; how far off was the actual lead. mape = median |actual−pred|/actual.
    mape_pct: Option<f64>,
    within_30pct: Option<f64>, // % of trucks whose actual idle landed within ±30% of the prediction
}

#[derive(Serialize, sqlx::FromRow)]
struct SiMetricPoint {
    captured_at: DateTime<Utc>,
    jobtype: String,
    source: String,
    predictions: i32,
    matched: i32,
    precision_pct: Option<f64>,
    recall_pct: Option<f64>,
    lead_p50_s: Option<f64>,
}

// DS minutes-to-idle feature model: predict the median lead within (distance-bin × firing-signal)
// cells. dist_bin: 0=≤30m 1=30–80m 2=80–150m 3=>150m 4=RTG없음. pred_s = the cell's prediction.
#[derive(Serialize, sqlx::FromRow)]
struct DsEtaCell {
    dist_bin: i32,
    source: String,
    n: i64,
    pred_s: Option<f64>,
    p10_s: Option<f64>,
    p90_s: Option<f64>,
}
#[derive(Serialize, sqlx::FromRow)]
struct DsEtaModel {
    evaluated: i64,
    feat_mape_pct: Option<f64>, // error of the feature-binned prediction
    flat_mape_pct: Option<f64>, // error of the flat jobtype-median baseline (for comparison)
    within_30pct: Option<f64>,
}

#[derive(Serialize)]
pub struct SoonIdleResp {
    predictions: i64, // overall, last 7d
    matched: i64,
    precision_pct: Option<f64>,
    by_source: Vec<SiSource>,
    by_jobtype: Vec<SiRecall>,
    lead_by_jobtype: Vec<SiLead>,
    ds_eta: DsEtaModel,             // DS feature-model accuracy (distance × signal)
    ds_eta_cells: Vec<DsEtaCell>,   // the predictor: per-cell median lead
    metric_series: Vec<SiMetricPoint>,
}

/// GET /api/learn/soon-idle — soon_idle prediction accuracy vs authoritative idle (shadow).
pub async fn soon_idle(State(pool): State<PgPool>) -> Result<Json<SoonIdleResp>, AppError> {
    // forward match: each prediction → nearest comp_ts within [−60s, +20min]; precision + lead.
    let by_source: Vec<SiSource> = sqlx::query_as(
        "WITH m AS (
           SELECT p.jobtype, p.source, h.comp_ts,
                  EXTRACT(EPOCH FROM (h.comp_ts - p.predicted_at)) AS lead_s
             FROM tt_soon_idle_pred p
             LEFT JOIN LATERAL (
               SELECT comp_ts FROM tos_handover_label h
                WHERE h.ytno = p.ytno AND (p.jobtype <> 'DS' OR h.contno = p.container)
                  AND h.comp_ts >= p.predicted_at - interval '60 seconds'
                  AND h.comp_ts <  p.predicted_at + interval '20 minutes'
                ORDER BY abs(EXTRACT(EPOCH FROM (h.comp_ts - p.predicted_at))) LIMIT 1
             ) h ON true
            WHERE p.predicted_at > now() - interval '7 days'
         )
         SELECT jobtype, source, count(*) AS predictions, count(comp_ts) AS matched,
                (100.0*count(comp_ts)/nullif(count(*),0))::float8 AS precision_pct,
                percentile_cont(0.1) WITHIN GROUP (ORDER BY lead_s) FILTER (WHERE lead_s >= 0) AS lead_p10_s,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY lead_s) FILTER (WHERE lead_s >= 0) AS lead_p50_s,
                percentile_cont(0.9) WITHIN GROUP (ORDER BY lead_s) FILTER (WHERE lead_s >= 0) AS lead_p90_s
           FROM m GROUP BY jobtype, source ORDER BY jobtype, source",
    )
    .fetch_all(&pool)
    .await?;

    // reverse match over censored truth: recall (any signal) vs GPS-only counterfactual.
    let by_jobtype: Vec<SiRecall> = sqlx::query_as(
        "WITH truth AS (
           SELECT h.jobtype, h.ytno, h.contno, h.comp_ts
             FROM tos_handover_label h
            WHERE h.comp_ts > now() - interval '7 days'
              AND h.comp_ts < now() - interval '180 seconds'
              AND h.comp_ts > (SELECT min(predicted_at) FROM tt_soon_idle_pred) + interval '5 minutes'
         ), j AS (
           SELECT t.jobtype, p.id AS pid, p.gps_would_fire
             FROM truth t
             LEFT JOIN LATERAL (
               SELECT id, gps_would_fire FROM tt_soon_idle_pred p
                WHERE p.ytno = t.ytno AND (t.jobtype <> 'DS' OR p.container = t.contno)
                  AND p.predicted_at BETWEEN t.comp_ts - interval '60 minutes' AND t.comp_ts + interval '60 seconds'
                ORDER BY abs(EXTRACT(EPOCH FROM (t.comp_ts - p.predicted_at))) LIMIT 1
             ) p ON true
         )
         SELECT jobtype, count(*) AS truth_idles, count(pid) AS predicted_any,
                count(*) FILTER (WHERE gps_would_fire) AS predicted_gps,
                (100.0*count(pid)/nullif(count(*),0))::float8 AS recall_pct,
                (100.0*count(*) FILTER (WHERE gps_would_fire)/nullif(count(*),0))::float8 AS recall_gps_pct
           FROM j GROUP BY jobtype ORDER BY jobtype",
    )
    .fetch_all(&pool)
    .await?;

    // per-jobtype lead time over ALL matched predictions (every source), DS and LD alike.
    let lead_by_jobtype: Vec<SiLead> = sqlx::query_as(
        "WITH m AS (
           SELECT p.jobtype, EXTRACT(EPOCH FROM (h.comp_ts - p.predicted_at)) AS lead_s
             FROM tt_soon_idle_pred p
             JOIN LATERAL (
               SELECT comp_ts FROM tos_handover_label h
                WHERE h.ytno = p.ytno AND (p.jobtype <> 'DS' OR h.contno = p.container)
                  AND h.comp_ts >= p.predicted_at - interval '60 seconds'
                  AND h.comp_ts <  p.predicted_at + interval '20 minutes'
                ORDER BY abs(EXTRACT(EPOCH FROM (h.comp_ts - p.predicted_at))) LIMIT 1
             ) h ON true
            WHERE p.predicted_at > now() - interval '7 days'
         ),
         g AS ( -- per-jobtype predicted minutes-to-idle = the learned median lead
           SELECT jobtype, percentile_cont(0.5) WITHIN GROUP (ORDER BY lead_s) FILTER (WHERE lead_s >= 0) AS med
             FROM m GROUP BY jobtype
         )
         SELECT m.jobtype, count(*) FILTER (WHERE m.lead_s >= 0) AS matched,
                percentile_cont(0.1) WITHIN GROUP (ORDER BY m.lead_s) FILTER (WHERE m.lead_s >= 0) AS lead_p10_s,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY m.lead_s) FILTER (WHERE m.lead_s >= 0) AS lead_p50_s,
                percentile_cont(0.9) WITHIN GROUP (ORDER BY m.lead_s) FILTER (WHERE m.lead_s >= 0) AS lead_p90_s,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY abs(m.lead_s - g.med) / nullif(m.lead_s, 0))
                   FILTER (WHERE m.lead_s > 0) * 100)::float8 AS mape_pct,
                (avg(((abs(m.lead_s - g.med) / nullif(m.lead_s, 0)) <= 0.30)::int)
                   FILTER (WHERE m.lead_s > 0) * 100)::float8 AS within_30pct
           FROM m JOIN g USING (jobtype) GROUP BY m.jobtype ORDER BY m.jobtype",
    )
    .fetch_all(&pool)
    .await?;

    let metric_series: Vec<SiMetricPoint> = sqlx::query_as(
        "SELECT captured_at, jobtype, source, predictions, matched, precision_pct, recall_pct, lead_p50_s
           FROM tt_soon_idle_metric WHERE captured_at > now() - interval '30 days' ORDER BY captured_at",
    )
    .fetch_all(&pool)
    .await?;

    // DS minutes-to-idle feature predictor — the cells (distance-bin × signal → median lead).
    // dist_bin: 0=≤30m 1=30–80m 2=80–150m 3=>150m 4=RTG없음. Only cells with ≥20 matched shown.
    const DS_ETA_M: &str = "SELECT
           (CASE WHEN p.nearest_rtg_m IS NULL THEN 4 WHEN p.nearest_rtg_m <= 30 THEN 0
                 WHEN p.nearest_rtg_m <= 80 THEN 1 WHEN p.nearest_rtg_m <= 150 THEN 2 ELSE 3 END) AS dist_bin,
           p.source, EXTRACT(EPOCH FROM (h.comp_ts - p.predicted_at)) AS lead_s
         FROM tt_soon_idle_pred p
         JOIN LATERAL (
           SELECT comp_ts FROM tos_handover_label h
            WHERE h.ytno = p.ytno AND h.contno = p.container
              AND h.comp_ts >= p.predicted_at - interval '60 seconds'
              AND h.comp_ts <  p.predicted_at + interval '20 minutes'
            ORDER BY abs(EXTRACT(EPOCH FROM (h.comp_ts - p.predicted_at))) LIMIT 1
         ) h ON true
        WHERE p.jobtype = 'DS' AND p.predicted_at > now() - interval '7 days'";
    let ds_eta_cells: Vec<DsEtaCell> = sqlx::query_as(&format!(
        "WITH m AS ({DS_ETA_M})
         SELECT dist_bin, source, count(*) AS n,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY lead_s) AS pred_s,
                percentile_cont(0.1) WITHIN GROUP (ORDER BY lead_s) AS p10_s,
                percentile_cont(0.9) WITHIN GROUP (ORDER BY lead_s) AS p90_s
           FROM m WHERE lead_s >= 0 GROUP BY dist_bin, source HAVING count(*) >= 20 ORDER BY dist_bin, source"
    ))
    .fetch_all(&pool)
    .await
    .unwrap_or_default();
    // feature-model accuracy vs the flat baseline (both in-sample; held-out confirmed dist×source
    // doesn't overfit — large cells). feat = error vs cell median, flat = error vs jobtype median.
    let ds_eta: DsEtaModel = sqlx::query_as(&format!(
        "WITH m AS ({DS_ETA_M}),
              f AS (SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY lead_s) FILTER (WHERE lead_s >= 0) AS med FROM m),
              g AS (SELECT dist_bin, source, percentile_cont(0.5) WITHIN GROUP (ORDER BY lead_s) FILTER (WHERE lead_s >= 0) AS med FROM m GROUP BY dist_bin, source)
         SELECT count(*) FILTER (WHERE m.lead_s > 0) AS evaluated,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY abs(m.lead_s - g.med) / nullif(m.lead_s, 0)) FILTER (WHERE m.lead_s > 0) * 100)::float8 AS feat_mape_pct,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY abs(m.lead_s - f.med) / nullif(m.lead_s, 0)) FILTER (WHERE m.lead_s > 0) * 100)::float8 AS flat_mape_pct,
                (avg(((abs(m.lead_s - g.med) / nullif(m.lead_s, 0)) <= 0.30)::int) FILTER (WHERE m.lead_s > 0) * 100)::float8 AS within_30pct
           FROM m JOIN g USING (dist_bin, source) CROSS JOIN f"
    ))
    .fetch_one(&pool)
    .await
    .unwrap_or(DsEtaModel { evaluated: 0, feat_mape_pct: None, flat_mape_pct: None, within_30pct: None });

    let predictions: i64 = by_source.iter().map(|s| s.predictions).sum();
    let matched: i64 = by_source.iter().map(|s| s.matched).sum();
    let precision_pct = (predictions > 0).then(|| 100.0 * matched as f64 / predictions as f64);

    Ok(Json(SoonIdleResp {
        predictions,
        matched,
        precision_pct,
        by_source,
        by_jobtype,
        lead_by_jobtype,
        ds_eta,
        ds_eta_cells,
        metric_series,
    }))
}
