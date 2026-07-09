//! Learning center API. v1 model = block work-point coordinates (target ②): per topos code,
//! the learned (lat,lon) accumulated from TTs observed ARRIVED there (livemap centroids,
//! persisted by `spawn_learn_persist`). Exposes the model points, a summary, and a quality
//! time series so the dashboard can show accumulation + precision improving over time.
//! Pure Postgres reads.

use axum::extract::{Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
           FROM learn_topos_point WHERE topos NOT LIKE 'WHARF%' ORDER BY obs DESC LIMIT 1000",
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
           FROM learn_topos_point WHERE topos NOT LIKE 'WHARF%'",
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
        median_speed_kmh,
        weather_wet_median_s, weather_dry_median_s, accuracy, od, metric_series,
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
    // ── GPS-first ground truth ──────────────────────────────────────────────────────────────
    // Physical "truck freed" = the truck's own next GPS cycle-close (tt_cycle_v2.dropped_at), which
    // we cross-validated to within ~0.5min of the real free AND covers more trips than the TOS label.
    // Fall back to the TOS label only where GPS has a gap (LD = YT_DIS_DT; DS = comp_ts — for LD,
    // comp_ts='box on ship' lags the truck-free by ~8min so dis_ts is preferred). Used for the
    // forward match (precision + lead). Recall below stays on TOS authoritative completions: they
    // carry the container key DS needs, and GPS-by-(ytno,time) would inflate DS recall.
    const IDLE_JOINS: &str = "
       LEFT JOIN LATERAL (
         SELECT dropped_at FROM tt_cycle_v2 c
          WHERE c.ytno = p.ytno AND c.jobtype = p.jobtype
            AND c.dropped_at >= p.predicted_at - interval '90 seconds'
            AND c.dropped_at <  p.predicted_at + interval '20 minutes'
          ORDER BY c.dropped_at LIMIT 1
       ) g ON true
       LEFT JOIN LATERAL (
         SELECT dis_ts, comp_ts FROM tos_handover_label h
          WHERE h.ytno = p.ytno AND (p.jobtype <> 'DS' OR h.contno = p.container)
            AND h.comp_ts >= p.predicted_at - interval '60 seconds'
            AND h.comp_ts <  p.predicted_at + interval '20 minutes'
          ORDER BY abs(EXTRACT(EPOCH FROM (h.comp_ts - p.predicted_at))) LIMIT 1
       ) t ON true";
    const IDLE_EXPR: &str =
        "coalesce(g.dropped_at, CASE WHEN p.jobtype = 'LD' THEN coalesce(t.dis_ts, t.comp_ts) ELSE t.comp_ts END)";

    let by_source: Vec<SiSource> = sqlx::query_as(&format!(
        "WITH j AS (
           SELECT p.jobtype, p.source, p.predicted_at, {IDLE_EXPR} AS idle_ts
             FROM tt_soon_idle_pred p {IDLE_JOINS}
            WHERE p.predicted_at > now() - interval '7 days'
         ),
         m AS (SELECT jobtype, source, idle_ts, EXTRACT(EPOCH FROM (idle_ts - predicted_at)) AS lead_s FROM j)
         SELECT jobtype, source, count(*) AS predictions, count(idle_ts) AS matched,
                (100.0*count(idle_ts)/nullif(count(*),0))::float8 AS precision_pct,
                percentile_cont(0.1) WITHIN GROUP (ORDER BY lead_s) FILTER (WHERE lead_s >= 0) AS lead_p10_s,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY lead_s) FILTER (WHERE lead_s >= 0) AS lead_p50_s,
                percentile_cont(0.9) WITHIN GROUP (ORDER BY lead_s) FILTER (WHERE lead_s >= 0) AS lead_p90_s
           FROM m GROUP BY jobtype, source ORDER BY jobtype, source"
    ))
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

    // per-jobtype lead time (GPS-first idle) over ALL matched predictions. minutes-to-idle prediction
    // = the learned median lead (gm.med); mape/within_30 measure that prediction vs actual.
    let lead_by_jobtype: Vec<SiLead> = sqlx::query_as(&format!(
        "WITH j AS (
           SELECT p.jobtype, p.predicted_at, {IDLE_EXPR} AS idle_ts
             FROM tt_soon_idle_pred p {IDLE_JOINS}
            WHERE p.predicted_at > now() - interval '7 days'
         ),
         m AS (SELECT jobtype, EXTRACT(EPOCH FROM (idle_ts - predicted_at)) AS lead_s FROM j),
         gm AS (SELECT jobtype, percentile_cont(0.5) WITHIN GROUP (ORDER BY lead_s) FILTER (WHERE lead_s >= 0) AS med FROM m GROUP BY jobtype)
         SELECT m.jobtype, count(*) FILTER (WHERE m.lead_s >= 0) AS matched,
                percentile_cont(0.1) WITHIN GROUP (ORDER BY m.lead_s) FILTER (WHERE m.lead_s >= 0) AS lead_p10_s,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY m.lead_s) FILTER (WHERE m.lead_s >= 0) AS lead_p50_s,
                percentile_cont(0.9) WITHIN GROUP (ORDER BY m.lead_s) FILTER (WHERE m.lead_s >= 0) AS lead_p90_s,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY abs(m.lead_s - gm.med) / nullif(m.lead_s, 0))
                   FILTER (WHERE m.lead_s > 0) * 100)::float8 AS mape_pct,
                (avg(((abs(m.lead_s - gm.med) / nullif(m.lead_s, 0)) <= 0.30)::int)
                   FILTER (WHERE m.lead_s > 0) * 100)::float8 AS within_30pct
           FROM m JOIN gm USING (jobtype) GROUP BY m.jobtype ORDER BY m.jobtype"
    ))
    .fetch_all(&pool)
    .await?;

    let metric_series: Vec<SiMetricPoint> = sqlx::query_as(
        "SELECT captured_at, jobtype, source, predictions, matched, precision_pct, recall_pct, lead_p50_s
           FROM tt_soon_idle_metric WHERE captured_at > now() - interval '30 days' ORDER BY captured_at",
    )
    .fetch_all(&pool)
    .await?;

    // DS minutes-to-idle feature predictor — the cells (distance-bin × signal → median lead), using
    // the same GPS-first idle. dist_bin: 0=≤30m 1=30–80m 2=80–150m 3=>150m 4=RTG없음. ≥20 matched.
    let ds_eta_m = format!(
        "SELECT
           (CASE WHEN p.nearest_rtg_m IS NULL THEN 4 WHEN p.nearest_rtg_m <= 30 THEN 0
                 WHEN p.nearest_rtg_m <= 80 THEN 1 WHEN p.nearest_rtg_m <= 150 THEN 2 ELSE 3 END) AS dist_bin,
           p.source, EXTRACT(EPOCH FROM ({IDLE_EXPR} - p.predicted_at)) AS lead_s
         FROM tt_soon_idle_pred p {IDLE_JOINS}
        WHERE p.jobtype = 'DS' AND p.predicted_at > now() - interval '7 days'"
    );
    let ds_eta_cells: Vec<DsEtaCell> = sqlx::query_as(&format!(
        "WITH m AS ({ds_eta_m})
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
        "WITH m AS ({ds_eta_m}),
              f AS (SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY lead_s) FILTER (WHERE lead_s >= 0) AS med FROM m),
              cg AS (SELECT dist_bin, source, percentile_cont(0.5) WITHIN GROUP (ORDER BY lead_s) FILTER (WHERE lead_s >= 0) AS med FROM m GROUP BY dist_bin, source)
         SELECT count(*) FILTER (WHERE m.lead_s > 0) AS evaluated,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY abs(m.lead_s - cg.med) / nullif(m.lead_s, 0)) FILTER (WHERE m.lead_s > 0) * 100)::float8 AS feat_mape_pct,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY abs(m.lead_s - f.med) / nullif(m.lead_s, 0)) FILTER (WHERE m.lead_s > 0) * 100)::float8 AS flat_mape_pct,
                (avg(((abs(m.lead_s - cg.med) / nullif(m.lead_s, 0)) <= 0.30)::int) FILTER (WHERE m.lead_s > 0) * 100)::float8 AS within_30pct
           FROM m JOIN cg USING (dist_bin, source) CROSS JOIN f"
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

// ── Stage-1 dispatch work-time prediction (shadow validation) ─────────────────────────────────
// The deadline-aware engine predicts WHEN each crane will work each container (work-ETA → dispatch
// deadline). We log every prediction and, when the container is actually worked, the truth: DS via
// the exact crane-discharge time, LD via pool-leave (lagged). This card shows the model's
// accumulating validated sample base (learning) + the near-term (dispatch-relevant, lead<20min)
// accuracy of predicted-vs-actual work time (test). DS is the clean ground truth; LD lags a few min.
#[derive(sqlx::FromRow)]
struct DpDay { n: i64 }
#[derive(sqlx::FromRow)]
struct DpTest {
    ds_eval: i64,
    ds_med_err_min: Option<f64>,
    ds_within10_pct: Option<f64>,
    ld_eval: i64,
    ld_med_err_min: Option<f64>,
}
#[derive(sqlx::FromRow)]
struct DpTot { resolved_total: i64, distinct_cont: i64 }

#[derive(Serialize)]
pub struct DispatchPredResp {
    samples: Vec<i64>,        // cumulative validated (resolved) predictions per day
    resolved_total: i64,
    distinct_cont: i64,
    ds_eval: i64,
    ds_med_err_min: Option<f64>,
    ds_within10_pct: Option<f64>,
    ld_eval: i64,
    ld_med_err_min: Option<f64>,
}

pub async fn dispatch_pred(State(pool): State<PgPool>) -> Result<Json<DispatchPredResp>, AppError> {
    // learning series: daily resolved-prediction count → cumulated (validation base grows)
    let days: Vec<DpDay> = sqlx::query_as(
        "SELECT count(*)::int8 AS n FROM dispatch_pred_sample
          WHERE resolved_at IS NOT NULL AND logged_at > now() - interval '14 days'
          GROUP BY logged_at::date ORDER BY logged_at::date",
    )
    .fetch_all(&pool)
    .await?;
    let mut cum = 0i64;
    let samples: Vec<i64> = days.iter().map(|d| { cum += d.n; cum }).collect();

    // test: near-term (lead < 20min, dispatch-relevant) accuracy of predicted vs actual work time
    let test: DpTest = sqlx::query_as(
        "WITH z AS (
           SELECT jobtype,
             extract(epoch FROM (resolved_at - pred_work_eta_ts))/60 AS err,
             (pred_work_eta_ts - logged_at) AS lead_iv
           FROM dispatch_pred_sample
           WHERE resolved_at IS NOT NULL AND resolved_at >= logged_at AND pred_work_eta_ts IS NOT NULL
             AND logged_at > now() - interval '2 days'
         ), n AS (SELECT * FROM z WHERE lead_iv < interval '20 minutes')
         SELECT
           count(*) FILTER (WHERE jobtype='DS')::int8 AS ds_eval,
           (percentile_cont(0.5) WITHIN GROUP (ORDER BY err) FILTER (WHERE jobtype='DS'))::float8 AS ds_med_err_min,
           (100.0*count(*) FILTER (WHERE jobtype='DS' AND abs(err)<=10)
             / nullif(count(*) FILTER (WHERE jobtype='DS'),0))::float8 AS ds_within10_pct,
           count(*) FILTER (WHERE jobtype='LD')::int8 AS ld_eval,
           (percentile_cont(0.5) WITHIN GROUP (ORDER BY err) FILTER (WHERE jobtype='LD'))::float8 AS ld_med_err_min
         FROM n",
    )
    .fetch_one(&pool)
    .await?;

    let tot: DpTot = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE resolved_at IS NOT NULL)::int8 AS resolved_total,
                count(DISTINCT contno)::int8 AS distinct_cont
           FROM dispatch_pred_sample",
    )
    .fetch_one(&pool)
    .await?;

    Ok(Json(DispatchPredResp {
        samples,
        resolved_total: tot.resolved_total,
        distinct_cont: tot.distinct_cont,
        ds_eval: test.ds_eval,
        ds_med_err_min: test.ds_med_err_min,
        ds_within10_pct: test.ds_within10_pct,
        ld_eval: test.ld_eval,
        ld_med_err_min: test.ld_med_err_min,
    }))
}

// ───────────────────────── data-collection catalog (데이터 수집 탭) ─────────────────────────
// Whitelist of the streams we actively collect. (key, table, recency-ts-column, sample SELECT).
// `table`/`ts`/`sample_sql` are compile-time constants — never request input — so the formatted SQL
// carries no injection surface; the only request param (`key`) is matched against this list. The
// frontend owns the prose (source / usage / description); here we serve just the live numbers + rows.

struct DataStream {
    key: &'static str,
    table: &'static str,
    ts: &'static str, // column used for recency counts + "latest collected"
    sample_sql: &'static str,
}

const DATA_STREAMS: &[DataStream] = &[
    DataStream { key: "truck_pos_hifreq", table: "truck_pos_hifreq", ts: "ts",
        sample_sql: "SELECT ts, ytno, round(lat::numeric,5) AS lat, round(lon::numeric,5) AS lon \
                     FROM truck_pos_hifreq ORDER BY ts DESC LIMIT 50" },
    DataStream { key: "truck_pos_hist", table: "truck_pos_hist", ts: "ts",
        sample_sql: "SELECT ts, ytno, round(lat::numeric,5) AS lat, round(lon::numeric,5) AS lon, state \
                     FROM truck_pos_hist ORDER BY ts DESC LIMIT 50" },
    DataStream { key: "tt_cycle_v2", table: "tt_cycle_v2", ts: "dropped_at",
        sample_sql: "SELECT dropped_at, ytno, jobtype, opened_at, empty_arrived_at, laden_arrived_at, legs \
                     FROM tt_cycle_v2 ORDER BY dropped_at DESC LIMIT 50" },
    DataStream { key: "learn_travel_sample", table: "learn_travel_sample", ts: "captured_at",
        sample_sql: "SELECT captured_at, ytno, origin_zone, dest_zone, travel_s, round(dist_m::numeric,0) AS dist_m, shift \
                     FROM learn_travel_sample ORDER BY captured_at DESC LIMIT 50" },
    DataStream { key: "tt_soon_idle_pred", table: "tt_soon_idle_pred", ts: "predicted_at",
        sample_sql: "SELECT predicted_at, ytno, jobtype, qc, source, gps_would_fire, \
                     round(nearest_rtg_m::numeric,0) AS nearest_rtg_m, reason \
                     FROM tt_soon_idle_pred ORDER BY predicted_at DESC LIMIT 50" },
    DataStream { key: "dispatch_pred_sample", table: "dispatch_pred_sample", ts: "logged_at",
        sample_sql: "SELECT logged_at, qc, vessel, contno, jobtype, pred_work_eta_ts, dispatch_deadline_ts, \
                     slack_s, resolved_at FROM dispatch_pred_sample ORDER BY logged_at DESC LIMIT 50" },
    DataStream { key: "free_in_sample", table: "free_in_sample", ts: "ts",
        sample_sql: "SELECT ts, ytno, state, jobtype, qc, secs_in_cycle, round(nearest_rtg_m::numeric,0) AS nearest_rtg_m, \
                     pred_free_in_s, soon_idle, actual_remaining_s FROM free_in_sample ORDER BY ts DESC LIMIT 50" },
    DataStream { key: "stage2_match_shadow", table: "stage2_match_shadow", ts: "ts",
        sample_sql: "SELECT ts, ytno, qc, vessel, jobtype, arrival_s, deadline_slack_s, cost_tier, switched \
                     FROM stage2_match_shadow ORDER BY ts DESC LIMIT 50" },
    DataStream { key: "dispatch_compare_shadow", table: "dispatch_compare_shadow", ts: "ts",
        sample_sql: "SELECT ts, qc, jobtype, tos_ytno, tos_arrival_s, our_ytno, our_arrival_s, agree, delta_s \
                     FROM dispatch_compare_shadow ORDER BY ts DESC LIMIT 50" },
    DataStream { key: "fair_compare_detail", table: "fair_compare_detail", ts: "ts",
        sample_sql: "SELECT ts, jobtype, qc, tos_s, our_s, (tos_s - our_s) AS save_s \
                     FROM fair_compare_detail ORDER BY ts DESC LIMIT 50" },
    DataStream { key: "congestion_edge", table: "congestion_edge", ts: "hour",
        sample_sql: "SELECT hour, round(mlat::numeric,5) AS mlat, round(mlon::numeric,5) AS mlon, \
                     round(med_speed_kmh::numeric,1) AS med_speed_kmh, n, round(len_m::numeric,0) AS len_m \
                     FROM congestion_edge ORDER BY hour DESC, n DESC LIMIT 50" },
    DataStream { key: "qc_wait_sample", table: "qc_wait_sample", ts: "ts",
        sample_sql: "SELECT ts, working_qc, starving_real, wait_real_s, starving_gps, wait_gps_s, pos_known_qc \
                     FROM qc_wait_sample ORDER BY ts DESC LIMIT 50" },
    DataStream { key: "weather_hourly", table: "weather_hourly", ts: "ts",
        sample_sql: "SELECT ts, precip_mm, wind_kmh, visibility_m, weather_code, captured_at \
                     FROM weather_hourly ORDER BY ts DESC LIMIT 50" },
    DataStream { key: "tos_handover_label", table: "tos_handover_label", ts: "captured_at",
        sample_sql: "SELECT captured_at, contno, ytno, jobtype, topos, dis_ts, comp_ts \
                     FROM tos_handover_label ORDER BY captured_at DESC LIMIT 50" },
    DataStream { key: "learn_qc_move_time", table: "learn_qc_move_time", ts: "as_of_ts",
        sample_sql: "SELECT qc, jobtype, shift, med_sec, n, as_of_ts \
                     FROM learn_qc_move_time ORDER BY as_of_ts DESC, n DESC LIMIT 50" },
];

#[derive(Serialize, sqlx::FromRow)]
pub struct DataStat {
    key: String,
    total: i64,   // pg_class.reltuples estimate (fast; autovacuum keeps it close)
    n_1h: i64,    // exact, last hour
    n_24h: i64,   // exact, last 24h
    latest: Option<DateTime<Utc>>,
}

/// GET /api/learn/data-catalog — per-stream collection volumes (total + recent) and latest timestamp.
pub async fn data_catalog(State(pool): State<PgPool>) -> Result<Json<Vec<DataStat>>, AppError> {
    let parts: Vec<String> = DATA_STREAMS
        .iter()
        .map(|s| {
            format!(
                "SELECT '{k}' AS key,
                        GREATEST((SELECT reltuples::bigint FROM pg_class WHERE relname='{t}' AND relkind='r' LIMIT 1), 0) AS total,
                        (SELECT count(*) FROM {t} WHERE {ts} > now() - interval '1 hour') AS n_1h,
                        (SELECT count(*) FROM {t} WHERE {ts} > now() - interval '24 hours') AS n_24h,
                        (SELECT max({ts})::timestamptz FROM {t}) AS latest",
                k = s.key, t = s.table, ts = s.ts
            )
        })
        .collect();
    let sql = parts.join("\nUNION ALL\n");
    let rows: Vec<DataStat> = sqlx::query_as(&sql).fetch_all(&pool).await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct SampleQ {
    key: String,
}

/// GET /api/learn/data-sample?key=… — recent rows of one whitelisted stream. Returns the raw JSON
/// array straight from Postgres `json_agg` (column order preserved — note serde_json::Value would
/// re-sort keys alphabetically via its BTreeMap, so we pass the bytes through untouched). Unknown
/// key → empty array.
pub async fn data_sample(
    State(pool): State<PgPool>,
    Query(q): Query<SampleQ>,
) -> Result<axum::response::Response, AppError> {
    use axum::http::header;
    use axum::response::IntoResponse;
    let json_ct = [(header::CONTENT_TYPE, "application/json")];
    let Some(s) = DATA_STREAMS.iter().find(|s| s.key == q.key) else {
        return Ok((json_ct, "[]").into_response());
    };
    let sql = format!("SELECT coalesce(json_agg(t), '[]'::json)::text FROM ({}) t", s.sample_sql);
    let txt: String = sqlx::query_scalar(&sql).fetch_one(&pool).await?;
    Ok((json_ct, txt).into_response())
}

// ─────────────────── extra: live stats for the newer models (map-match, cycle, stage-2, QC) ───────────────────
#[derive(Serialize, sqlx::FromRow)]
pub struct FreeInStage {
    state: String,
    jobtype: String,
    n: i32,
    med_rem_s: i32, // learned median seconds-to-free for a truck observed in this stage
}

#[derive(Serialize)]
pub struct ExtraResp {
    // live map-match arrival shadow (mm_arrival_shadow, block legs, 24h)
    mm_legs: i64,
    mm_saw_pct: Option<f64>,
    mm_missed: i64,
    mm_recoverable: i64, // missed by geofence/ARRIVED but route-progress reached the end
    mm_avg_prog: Option<f64>,
    // cycle decomposition capture (tt_cycle_v2, last 3d)
    cyc_n: i64,
    cyc_empty_miss_pct: Option<f64>,
    cyc_laden_miss_pct: Option<f64>,
    cyc_pickdone_pct: Option<f64>, // pickup_left backed by crane ground truth
    // QC work-point (learn_topos_point cranes)
    qc_total: i64,
    qc_projected: i64, // cranes resolved to the swing-free quay-line work-point (spread≈15)
    // stage-2 dispatch matching shadow (24h)
    s2_rows: i64,
    s2_feasible_pct: Option<f64>,
    s2_switched: i64,
    s2_gap_pct: Option<f64>, // greedy vs optimal cost gap
    // ⑤ soon-idle gate self-cal (learn_soon_idle_gate) + ⑥ free-in residual (learn_free_in_bias)
    si_gate_m: Option<f64>,       // learned DS RTG-distance cutoff (m); default 50
    si_gate_prec: Option<i32>,    // precision (%) held at that gate
    si_gate_n: i64,               // total observations the gate was learned from (fired ∪ near-miss)
    si_gate_nearmiss_n: i64,      // near-miss (>gate) observations — enables loosening
    fi_stages: Vec<FreeInStage>, // ⑤⑥ learned median seconds-to-free per cycle stage (rollup rows)
}

/// GET /api/learn/extra — one-shot live stats for the models that lack a dedicated endpoint.
pub async fn extra(State(pool): State<PgPool>) -> Result<Json<ExtraResp>, AppError> {
    let (mm_legs, mm_saw_pct, mm_missed, mm_recoverable, mm_avg_prog): (i64, Option<f64>, i64, i64, Option<f64>) =
        sqlx::query_as(
            "SELECT count(*),
                    (100.0*avg(saw_arrived::int))::float8,
                    count(*) FILTER (WHERE NOT saw_arrived AND (min_dest_m<0 OR min_dest_m>70)),
                    count(*) FILTER (WHERE NOT saw_arrived AND (min_dest_m<0 OR min_dest_m>70) AND progress_frac>=0.9),
                    avg(progress_frac)::float8
               FROM mm_arrival_shadow
              WHERE NOT is_crane AND leg_dur_s>=30 AND logged_at > now()-interval '24 hours'",
        )
        .fetch_one(&pool)
        .await?;
    let (cyc_n, cyc_empty_miss_pct, cyc_laden_miss_pct, cyc_pickdone_pct): (i64, Option<f64>, Option<f64>, Option<f64>) =
        sqlx::query_as(
            "SELECT count(*),
                    (100.0*avg((empty_arrived_at IS NULL)::int))::float8,
                    (100.0*avg((laden_arrived_at IS NULL)::int))::float8,
                    (100.0*avg((pickup_done_at IS NOT NULL)::int))::float8
               FROM tt_cycle_v2 WHERE dropped_at > now()-interval '3 days'",
        )
        .fetch_one(&pool)
        .await?;
    let (qc_total, qc_projected): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE spread_m <= 16) FROM learn_topos_point WHERE is_crane",
    )
    .fetch_one(&pool)
    .await?;
    let (s2_rows, s2_feasible_pct, s2_switched): (i64, Option<f64>, i64) = sqlx::query_as(
        "SELECT count(*), (100.0*avg(feasible::int))::float8, count(*) FILTER (WHERE switched)
           FROM stage2_match_shadow WHERE ts > now()-interval '24 hours'",
    )
    .fetch_one(&pool)
    .await?;
    let s2_gap_pct: Option<f64> = sqlx::query_scalar(
        "SELECT avg(gap_pct)::float8 FROM stage2_solver_shadow WHERE ts > now()-interval '24 hours'",
    )
    .fetch_one(&pool)
    .await?;
    let gate: Option<(f32, i32, i32, i32)> = sqlx::query_as(
        "SELECT gate_m, prec_pct, n, nearmiss_n FROM learn_soon_idle_gate WHERE jobtype = 'DS'",
    )
    .fetch_optional(&pool)
    .await?;
    let (si_gate_m, si_gate_prec, si_gate_n, si_gate_nearmiss_n) = gate
        .map(|(g, p, n, nm)| (Some(g as f64), Some(p), n as i64, nm as i64))
        .unwrap_or((None, None, 0, 0));
    let fi_stages: Vec<FreeInStage> = sqlx::query_as(
        "SELECT state, jobtype, n, med_rem_s FROM learn_free_in_bias
          WHERE dist_bin = -99 AND med_rem_s IS NOT NULL
          ORDER BY array_position(ARRAY['delivering','approaching','wait_rtg','soon_idle'], state), jobtype",
    )
    .fetch_all(&pool)
    .await
    .unwrap_or_default();
    Ok(Json(ExtraResp {
        mm_legs, mm_saw_pct, mm_missed, mm_recoverable, mm_avg_prog,
        cyc_n, cyc_empty_miss_pct, cyc_laden_miss_pct, cyc_pickdone_pct,
        qc_total, qc_projected, s2_rows, s2_feasible_pct, s2_switched, s2_gap_pct,
        si_gate_m, si_gate_prec, si_gate_n, si_gate_nearmiss_n, fi_stages,
    }))
}
