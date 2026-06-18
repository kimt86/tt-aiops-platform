//! Read-only HTTP handlers over the L1/L2 tables.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::PgPool;
use wp_core::kpi::KpiKey;

use crate::db;
use crate::models::*;
use crate::{agg, periods};

/// Anything that goes wrong becomes a 500 with a short message.
pub struct AppError(anyhow::Error);
impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        AppError(e.into())
    }
}
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!(error = %self.0, "request failed");
        (StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()).into_response()
    }
}

#[derive(Deserialize)]
pub struct AsOfQuery {
    as_of: Option<String>,
}

#[derive(Deserialize)]
pub struct TrendQuery {
    days: Option<i64>,
    from: Option<String>,
    to: Option<String>,
}

async fn resolve_as_of(pool: &PgPool, given: Option<String>) -> Result<Option<NaiveDate>, AppError> {
    match given {
        Some(s) => Ok(Some(NaiveDate::parse_from_str(&s, "%Y-%m-%d").map_err(anyhow::Error::from)?)),
        None => Ok(db::latest_as_of(pool).await?),
    }
}

// Display order (KPI cards on the dashboard): empty dist → empty ratio → TT cycle
// → TT utilization → QC wait → QC productivity (MPH).
// K_RTG_Q (yard handover wait) is hidden for now — still extracted, just not shown.
pub(crate) const ORDER: [KpiKey; 6] = [
    KpiKey::KEmpty,
    KpiKey::KEmptyR,
    KpiKey::KCycle,
    KpiKey::KUtil,
    KpiKey::KQcNomove,
    KpiKey::KMph,
];

#[derive(Deserialize)]
pub struct PeriodQuery {
    period: Option<String>,
}

pub(crate) struct Target {
    pub target: Option<f64>,
    pub excellent: Option<f64>,
    pub direction: Option<String>,
    pub tier: Option<String>,
}

pub(crate) async fn load_targets(pool: &PgPool) -> Result<std::collections::HashMap<String, Target>, AppError> {
    let rows: Vec<(String, Option<f64>, Option<f64>, Option<String>, Option<String>)> =
        sqlx::query_as(
            "SELECT kpi_key, target_value::float8, excellent_value::float8, direction, tier FROM kpi_target",
        )
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(k, t, e, d, ti)| (k, Target { target: t, excellent: e, direction: d, tier: ti }))
        .collect())
}

/// Per-jobtype TT cycle (seconds) over [from,to] — samples-weighted median of discharge (DS)
/// and load (LD) cycles, for the cycle card breakdown.
pub(crate) async fn cycle_by_jobtype(
    pool: &PgPool,
    from: chrono::NaiveDate,
    to: chrono::NaiveDate,
) -> anyhow::Result<(Option<f64>, Option<f64>)> {
    let row: Option<(Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT round(sum(ds_med_sec*ds_samples)/nullif(sum(ds_samples),0), 1)::float8,
                round(sum(ld_med_sec*ld_samples)/nullif(sum(ld_samples),0), 1)::float8
           FROM raw_k_tt_cycle WHERE snapshot_date BETWEEN $1 AND $2",
    )
    .bind(from)
    .bind(to)
    .fetch_optional(pool)
    .await?;
    Ok(row.unwrap_or((None, None)))
}

/// Headline KPIs for the selected calendar period, with delta vs the immediately
/// preceding equal period and a Welch significance test over the daily series.
pub async fn kpis(
    State(pool): State<PgPool>,
    Query(q): Query<PeriodQuery>,
) -> Result<Json<KpisResponse>, AppError> {
    // Period boundaries follow the TERMINAL operational day (MYT, UTC+8), not the
    // server clock (KST, UTC+9). Using server-local date flips "today"/"yesterday"
    // an hour early (KST midnight = terminal 23:00), showing a blank "today" for the
    // last operating hour. Same rule as the LIVE shift detection.
    let today = wp_core::shift::terminal_now().date_naive();
    let r = periods::resolve(q.period.as_deref().unwrap_or("yesterday"), today);
    let cur = agg::aggregate(&pool, r.cur.from, r.cur.to).await?;
    let prev = agg::aggregate(&pool, r.prev.from, r.prev.to).await?;
    let targets = load_targets(&pool).await?;
    let provisional = periods::includes_today(&r.cur, today);

    let mut cards = Vec::new();
    for kpi in ORDER {
        let key = kpi.as_str();
        let (value, sample_n) = cur.get(key).copied().unwrap_or((None, None));
        let (base, _) = prev.get(key).copied().unwrap_or((None, None));

        let cs = agg::daily_series(&pool, key, r.cur.from, r.cur.to).await?;
        let ps = agg::daily_series(&pool, key, r.prev.from, r.prev.to).await?;
        let test = wp_core::stats::welch_t_test(&cs, &ps);

        let (delta_abs, delta_pct) = match (value, base) {
            (Some(v), Some(b)) if b != 0.0 => (Some(v - b), Some((v - b) / b * 100.0)),
            (Some(v), Some(b)) => (Some(v - b), None),
            _ => (None, None),
        };
        let tgt = targets.get(key);
        let dir = tgt.and_then(|t| t.direction.as_deref());
        let meets = |thr: Option<f64>| match (value, thr, dir) {
            (Some(v), Some(th), Some(d)) => Some(if d == "LOWER_BETTER" { v <= th } else { v >= th }),
            _ => None,
        };
        // per-jobtype cycle (DS/LD) for the cycle card only
        let (ds_cycle_s, ld_cycle_s) = if key == "K_CYCLE" {
            cycle_by_jobtype(&pool, r.cur.from, r.cur.to).await?
        } else {
            (None, None)
        };

        cards.push(KpiCard {
            key: key.to_string(),
            name_en: kpi.name_en().to_string(),
            name_ko: kpi.name_ko().to_string(),
            unit: kpi.unit().to_string(),
            tier: tgt.and_then(|t| t.tier.clone()),
            direction: tgt.and_then(|t| t.direction.clone()),
            value,
            sample_n: sample_n.map(|n| n as i32),
            is_provisional: provisional,
            as_of: r.cur.to.to_string(),
            baseline: base,
            baseline_n_days: Some(ps.len() as i32),
            delta_abs,
            delta_pct,
            p_value: test.as_ref().map(|t| t.p_value),
            cohens_d: test.as_ref().map(|t| t.cohens_d),
            is_significant: test.as_ref().map(|t| t.p_value < 0.05),
            target: tgt.and_then(|t| t.target),
            excellent: tgt.and_then(|t| t.excellent),
            meets_target: meets(tgt.and_then(|t| t.target)),
            meets_excellent: meets(tgt.and_then(|t| t.excellent)),
            ds_cycle_s,
            ld_cycle_s,
        });
    }

    Ok(Json(KpisResponse {
        as_of: r.cur.to.to_string(),
        period: r.period,
        range_from: r.cur.from.to_string(),
        range_to: r.cur.to.to_string(),
        prev_from: r.prev.from.to_string(),
        prev_to: r.prev.to.to_string(),
        kpis: cards,
    }))
}

pub async fn trend(
    State(pool): State<PgPool>,
    Path(key): Path<String>,
    Query(q): Query<TrendQuery>,
) -> Result<Json<TrendResponse>, AppError> {
    // accept the headline KPIs plus the trend-only per-jobtype cycle keys (K_CYCLE_DS/LD).
    let unit: &str = match KpiKey::from_str(&key) {
        Some(k) => k.unit(),
        None if key == "K_CYCLE_DS" || key == "K_CYCLE_LD" => "s",
        None => return Err(AppError(anyhow::anyhow!("unknown kpi key"))),
    };
    // Range mode: the sparkline follows the selected period (from..=to). Days mode:
    // the most-recent N days. Range keeps the chart consistent with the period cards.
    let raw: Vec<(NaiveDate, f64, Option<i32>)> = match (q.from.as_deref(), q.to.as_deref()) {
        (Some(f), Some(t)) => {
            let from = NaiveDate::parse_from_str(f, "%Y-%m-%d").map_err(anyhow::Error::from)?;
            let to = NaiveDate::parse_from_str(t, "%Y-%m-%d").map_err(anyhow::Error::from)?;
            sqlx::query_as(
                "SELECT snapshot_date, value::float8, sample_n FROM kpi_daily
                  WHERE kpi_key = $1 AND snapshot_date BETWEEN $2 AND $3
                  ORDER BY snapshot_date",
            )
            .bind(&key).bind(from).bind(to)
            .fetch_all(&pool).await?
        }
        _ => {
            let days = q.days.unwrap_or(14).clamp(1, 120);
            let mut rows: Vec<(NaiveDate, f64, Option<i32>)> = sqlx::query_as(
                "SELECT snapshot_date, value::float8, sample_n FROM kpi_daily
                  WHERE kpi_key = $1
                  ORDER BY snapshot_date DESC LIMIT $2",
            )
            .bind(&key).bind(days)
            .fetch_all(&pool).await?;
            rows.reverse(); // chronological for the chart
            rows
        }
    };
    let mut points: Vec<TrendPoint> = raw
        .into_iter()
        .map(|(d, v, n)| TrendPoint { date: d.to_string(), value: v, sample_n: n })
        .collect();
    points.sort_by(|a, b| a.date.cmp(&b.date));

    let tb: Option<(Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT t.target_value::float8,
                (SELECT baseline_value::float8 FROM kpi_baseline
                  WHERE kpi_key=$1 ORDER BY as_of_date DESC LIMIT 1)
           FROM kpi_target t WHERE t.kpi_key = $1",
    )
    .bind(&key)
    .fetch_optional(&pool)
    .await?;
    let (target, baseline) = tb.unwrap_or((None, None));

    Ok(Json(TrendResponse {
        key: key.clone(),
        unit: unit.to_string(),
        target,
        baseline,
        points,
    }))
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    gran: Option<String>,
    n: Option<i64>,
}

/// KPI values bucketed by day / week / month (newest-first). Reads ONLY Postgres —
/// `kpi_daily` for day (one query, same source as `trend`); `agg::aggregate` over
/// `raw_*`/shift for week/month (exact incl. K_UTIL + provisional today). Zero Oracle load.
pub async fn kpi_history(
    State(pool): State<PgPool>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<HistoryResponse>, AppError> {
    let today = wp_core::shift::terminal_now().date_naive();
    let gran = match q.gran.as_deref().unwrap_or("day") {
        "week" => "week",
        "month" => "month",
        _ => "day",
    };
    let (cap, default_n) = match gran {
        "day" => (60_i64, 14),
        "week" => (26, 12),
        _ => (12, 12),
    };
    let n = q.n.unwrap_or(default_n).clamp(1, cap) as usize;

    let targets = load_targets(&pool).await?;
    // trend columns = the 6 headline KPIs + the two per-jobtype cycle metrics (trend-only).
    let extra = [
        ("K_CYCLE_DS", "TT Cycle (Discharge)", "TT 사이클 (양하)", "s"),
        ("K_CYCLE_LD", "TT Cycle (Load)", "TT 사이클 (적하)", "s"),
    ];
    let keys: Vec<(String, String, String, String, Option<String>)> = ORDER
        .iter()
        .map(|kpi| (
            kpi.as_str().to_string(), kpi.name_en().to_string(), kpi.name_ko().to_string(),
            kpi.unit().to_string(), targets.get(kpi.as_str()).and_then(|t| t.direction.clone()),
        ))
        .chain(extra.iter().map(|(k, en, ko, u)| (
            k.to_string(), en.to_string(), ko.to_string(), u.to_string(), Some("LOWER_BETTER".to_string()),
        )))
        .collect();
    let columns: Vec<HistoryColumn> = keys
        .iter()
        .map(|(key, en, ko, u, d)| HistoryColumn {
            key: key.clone(), name_en: en.clone(), name_ko: ko.clone(), unit: u.clone(), direction: d.clone(),
        })
        .collect();

    let ranges = match gran {
        "week" => periods::week_buckets(today, n),
        "month" => periods::month_buckets(today, n),
        _ => periods::day_buckets(today, n),
    };

    let mut buckets = Vec::with_capacity(ranges.len());

    if gran == "day" {
        // one kpi_daily query for the whole window, then pivot per day (matches `trend`).
        let oldest = ranges.last().map(|r| r.from).unwrap_or(today);
        let rows: Vec<(String, NaiveDate, f64, Option<i32>)> = sqlx::query_as(
            "SELECT kpi_key, snapshot_date, value::float8, sample_n FROM kpi_daily
              WHERE snapshot_date BETWEEN $1 AND $2",
        )
        .bind(oldest)
        .bind(today)
        .fetch_all(&pool)
        .await?;
        let mut by: std::collections::HashMap<NaiveDate, std::collections::HashMap<String, (f64, Option<i32>)>> =
            std::collections::HashMap::new();
        for (k, d, v, sn) in rows {
            by.entry(d).or_default().insert(k, (v, sn));
        }
        for r in &ranges {
            let day_map = by.get(&r.from);
            let cells = keys
                .iter()
                .map(|(key, ..)| {
                    let c = day_map.and_then(|m| m.get(key));
                    (
                        key.clone(),
                        HistoryCell { value: c.map(|x| x.0), sample_n: c.and_then(|x| x.1).map(|n| n as i64) },
                    )
                })
                .collect();
            buckets.push(HistoryBucket {
                bucket: r.from.to_string(),
                label_from: r.from.to_string(),
                label_to: r.to.to_string(),
                is_provisional: periods::includes_today(r, today),
                cells,
            });
        }
    } else {
        // week/month: exact range aggregation per bucket (reuses agg.rs).
        for r in &ranges {
            let a = agg::aggregate(&pool, r.from, r.to).await?;
            let cells = keys
                .iter()
                .map(|(key, ..)| {
                    let (v, sn) = a.get(key.as_str()).copied().unwrap_or((None, None));
                    (key.clone(), HistoryCell { value: v, sample_n: sn })
                })
                .collect();
            buckets.push(HistoryBucket {
                bucket: r.from.to_string(),
                label_from: r.from.to_string(),
                label_to: r.to.to_string(),
                is_provisional: periods::includes_today(r, today),
                cells,
            });
        }
    }

    Ok(Json(HistoryResponse { gran: gran.to_string(), kpis: columns, buckets }))
}

pub async fn breakdown_qc(
    State(pool): State<PgPool>,
    Query(q): Query<PeriodQuery>,
) -> Result<Json<BreakdownResponse>, AppError> {
    // Per-QC throughput for the SELECTED period, every QC, ordered by crane id asc.
    // Past days come from raw_*; the terminal-today day folds in MPH from the live
    // vessel_qc_shift rows (same as the headline). Today's per-QC wait isn't captured
    // per-crane, so the wait column reflects the period's past days.
    let today = wp_core::shift::terminal_now().date_naive();
    let r = periods::resolve(q.period.as_deref().unwrap_or("yesterday"), today);
    let (from, to) = (r.cur.from, r.cur.to);
    let raw_to = to.min(today - chrono::Duration::days(1));
    let include_today = from <= today && today <= to;

    // MPH numerator/denominator per QC from raw past days
    let mph_raw: Vec<(String, f64, f64)> = sqlx::query_as(
        "SELECT qc_machno,
                coalesce(sum(k_mph_per_active_hour*active_hours),0)::float8,
                coalesce(sum(active_hours),0)::float8
           FROM raw_k_mph_realtime WHERE snapshot_date BETWEEN $1 AND $2
          GROUP BY qc_machno",
    ).bind(from).bind(raw_to).fetch_all(&pool).await?;
    // MPH today (per QC) from the live shift rows: num = moves, den = active_hours
    let mph_today: Vec<(String, f64, f64)> = if include_today {
        sqlx::query_as(
            "SELECT qc, coalesce(sum(moves),0)::float8, coalesce(sum(active_hours),0)::float8
               FROM vessel_qc_shift WHERE business_date = $1 GROUP BY qc",
        ).bind(today).fetch_all(&pool).await?
    } else { vec![] };
    // QC wait numerator/denominator per QC from raw past days
    let wait_raw: Vec<(String, f64, f64)> = sqlx::query_as(
        "SELECT qc,
                coalesce(sum(avg_idle_sec*idle_periods),0)::float8,
                coalesce(sum(idle_periods),0)::float8
           FROM raw_k_qc_q WHERE snapshot_date BETWEEN $1 AND $2
          GROUP BY qc",
    ).bind(from).bind(raw_to).fetch_all(&pool).await?;

    #[derive(Default)]
    struct Acc { mph_num: f64, mph_den: f64, wait_num: f64, wait_den: f64 }
    let mut by: std::collections::BTreeMap<String, Acc> = std::collections::BTreeMap::new(); // BTreeMap → crane id ascending
    for (qc, num, den) in mph_raw.into_iter().chain(mph_today) {
        let e = by.entry(qc).or_default();
        e.mph_num += num; e.mph_den += den;
    }
    for (qc, num, den) in wait_raw {
        let e = by.entry(qc).or_default();
        e.wait_num += num; e.wait_den += den;
    }

    let rows = by
        .into_iter()
        .map(|(qc, a)| QcRow {
            qc,
            mph: (a.mph_den > 0.0).then(|| (a.mph_num / a.mph_den * 100.0).round() / 100.0),
            qc_wait_sec: (a.wait_den > 0.0).then(|| (a.wait_num / a.wait_den * 10.0).round() / 10.0),
            status: None,
        })
        .collect();
    Ok(Json(BreakdownResponse { as_of: format!("{from} ~ {to}"), rows }))
}

pub async fn stats(
    State(pool): State<PgPool>,
    Path(key): Path<String>,
    Query(q): Query<AsOfQuery>,
) -> Result<Json<StatsResponse>, AppError> {
    let Some(kpi) = KpiKey::from_str(&key) else {
        return Err(AppError(anyhow::anyhow!("unknown kpi key")));
    };
    let as_of = resolve_as_of(&pool, q.as_of).await?;
    let row: Option<(Option<f64>, Option<i32>, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<bool>)> =
        sqlx::query_as(
            "SELECT baseline_value::float8, baseline_n_days, delta_abs::float8, delta_pct::float8,
                    p_value::float8, cohens_d::float8, is_significant
               FROM kpi_baseline
              WHERE kpi_key = $1 AND ($2::date IS NULL OR as_of_date = $2)
              ORDER BY as_of_date DESC LIMIT 1",
        )
        .bind(kpi.as_str())
        .bind(as_of)
        .fetch_optional(&pool)
        .await?;
    let (baseline, baseline_n_days, delta_abs, delta_pct, p_value, cohens_d, is_significant) =
        row.unwrap_or((None, None, None, None, None, None, None));
    Ok(Json(StatsResponse {
        key: kpi.as_str().to_string(),
        as_of: as_of.map(|d| d.to_string()).unwrap_or_default(),
        baseline,
        baseline_n_days,
        delta_abs,
        delta_pct,
        p_value,
        cohens_d,
        is_significant,
    }))
}

pub async fn health(State(pool): State<PgPool>) -> Result<Json<HealthResponse>, AppError> {
    let sources: Vec<(String, Option<String>, Option<NaiveDate>, bool)> = sqlx::query_as(
        "SELECT kpi_key, last_status, last_success_date, is_stale FROM data_freshness ORDER BY kpi_key",
    )
    .fetch_all(&pool)
    .await?;
    let any_stale = sources.iter().any(|s| s.3);
    let any_fail = sources.iter().any(|s| s.1.as_deref() == Some("FAILED"));
    let overall = if any_fail { "DEGRADED" } else if any_stale { "STALE" } else { "OK" };
    Ok(Json(HealthResponse {
        overall: overall.to_string(),
        postgres: "OK".to_string(),
        sources: sources
            .into_iter()
            .map(|(k, st, d, stale)| FreshnessRow {
                source: k,
                last_status: st,
                last_success_date: d.map(|x| x.to_string()),
                is_stale: stale,
            })
            .collect(),
    }))
}
