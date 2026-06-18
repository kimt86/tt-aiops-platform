//! Range aggregation of the 7 headline KPIs over [from, to].
//!
//! Past days come from L0 (`raw_*`) with the same intensive formulas as the daily
//! transform, summed across the range so a single past day reproduces the stored
//! `kpi_daily` value exactly. The TERMINAL-today day, which has no authoritative
//! `raw_*` yet, is folded in from the LIVE shift cumulatives (`kpi_shift`) at the
//! numerator/denominator level — **zero extra Oracle scan**, the shift ticks already
//! pulled it. Six KPIs combine exactly (sum-weighted means / ratios of sums); K_UTIL
//! (avg-of-ratios) cannot be component-combined, so today is omitted for it and the
//! nightly authoritative run fills it.

use anyhow::Result;
use chrono::{Duration, NaiveDate};
use sqlx::PgPool;
use std::collections::HashMap;

pub type Agg = HashMap<&'static str, (Option<f64>, Option<i64>)>;

/// (numerator, denominator, sample_n) over the raw range. `value = num/den` in the
/// KPI's natural units (callers apply rounding / the EMPTY_R ×100 scaling).
async fn raw_nd(pool: &PgPool, sql: &str, from: NaiveDate, to: NaiveDate) -> Result<(f64, f64, i64)> {
    if to < from {
        return Ok((0.0, 0.0, 0)); // empty raw window (range is today-only)
    }
    let row: Option<(Option<f64>, Option<f64>, Option<i64>)> =
        sqlx::query_as(sql).bind(from).bind(to).fetch_optional(pool).await?;
    let (n, d, c) = row.unwrap_or((None, None, None));
    Ok((n.unwrap_or(0.0), d.unwrap_or(0.0), c.unwrap_or(0)))
}

/// Today's per-KPI shift contribution: Σ(value·weight), Σ(weight), Σ(sample_n).
/// COALESCE(agg_weight, sample_n) lets pre-`agg_weight` shift rows still contribute
/// (exact where weight==sample_n; a small transition skew for K_MPH/K_EMPTY_R until
/// the next tick repopulates agg_weight — the nightly run then makes it authoritative).
async fn today_contrib(pool: &PgPool, date: NaiveDate) -> Result<HashMap<String, (f64, f64, i64)>> {
    let rows: Vec<(String, Option<f64>, Option<f64>, Option<i64>)> = sqlx::query_as(
        "SELECT kpi_key,
                sum(value * coalesce(agg_weight, sample_n))::float8,
                sum(coalesce(agg_weight, sample_n))::float8,
                sum(sample_n)::int8
           FROM kpi_shift
          WHERE business_date = $1 AND value IS NOT NULL
            AND coalesce(agg_weight, sample_n) IS NOT NULL
          GROUP BY kpi_key",
    )
    .bind(date)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(k, vw, w, n)| (k, (vw.unwrap_or(0.0), w.unwrap_or(0.0), n.unwrap_or(0))))
        .collect())
}

pub async fn aggregate(pool: &PgPool, from: NaiveDate, to: NaiveDate) -> Result<Agg> {
    let today = wp_core::shift::terminal_now().date_naive();
    let raw_to = to.min(today - Duration::days(1)); // raw_* owns everything before today
    let include_today = from <= today && today <= to;

    let tc = if include_today { today_contrib(pool, today).await? } else { HashMap::new() };
    let t = |key: &str| tc.get(key).copied().unwrap_or((0.0, 0.0, 0));

    let mut m: Agg = HashMap::new();

    // ---- six component KPIs: value = (raw_num + today_num) / (raw_den + today_w) ----
    // K_EMPTY (km/Job): raw_num already in km (metres/1000); today value·W is in km.
    {
        let (num, den, nr) = raw_nd(pool,
            "SELECT (sum(total_empty_m)/1000.0)::float8, sum(jobs)::float8, sum(jobs)::int8
               FROM raw_k_empty WHERE snapshot_date BETWEEN $1 AND $2", from, raw_to).await?;
        let (vw, w, nt) = t("K_EMPTY");
        m.insert("K_EMPTY", finish(num + vw, den + w, nr + nt, 4, 1.0));
    }
    // K_EMPTY_R (%): keep numerator in metres. today value·W = %·metres = 100·empty_m.
    {
        let (num, den, nr) = raw_nd(pool,
            "SELECT sum(total_empty_m)::float8, sum(total_empty_m+total_laden_m)::float8, sum(jobs)::int8
               FROM raw_k_empty WHERE snapshot_date BETWEEN $1 AND $2", from, raw_to).await?;
        let (vw, w, nt) = t("K_EMPTY_R");
        m.insert("K_EMPTY_R", finish(num + vw / 100.0, den + w, nr + nt, 4, 100.0));
    }
    // K_CYCLE (s): real TT cycle (raw_k_tt_cycle), weighted median, weight = samples.
    // (The container handling span lives in raw_k_cycle, kept internally, not displayed.)
    {
        let (num, den, nr) = raw_nd(pool,
            "SELECT sum(med_sec*samples)::float8, sum(samples)::float8, sum(samples)::int8
               FROM raw_k_tt_cycle WHERE snapshot_date BETWEEN $1 AND $2", from, raw_to).await?;
        let (vw, w, nt) = t("K_CYCLE");
        m.insert("K_CYCLE", finish(num + vw, den + w, nr + nt, 1, 1.0));
    }
    // K_CYCLE_DS / K_CYCLE_LD (s): per-jobtype, samples-weighted median. raw_k_tt_cycle has
    // today's provisional row already (tick-written), so read the full [from,to] directly.
    {
        let (num, den, nr) = raw_nd(pool,
            "SELECT sum(ds_med_sec*ds_samples)::float8, sum(ds_samples)::float8, sum(ds_samples)::int8
               FROM raw_k_tt_cycle WHERE snapshot_date BETWEEN $1 AND $2", from, to).await?;
        m.insert("K_CYCLE_DS", finish(num, den, nr, 1, 1.0));
        let (num, den, nr) = raw_nd(pool,
            "SELECT sum(ld_med_sec*ld_samples)::float8, sum(ld_samples)::float8, sum(ld_samples)::int8
               FROM raw_k_tt_cycle WHERE snapshot_date BETWEEN $1 AND $2", from, to).await?;
        m.insert("K_CYCLE_LD", finish(num, den, nr, 1, 1.0));
    }
    // K_RTG_Q (s): weight = in_range.
    {
        let (num, den, nr) = raw_nd(pool,
            "SELECT sum(k_crane_q_avg_sec*in_range)::float8, sum(in_range)::float8, sum(in_range)::int8
               FROM raw_k_crane_q_daily WHERE work_date BETWEEN $1 AND $2", from, raw_to).await?;
        let (vw, w, nt) = t("K_RTG_Q");
        m.insert("K_RTG_Q", finish(num + vw, den + w, nr + nt, 1, 1.0));
    }
    // K_MPH (move/hr): weight = active_hours. N = distinct voyages (raw) + today voyages.
    {
        let (num, den, nr) = raw_nd(pool,
            "SELECT sum(k_mph_per_active_hour*active_hours)::float8, sum(active_hours)::float8,
                    count(distinct vessel||'/'||voyage)::int8
               FROM raw_k_mph_realtime WHERE snapshot_date BETWEEN $1 AND $2", from, raw_to).await?;
        let (vw, w, nt) = t("K_MPH");
        m.insert("K_MPH", finish(num + vw, den + w, nr + nt, 2, 1.0));
    }
    // K_QC_NOMOVE (s): weight = idle_periods.
    {
        let (num, den, nr) = raw_nd(pool,
            "SELECT sum(avg_idle_sec*idle_periods)::float8, sum(idle_periods)::float8, sum(idle_periods)::int8
               FROM raw_k_qc_q WHERE snapshot_date BETWEEN $1 AND $2", from, raw_to).await?;
        let (vw, w, nt) = t("K_QC_NOMOVE");
        m.insert("K_QC_NOMOVE", finish(num + vw, den + w, nr + nt, 1, 1.0));
    }

    // ---- K_UTIL: TIME-BASED utilization = mean of the 60s assignment samples over the
    // whole range (today included — samples carry business_date). value = Σ(ratio)/Σ(1).
    // The TOS session value is no longer used (it counted manned-idle time as utilized). ----
    {
        let row: Option<(Option<f64>, Option<i64>)> = sqlx::query_as(
            "SELECT sum(100.0*assigned/nullif(on_duty,0))::float8, count(*)::int8
               FROM util_tt_sample WHERE business_date BETWEEN $1 AND $2",
        )
        .bind(from)
        .bind(to)
        .fetch_optional(pool)
        .await?;
        let (num, n) = row.map(|(s, c)| (s.unwrap_or(0.0), c.unwrap_or(0))).unwrap_or((0.0, 0));
        m.insert("K_UTIL", finish(num, n as f64, n, 1, 1.0));
    }

    Ok(m)
}

/// Combine a numerator/denominator into a rounded value (×`scale`, `prec` decimals).
/// Returns (None, None) when there is no data so the card shows "—".
fn finish(num: f64, den: f64, n: i64, prec: i32, scale: f64) -> (Option<f64>, Option<i64>) {
    if den <= 0.0 {
        return (None, None);
    }
    let p = 10f64.powi(prec);
    let v = (num / den * scale * p).round() / p;
    (Some(v), Some(n))
}

/// Daily kpi_daily values for a KPI over [from,to] — feeds the Welch test.
pub async fn daily_series(pool: &PgPool, key: &str, from: NaiveDate, to: NaiveDate) -> Result<Vec<f64>> {
    let rows: Vec<(f64,)> = sqlx::query_as(
        "SELECT value::float8 FROM kpi_daily
          WHERE kpi_key=$1 AND snapshot_date BETWEEN $2 AND $3 ORDER BY snapshot_date",
    )
    .bind(key).bind(from).bind(to).fetch_all(pool).await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}
