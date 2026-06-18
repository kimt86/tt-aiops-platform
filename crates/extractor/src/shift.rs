//! Current-shift cumulative KPI extraction for the LIVE tab. Each tick renders the
//! shift-windowed SQL (shift start -> min(now, shift end)), folds the rows to a single
//! cumulative headline with the SAME intensive formulas as `transform.rs`, and writes
//! `kpi_shift` (latest) + `kpi_shift_history` (per-elapsed, for same-elapsed deltas).
//! No `raw_*` write — keeps those nightly/today-tick owned.

use anyhow::{Context, Result};
use chrono::{NaiveDate, NaiveDateTime};
use sqlx::PgPool;
use wp_core::shift::{self, Shift};

use crate::kpis::common::run_logged;
use crate::params::{self, TimeCol};
use crate::runner::Toolbox;

// same SQL files as the day path; now token-parameterized for the shift window
const SQL_MPH: &str = include_str!("../sql/c07_k_mph_realtime.sql");
const SQL_QCQ: &str = include_str!("../sql/f2_k_qc_q.sql");
const SQL_EMPTY: &str = include_str!("../sql/e4_k_empty_decomposition.sql");
const SQL_TT_CYCLE: &str = include_str!("../sql/c10_k_tt_cycle.sql");
const SQL_CRANEQ: &str = include_str!("../sql/c08_k_crane_q.sql");

fn sum<T: Copy + Into<f64>>(it: impl Iterator<Item = Option<T>>) -> f64 {
    it.flatten().map(|v| v.into()).sum()
}

/// Run the full shift tick for a tier.
pub async fn tick_shift(pool: &PgPool, target: &str, tier: &str) -> Result<()> {
    let now = shift::terminal_now().naive_local();
    let (date, sh) = shift::current(now);
    let (start, nominal_end) = shift::window(date, sh);
    let end = now.min(nominal_end);

    let want_util = matches!(tier, "t1" | "all");
    let want_cheap = matches!(tier, "t1" | "all"); // MCH_OPERATION KPIs
    let want_heavy = matches!(tier, "t2" | "all"); // JOB_ORDER_HISTORY KPIs
    if !(want_util || want_cheap || want_heavy) {
        anyhow::bail!("unknown shift tier '{tier}' (t1|t2|all)");
    }

    // each source fetched once; continue past individual failures
    macro_rules! step {
        ($name:expr, $fut:expr) => {
            if let Err(e) = $fut.await {
                tracing::error!(source = $name, error = %e, "shift source failed (continuing)");
            }
        };
    }
    if want_cheap {
        // one MCH_OPERATION fetch produces both K_MPH and the vessel panel (zero extra query)
        step!("mph+vessels", src_mph_vessels(pool, target, date, sh, start, end));
        step!("qc_q", src_qcq(pool, target, date, sh, start, end));
        // TT cycle is now also an MCH_OPERATION query (cheap) → refresh it on the fast tier
        step!("cycle", src_cycle(pool, target, date, sh, start, end));
        step!("voyage_plan", crate::vessel::extract_voyage_plan(pool, target, date));
    }
    if want_util {
        step!("util", src_util(pool, target, date, sh, start, end));
    }
    if want_heavy {
        step!("empty", src_empty(pool, target, date, sh, start, end));
        step!("crane_q", src_craneq(pool, target, date, sh, start, end));
    }

    // Refresh the HISTORY "today" provisional rows by folding today's shift
    // cumulatives in Postgres — no extra Oracle scan. Whatever shift KPIs exist so
    // far (this tier + earlier ticks) are combined; the rest fill on later ticks.
    step!("today-rollup", crate::transform::rollup_today_from_shifts(pool, date));

    tracing::info!(%date, shift = sh.label(), %tier, "shift tick done");
    Ok(())
}

/// Write one cumulative shift value to kpi_shift + append history.
///
/// `agg_weight` is the true aggregation denominator (active_hours, distance,
/// in_range, idle_periods, jobs …) used to combine shifts into a day value as
/// Σ(value·weight)/Σ(weight). Pass `None` only for K_UTIL (avg-of-ratios — not
/// linearly combinable; the HISTORY "today" rollup skips it and nightly fills it).
#[allow(clippy::too_many_arguments)]
async fn upsert_shift(
    pool: &PgPool,
    date: NaiveDate,
    sh: Shift,
    kpi_key: &str,
    unit: &str,
    value: Option<f64>,
    sample_n: Option<i64>,
    agg_weight: Option<f64>,
    start: NaiveDateTime,
    end: NaiveDateTime,
) -> Result<()> {
    let as_of = wp_core::shift::terminal_to_utc(end);
    let win_start = wp_core::shift::terminal_to_utc(start);
    let elapsed_min = (end - start).num_minutes().max(0) as i32;
    let n = sample_n.map(|v| v as i32);

    sqlx::query(
        "INSERT INTO kpi_shift (business_date, shift, kpi_key, value, sample_n, agg_weight, unit, as_of_ts, window_start)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
         ON CONFLICT (business_date, shift, kpi_key) DO UPDATE SET
           value=EXCLUDED.value, sample_n=EXCLUDED.sample_n, agg_weight=EXCLUDED.agg_weight, unit=EXCLUDED.unit,
           as_of_ts=EXCLUDED.as_of_ts, window_start=EXCLUDED.window_start, computed_at=now()",
    )
    .bind(date).bind(sh.label()).bind(kpi_key)
    .bind(value).bind(n).bind(agg_weight).bind(unit).bind(as_of).bind(win_start)
    .execute(pool).await.with_context(|| format!("kpi_shift upsert {kpi_key}"))?;

    sqlx::query(
        "INSERT INTO kpi_shift_history (business_date, shift, kpi_key, as_of_ts, elapsed_min, value, sample_n)
         VALUES ($1,$2,$3,$4,$5,$6,$7)
         ON CONFLICT (business_date, shift, kpi_key, as_of_ts) DO NOTHING",
    )
    .bind(date).bind(sh.label()).bind(kpi_key).bind(as_of).bind(elapsed_min).bind(value).bind(n)
    .execute(pool).await.with_context(|| format!("kpi_shift_history append {kpi_key}"))?;
    Ok(())
}

async fn fetch<T: serde::de::DeserializeOwned>(
    target: &str, sql: &str,
) -> Result<Vec<T>> {
    let raw = Toolbox::from_env(target)?.run_sql(sql).await?;
    Ok(wp_core::parse::parse_rows(&raw)?)
}

// ---- per-source extract+fold (mirrors transform.rs headline formulas) ----

async fn src_util(pool: &PgPool, _target: &str, date: NaiveDate, sh: Shift, start: NaiveDateTime, end: NaiveDateTime) -> Result<()> {
    // TIME-BASED utilization from the API's work-pool assignment samples (no Oracle). Mean of
    // assigned/on-duty over this shift's 60s samples. (TOS session util is no longer used.)
    run_logged(pool, "K_UTIL_SHIFT", date, |_| async move {
        let (value, n): (Option<f64>, i64) = sqlx::query_as(
            "SELECT round(avg(100.0*assigned/nullif(on_duty,0)),1)::float8, count(*)::int8
               FROM util_tt_sample WHERE business_date=$1 AND shift=$2",
        )
        .bind(date)
        .bind(sh.label())
        .fetch_one(pool)
        .await
        .unwrap_or((None, 0));
        // weight None: not combined across shifts (today K_UTIL is recomputed from samples directly)
        upsert_shift(pool, date, sh, "K_UTIL", "%", value, Some(n), None, start, end).await?;
        Ok(n as u64)
    }).await.map(|_| ())
}

/// One MCH_OPERATION (c07) shift fetch → K_MPH headline + per-vessel panel rows.
async fn src_mph_vessels(pool: &PgPool, target: &str, date: NaiveDate, sh: Shift, start: NaiveDateTime, end: NaiveDateTime) -> Result<()> {
    let end2 = end;
    run_logged(pool, "K_MPH_SHIFT", date, |_| async move {
        let sql = params::render_shift(SQL_MPH, date, start, end, Some(TimeCol::MchOper))?;
        let rows: Vec<crate::kpis::k_mph_realtime::Row> = fetch(target, &sql).await?;

        // K_MPH headline (active-hours-weighted)
        let num = sum(rows.iter().map(|r| match (r.k_mph_per_active_hour, r.active_hours) { (Some(m), Some(h)) => Some(m*h), _ => None }));
        let den = sum(rows.iter().map(|r| r.active_hours));
        let value = if den > 0.0 { Some((num/den*100.0).round()/100.0) } else { None };
        let voyages = rows.iter().map(|r| format!("{}/{}", r.vessel, r.voyage)).collect::<std::collections::HashSet<_>>().len();
        // weight = Σactive_hours (the true K_MPH denominator), NOT the voyage count
        upsert_shift(pool, date, sh, "K_MPH", "move/hr", value, Some(voyages as i64), Some(den), start, end2).await?;

        // vessel panel (group the same rows by vessel/voyage)
        crate::vessel::write_vessel_shift(pool, date, sh, end2, &rows).await?;
        Ok(rows.len() as u64)
    }).await.map(|_| ())
}

async fn src_qcq(pool: &PgPool, target: &str, date: NaiveDate, sh: Shift, start: NaiveDateTime, end: NaiveDateTime) -> Result<()> {
    run_logged(pool, "K_QC_NOMOVE_SHIFT", date, |_| async move {
        let sql = params::render_shift(SQL_QCQ, date, start, end, Some(TimeCol::MchOper))?;
        let rows: Vec<crate::kpis::k_qc_q::Row> = fetch(target, &sql).await?;
        let num = sum(rows.iter().map(|r| match (r.avg_idle_sec, r.idle_periods) { (Some(a), Some(p)) => Some(a*p), _ => None }));
        let den = sum(rows.iter().map(|r| r.idle_periods));
        let value = if den > 0.0 { Some((num/den*10.0).round()/10.0) } else { None };
        // weight = Σidle_periods (= sample_n here)
        upsert_shift(pool, date, sh, "K_QC_NOMOVE", "s", value, Some(den as i64), Some(den), start, end).await?;
        // also K_QC_TT_WAIT (same-bay ≈ truck-wait) from the same rows (no extra Oracle hit)
        let sb_num = sum(rows.iter().map(|r| match (r.same_bay_avg_sec, r.same_bay_periods) { (Some(a), Some(p)) => Some(a*p), _ => None }));
        let sb_den = sum(rows.iter().map(|r| r.same_bay_periods));
        let sb_value = if sb_den > 0.0 { Some((sb_num/sb_den*10.0).round()/10.0) } else { None };
        upsert_shift(pool, date, sh, "K_QC_TT_WAIT", "s", sb_value, Some(sb_den as i64), Some(sb_den), start, end).await?;
        Ok(rows.len() as u64)
    }).await.map(|_| ())
}

async fn src_empty(pool: &PgPool, target: &str, date: NaiveDate, sh: Shift, start: NaiveDateTime, end: NaiveDateTime) -> Result<()> {
    run_logged(pool, "K_EMPTY_SHIFT", date, |_| async move {
        let sql = params::render_shift(SQL_EMPTY, date, start, end, Some(TimeCol::JobHist))?;
        let rows: Vec<crate::kpis::k_empty::Row> = fetch(target, &sql).await?;
        let jobs = sum(rows.iter().map(|r| r.jobs));
        let empty = sum(rows.iter().map(|r| r.total_empty_m));
        let laden = sum(rows.iter().map(|r| r.total_laden_m));
        let empty_km = if jobs > 0.0 { Some((empty/jobs/1000.0*10000.0).round()/10000.0) } else { None };
        let ratio = if empty+laden > 0.0 { Some((empty/(empty+laden)*100.0*10000.0).round()/10000.0) } else { None };
        // K_EMPTY weight = jobs (= sample_n); K_EMPTY_R weight = Σ(empty+laden) distance, NOT jobs
        upsert_shift(pool, date, sh, "K_EMPTY", "km/Job", empty_km, Some(jobs as i64), Some(jobs), start, end).await?;
        upsert_shift(pool, date, sh, "K_EMPTY_R", "%", ratio, Some(jobs as i64), Some(empty + laden), start, end).await?;
        Ok(rows.len() as u64)
    }).await.map(|_| ())
}

async fn src_cycle(pool: &PgPool, target: &str, date: NaiveDate, sh: Shift, start: NaiveDateTime, end: NaiveDateTime) -> Result<()> {
    // Displayed K_CYCLE is the REAL TT cycle (MCH_OPERATION per-truck QC-move interval),
    // not the container handling span. One aggregate row; value = median, weight = samples.
    run_logged(pool, "K_CYCLE_SHIFT", date, |_| async move {
        let sql = params::render_shift(SQL_TT_CYCLE, date, start, end, Some(TimeCol::MchOper))?;
        let rows: Vec<crate::kpis::k_tt_cycle::Row> = fetch(target, &sql).await?;
        let r = rows.first();
        let samples = r.and_then(|x| x.samples).unwrap_or(0.0);
        let value = r.and_then(|x| x.med_sec).filter(|_| samples > 0.0);
        // weight = samples (= sample_n)
        upsert_shift(pool, date, sh, "K_CYCLE", "s", value, Some(samples as i64), Some(samples), start, end).await?;
        Ok(rows.len() as u64)
    }).await.map(|_| ())
}

async fn src_craneq(pool: &PgPool, target: &str, date: NaiveDate, sh: Shift, start: NaiveDateTime, end: NaiveDateTime) -> Result<()> {
    run_logged(pool, "K_RTG_Q_SHIFT", date, |_| async move {
        let sql = params::render_shift(SQL_CRANEQ, date, start, end, Some(TimeCol::JobHist))?;
        let rows: Vec<crate::kpis::k_crane_q_daily::Row> = fetch(target, &sql).await?;
        let num = sum(rows.iter().map(|r| match (r.k_crane_q_avg_sec, r.in_range) { (Some(a), Some(n)) => Some(a*n), _ => None }));
        let den = sum(rows.iter().map(|r| r.in_range));
        let value = if den > 0.0 { Some((num/den*10.0).round()/10.0) } else { None };
        // weight = Σin_range (= sample_n here)
        upsert_shift(pool, date, sh, "K_RTG_Q", "s", value, Some(den as i64), Some(den), start, end).await?;
        Ok(rows.len() as u64)
    }).await.map(|_| ())
}
