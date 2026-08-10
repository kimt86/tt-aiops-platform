//! Current-shift cumulative KPI extraction for the LIVE tab. Each tick renders the
//! shift-windowed SQL (shift start -> min(now, shift end)), folds the rows to a single
//! cumulative headline with the SAME intensive formulas as `transform.rs`, and writes
//! `kpi_shift` (latest) + `kpi_shift_history` (per-elapsed, for same-elapsed deltas).
//! No `raw_*` write — keeps those nightly/today-tick owned.

use anyhow::{Context, Result};
use chrono::{NaiveDate, NaiveDateTime};
use sqlx::PgPool;
use tt_core::shift::{self, Shift};

use crate::kpis::common::run_logged;
use crate::params::{self, TimeCol};
use crate::runner::Toolbox;

// same SQL files as the day path; now token-parameterized for the shift window
const SQL_MPH: &str = include_str!("../sql/c07_k_mph_realtime.sql");
const SQL_QCQ: &str = include_str!("../sql/f2_k_qc_q.sql");
const SQL_EMPTY: &str = include_str!("../sql/e4_k_empty_decomposition.sql");
const SQL_TT_CYCLE: &str = include_str!("../sql/c10_k_tt_cycle.sql");
const SQL_CRANEQ: &str = include_str!("../sql/c08_k_crane_q.sql");

// KPI parity (PLAN-extractor.md CHUNK2): local Postgres-mirror re-derivations of the same
// KPI, run alongside the Oracle path so both land in kpi_parity_log. Oracle stays the
// authoritative value (not swapped) -- this is measurement only.
const SQL_LOCAL_MPH: &str = include_str!("../sql/local/l_mph.sql");
const SQL_LOCAL_QCQ: &str = include_str!("../sql/local/l_qc_q.sql");
const SQL_LOCAL_TT_CYCLE: &str = include_str!("../sql/local/l_tt_cycle.sql");
// 옛 c10 "회전 리듬" — 표시에서 내린 뒤에도 파리티 키 K_CYCLE_C10 으로 내부 수집을
// 계속한다(2026-08-10 재정의 때 사용자와 약속·비교용). 산식 이력은 파일 머리 참조.
const SQL_LOCAL_TT_CYCLE_C10: &str = include_str!("../sql/local/l_tt_cycle_c10.sql");
const SQL_LOCAL_CRANEQ: &str = include_str!("../sql/local/l_crane_q.sql");
const SQL_LOCAL_CYCLE: &str = include_str!("../sql/local/l_cycle.sql");
// K_EMPTY production local source (PLAN-extractor.md CHUNK7 7-2(d), same KPI_T1_SRC gate as
// the other src_* below; K_EMPTY lives in the "want_heavy"/t2 branch but the switch name is
// shared across all shift-tick sources, not just literal t1).
const SQL_LOCAL_EMPTY: &str = include_str!("../sql/local/l_empty.sql");

// KPI_T1_SRC kill switch (PLAN-extractor.md CHUNK6 6-1/6-2): local Postgres mirror
// (vessel-grouped, same shape as SQL_MPH) for the PRODUCTION src_mph_vessels /
// src_qcq / src_cycle / src_craneq paths, distinct from the parity-only SQL_LOCAL_*
// above (which stay machno-only / headline-only and keep logging to kpi_parity_log
// regardless of this switch).
const SQL_LOCAL_MPH_VESSEL: &str = include_str!("../sql/local/l_mph_vessel.sql");

/// `KPI_T1_SRC=oracle` reverts src_mph_vessels/src_qcq/src_cycle/src_craneq to the
/// Oracle path unchanged; any other value (including unset) = local (default).
fn kpi_t1_src_local() -> bool {
    std::env::var("KPI_T1_SRC").map(|v| v != "oracle").unwrap_or(true)
}

fn sum<T: Copy + Into<f64>>(it: impl Iterator<Item = Option<T>>) -> f64 {
    it.flatten().map(|v| v.into()).sum()
}

// ---- KPI parity (kpi_parity_log): local-vs-Oracle, log-only, never swaps the KPI value ----

/// `KPI_PARITY=off` disables all parity writes; default on. Any other value (including
/// unset) is treated as on.
fn kpi_parity_enabled() -> bool {
    std::env::var("KPI_PARITY").map(|v| v != "off").unwrap_or(true)
}

async fn insert_parity(
    pool: &PgPool,
    kpi_key: &str,
    date: NaiveDate,
    sh: Shift,
    src: &str,
    value: Option<f64>,
    sample_n: Option<i64>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO kpi_parity_log (kpi_key, business_date, shift, src, value, sample_n)
         VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(kpi_key)
    .bind(date)
    .bind(sh.label())
    .bind(src)
    .bind(value)
    .bind(sample_n)
    .execute(pool)
    .await
    .with_context(|| format!("kpi_parity_log insert {kpi_key}/{src}"))?;
    Ok(())
}

/// Same continue-past-failure attitude as the `step!` macro, but at warn level (parity
/// is measurement-only and must never affect the KPI's own success/failure).
macro_rules! parity_step {
    ($kpi:expr, $fut:expr) => {
        if let Err(e) = $fut.await {
            tracing::warn!(kpi = $kpi, error = %e, "kpi parity failed (continuing)");
        }
    };
}

#[derive(Debug, sqlx::FromRow)]
struct LocalMphRow {
    k_mph_per_active_hour: Option<f64>,
    active_hours: Option<f64>,
}

#[derive(Debug, sqlx::FromRow)]
struct LocalQcqRow {
    avg_idle_sec: Option<f64>,
    idle_periods: Option<f64>,
}

#[derive(Debug, sqlx::FromRow)]
struct LocalTtCycleRow {
    samples: Option<f64>,
    med_sec: Option<f64>,
}

#[derive(Debug, sqlx::FromRow)]
struct LocalCraneQRow {
    k_crane_q_avg_sec: Option<f64>,
    in_range: Option<f64>,
}

#[derive(Debug, sqlx::FromRow)]
struct LocalCycleRow {
    jobs: Option<f64>,
    med_sec: Option<f64>,
}

async fn parity_mph(
    pool: &PgPool,
    date: NaiveDate,
    sh: Shift,
    start: NaiveDateTime,
    end: NaiveDateTime,
    oracle_value: Option<f64>,
    oracle_n: i64,
) -> Result<()> {
    // ★정정(3원칙#3): oracle 값이 없으면 oracle 행을 넣지 않는다 -- NULL 행을 틱마다 쌓지 않음.
    if oracle_n > 0 {
        insert_parity(pool, "K_MPH", date, sh, "oracle", oracle_value, Some(oracle_n)).await?;
    }
    let ws = tt_core::shift::terminal_to_utc(start);
    let we = tt_core::shift::terminal_to_utc(end);
    let rows: Vec<LocalMphRow> = sqlx::query_as(SQL_LOCAL_MPH).bind(ws).bind(we).fetch_all(pool).await?;
    let num = sum(rows.iter().map(|r| match (r.k_mph_per_active_hour, r.active_hours) { (Some(m), Some(h)) => Some(m * h), _ => None }));
    let den = sum(rows.iter().map(|r| r.active_hours));
    let value = if den > 0.0 { Some((num / den * 100.0).round() / 100.0) } else { None };
    insert_parity(pool, "K_MPH", date, sh, "local", value, Some(den as i64)).await
}

async fn parity_qcq(
    pool: &PgPool,
    date: NaiveDate,
    sh: Shift,
    start: NaiveDateTime,
    end: NaiveDateTime,
    oracle_value: Option<f64>,
    oracle_n: i64,
) -> Result<()> {
    if oracle_n > 0 {
        insert_parity(pool, "K_QC_Q", date, sh, "oracle", oracle_value, Some(oracle_n)).await?;
    }
    let ws = tt_core::shift::terminal_to_utc(start);
    let we = tt_core::shift::terminal_to_utc(end);
    let rows: Vec<LocalQcqRow> = sqlx::query_as(SQL_LOCAL_QCQ).bind(ws).bind(we).fetch_all(pool).await?;
    let num = sum(rows.iter().map(|r| match (r.avg_idle_sec, r.idle_periods) { (Some(a), Some(p)) => Some(a * p), _ => None }));
    let den = sum(rows.iter().map(|r| r.idle_periods));
    let value = if den > 0.0 { Some((num / den * 10.0).round() / 10.0) } else { None };
    insert_parity(pool, "K_QC_Q", date, sh, "local", value, Some(den as i64)).await
}

async fn parity_tt_cycle(
    pool: &PgPool,
    date: NaiveDate,
    sh: Shift,
    start: NaiveDateTime,
    end: NaiveDateTime,
    oracle_value: Option<f64>,
    oracle_n: i64,
) -> Result<()> {
    if oracle_n > 0 {
        insert_parity(pool, "K_TT_CYCLE", date, sh, "oracle", oracle_value, Some(oracle_n)).await?;
    }
    let ws = tt_core::shift::terminal_to_utc(start);
    let we = tt_core::shift::terminal_to_utc(end);
    let rows: Vec<LocalTtCycleRow> = sqlx::query_as(SQL_LOCAL_TT_CYCLE).bind(ws).bind(we).fetch_all(pool).await?;
    let r = rows.first();
    let samples = r.and_then(|x| x.samples).unwrap_or(0.0);
    let value = r.and_then(|x| x.med_sec).filter(|_| samples > 0.0);
    insert_parity(pool, "K_TT_CYCLE", date, sh, "local", value, Some(samples as i64)).await
}

async fn parity_craneq(
    pool: &PgPool,
    date: NaiveDate,
    sh: Shift,
    start: NaiveDateTime,
    end: NaiveDateTime,
    oracle_value: Option<f64>,
    oracle_n: i64,
) -> Result<()> {
    if oracle_n > 0 {
        insert_parity(pool, "K_RTG_Q", date, sh, "oracle", oracle_value, Some(oracle_n)).await?;
    }
    let ws = tt_core::shift::terminal_to_utc(start);
    let we = tt_core::shift::terminal_to_utc(end);
    let rows: Vec<LocalCraneQRow> = sqlx::query_as(SQL_LOCAL_CRANEQ).bind(ws).bind(we).fetch_all(pool).await?;
    let num = sum(rows.iter().map(|r| match (r.k_crane_q_avg_sec, r.in_range) { (Some(a), Some(n)) => Some(a * n), _ => None }));
    let den = sum(rows.iter().map(|r| r.in_range));
    let value = if den > 0.0 { Some((num / den * 10.0).round() / 10.0) } else { None };
    insert_parity(pool, "K_RTG_Q", date, sh, "local", value, Some(den as i64)).await
}

/// K_CYCLE parity (t2 only, per PLAN-extractor.md 2-3): the Oracle side is NOT
/// re-queried here -- e3b (`sql/e3b_k_cycle_refined_v2.sql`) is already fetched once a
/// day by the nightly `kpis::k_cycle` extract into `raw_k_cycle`; we read that back.
/// Its window is the whole business day, while the local side (`l_cycle.sql`, tt_move_log
/// dispatch->free span) is windowed to shift-start..now -- the two windows differ, which
/// is expected: this key's whole purpose (per the 2-2 table) is to measure how the local
/// TOS-anchored cycle compares to the JOB_ORDER_HISTORY transition-based one, not to match.
async fn parity_cycle_t2(pool: &PgPool, date: NaiveDate, sh: Shift, start: NaiveDateTime, end: NaiveDateTime) -> Result<()> {
    let oracle_rows: Vec<LocalCycleRow> = sqlx::query_as(
        "SELECT jobs, med_sec FROM raw_k_cycle WHERE snapshot_date = $1",
    )
    // raw_k_cycle is one row per jobtype per day (no jobtype filter needed here, folded below)
    .bind(date)
    .fetch_all(pool)
    .await?;
    let o_num = sum(oracle_rows.iter().map(|r| match (r.med_sec, r.jobs) { (Some(m), Some(j)) => Some(m * j), _ => None }));
    let o_den = sum(oracle_rows.iter().map(|r| r.jobs));
    // ★정정(3원칙#3): raw_k_cycle has no row for `date` until the nightly e3b extract runs
    // for it (end of day) -- don't write a NULL oracle row every t2 tick until then.
    if o_den > 0.0 {
        let oracle_value = Some((o_num / o_den * 10.0).round() / 10.0);
        insert_parity(pool, "K_CYCLE", date, sh, "oracle", oracle_value, Some(o_den as i64)).await?;
    }

    let ws = tt_core::shift::terminal_to_utc(start);
    let we = tt_core::shift::terminal_to_utc(end);
    let local_rows: Vec<LocalCycleRow> = sqlx::query_as(SQL_LOCAL_CYCLE).bind(ws).bind(we).fetch_all(pool).await?;
    let l_num = sum(local_rows.iter().map(|r| match (r.med_sec, r.jobs) { (Some(m), Some(j)) => Some(m * j), _ => None }));
    let l_den = sum(local_rows.iter().map(|r| r.jobs));
    let local_value = if l_den > 0.0 { Some((l_num / l_den * 10.0).round() / 10.0) } else { None };
    insert_parity(pool, "K_CYCLE", date, sh, "local", local_value, Some(l_den as i64)).await
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
    }
    if want_util {
        step!("util", src_util(pool, target, date, sh, start, end));
    }
    if want_heavy {
        // moved from t1 -> t2 (PLAN-extractor.md CHUNK6 6-1): 228-row VSB_VOYAGE scan,
        // no need for 3-minute freshness -- 15-minute is plenty.
        step!("voyage_plan", crate::vessel::extract_voyage_plan(pool, target, date));
        step!("empty", src_empty(pool, target, date, sh, start, end));
        step!("crane_q", src_craneq(pool, target, date, sh, start, end));
        // K_CYCLE parity (l_cycle vs the nightly e3b raw_k_cycle already in Postgres) --
        // t2 only, per PLAN-extractor.md 2-3. No extra Oracle round trip.
        if kpi_parity_enabled() {
            parity_step!("K_CYCLE", parity_cycle_t2(pool, date, sh, start, end));
        }
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
    let as_of = tt_core::shift::terminal_to_utc(end);
    let win_start = tt_core::shift::terminal_to_utc(start);
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
    Ok(tt_core::parse::parse_rows(&raw)?)
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
        let local = kpi_t1_src_local();
        let rows: Vec<crate::kpis::k_mph_realtime::Row> = if local {
            let ws = tt_core::shift::terminal_to_utc(start);
            let we = tt_core::shift::terminal_to_utc(end);
            sqlx::query_as(SQL_LOCAL_MPH_VESSEL).bind(ws).bind(we).fetch_all(pool).await?
        } else {
            let sql = params::render_shift(SQL_MPH, date, start, end, Some(TimeCol::MchOper))?;
            fetch(target, &sql).await?
        };

        // K_MPH headline (active-hours-weighted)
        let num = sum(rows.iter().map(|r| match (r.k_mph_per_active_hour, r.active_hours) { (Some(m), Some(h)) => Some(m*h), _ => None }));
        let den = sum(rows.iter().map(|r| r.active_hours));
        let value = if den > 0.0 { Some((num/den*100.0).round()/100.0) } else { None };
        let voyages = rows.iter().map(|r| format!("{}/{}", r.vessel, r.voyage)).collect::<std::collections::HashSet<_>>().len();
        // weight = Σactive_hours (the true K_MPH denominator), NOT the voyage count
        upsert_shift(pool, date, sh, "K_MPH", "move/hr", value, Some(voyages as i64), Some(den), start, end2).await?;
        if kpi_parity_enabled() {
            // local mode: no Oracle round trip happened here, so the oracle side of
            // parity naturally stops logging (oracle_n=0 -> insert_parity skips it).
            let (ov, on) = if local { (None, 0) } else { (value, den as i64) };
            parity_step!("K_MPH", parity_mph(pool, date, sh, start, end2, ov, on));
        }

        // vessel panel (group the same rows by vessel/voyage)
        crate::vessel::write_vessel_shift(pool, date, sh, end2, &rows).await?;
        Ok(rows.len() as u64)
    }).await.map(|_| ())
}

async fn src_qcq(pool: &PgPool, target: &str, date: NaiveDate, sh: Shift, start: NaiveDateTime, end: NaiveDateTime) -> Result<()> {
    run_logged(pool, "K_QC_NOMOVE_SHIFT", date, |_| async move {
        let local = kpi_t1_src_local();
        let rows: Vec<crate::kpis::k_qc_q::Row> = if local {
            let ws = tt_core::shift::terminal_to_utc(start);
            let we = tt_core::shift::terminal_to_utc(end);
            sqlx::query_as(SQL_LOCAL_QCQ).bind(ws).bind(we).fetch_all(pool).await?
        } else {
            let sql = params::render_shift(SQL_QCQ, date, start, end, Some(TimeCol::MchOper))?;
            fetch(target, &sql).await?
        };
        let num = sum(rows.iter().map(|r| match (r.avg_idle_sec, r.idle_periods) { (Some(a), Some(p)) => Some(a*p), _ => None }));
        let den = sum(rows.iter().map(|r| r.idle_periods));
        let value = if den > 0.0 { Some((num/den*10.0).round()/10.0) } else { None };
        // weight = Σidle_periods (= sample_n here)
        upsert_shift(pool, date, sh, "K_QC_NOMOVE", "s", value, Some(den as i64), Some(den), start, end).await?;
        if kpi_parity_enabled() {
            let (ov, on) = if local { (None, 0) } else { (value, den as i64) };
            parity_step!("K_QC_Q", parity_qcq(pool, date, sh, start, end, ov, on));
        }
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
        let rows: Vec<crate::kpis::k_empty::Row> = if kpi_t1_src_local() {
            let ws = tt_core::shift::terminal_to_utc(start);
            let we = tt_core::shift::terminal_to_utc(end);
            sqlx::query_as(SQL_LOCAL_EMPTY).bind(ws).bind(we).fetch_all(pool).await?
        } else {
            let sql = params::render_shift(SQL_EMPTY, date, start, end, Some(TimeCol::JobHist))?;
            fetch(target, &sql).await?
        };
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
    // ★2026-08-10 재정의(사용자 승인·비교 설명 후 결정): 표시 K_CYCLE = **배정→트럭 자유**
    // (tt_move_log dispatch_ts→free_ts, l_tt_cycle.sql 머리에 근거 전체). 값이 c10 리듬의
    // 13~15분에서 ~24분으로 이동 — KC kpi 문서 2곳에 08-10 단차 고지. 옛 c10 은 아래에서
    // K_CYCLE_C10 파리티로 내부 수집 지속, Oracle 킬스위치(KPI_T1_SRC=oracle)는 c10 원본
    // 그대로라 즉시 복귀 가능. (7-3 철회 사고의 교훈대로 raw_k_tt_cycle·kpi_shift 두 표시
    // 경로가 같은 SQL 내용 교체로 동시에 움직인다 — 한쪽만 바뀌는 구조가 아님.)
    run_logged(pool, "K_CYCLE_SHIFT", date, |_| async move {
        let local = kpi_t1_src_local();
        let rows: Vec<crate::kpis::k_tt_cycle::Row> = if local {
            let ws = tt_core::shift::terminal_to_utc(start);
            let we = tt_core::shift::terminal_to_utc(end);
            sqlx::query_as(SQL_LOCAL_TT_CYCLE).bind(ws).bind(we).fetch_all(pool).await?
        } else {
            let sql = params::render_shift(SQL_TT_CYCLE, date, start, end, Some(TimeCol::MchOper))?;
            fetch(target, &sql).await?
        };
        let r = rows.first();
        let samples = r.and_then(|x| x.samples).unwrap_or(0.0);
        let value = r.and_then(|x| x.med_sec).filter(|_| samples > 0.0);
        // weight = samples (= sample_n)
        upsert_shift(pool, date, sh, "K_CYCLE", "s", value, Some(samples as i64), Some(samples), start, end).await?;
        if kpi_parity_enabled() {
            let (ov, on) = if local { (None, 0) } else { (value, samples as i64) };
            parity_step!("K_TT_CYCLE", parity_tt_cycle(pool, date, sh, start, end, ov, on));
        }
        // 옛 정의(c10 회전 리듬) 내부 수집 — 표시에서 내렸지만 계속 잰다(재정의 때 약속).
        if local && kpi_parity_enabled() {
            let ws = tt_core::shift::terminal_to_utc(start);
            let we = tt_core::shift::terminal_to_utc(end);
            match sqlx::query_as::<_, crate::kpis::k_tt_cycle::Row>(SQL_LOCAL_TT_CYCLE_C10)
                .bind(ws).bind(we).fetch_all(pool).await
            {
                Ok(c10) => {
                    let r = c10.first();
                    let n = r.and_then(|x| x.samples).unwrap_or(0.0);
                    let v = r.and_then(|x| x.med_sec).filter(|_| n > 0.0);
                    if let Err(e) = insert_parity(pool, "K_CYCLE_C10", date, sh, "local", v, Some(n as i64)).await {
                        tracing::warn!(error = %e, "K_CYCLE_C10 parity insert failed (continuing)");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "K_CYCLE_C10 query failed (continuing)"),
            }
        }
        Ok(rows.len() as u64)
    }).await.map(|_| ())
}

async fn src_craneq(pool: &PgPool, target: &str, date: NaiveDate, sh: Shift, start: NaiveDateTime, end: NaiveDateTime) -> Result<()> {
    run_logged(pool, "K_RTG_Q_SHIFT", date, |_| async move {
        let local = kpi_t1_src_local();
        // local mode uses LocalCraneQRow (k_crane_q_avg_sec, in_range only -- the
        // production fold below never touches jobtype/events_nn/etc), not
        // k_crane_q_daily::Row (which requires a work_date this shift window has none
        // of a single fixed value for).
        let (num, den, n): (f64, f64, usize) = if local {
            let ws = tt_core::shift::terminal_to_utc(start);
            let we = tt_core::shift::terminal_to_utc(end);
            let rows: Vec<LocalCraneQRow> = sqlx::query_as(SQL_LOCAL_CRANEQ).bind(ws).bind(we).fetch_all(pool).await?;
            let num = sum(rows.iter().map(|r| match (r.k_crane_q_avg_sec, r.in_range) { (Some(a), Some(n)) => Some(a*n), _ => None }));
            let den = sum(rows.iter().map(|r| r.in_range));
            (num, den, rows.len())
        } else {
            let sql = params::render_shift(SQL_CRANEQ, date, start, end, Some(TimeCol::JobHist))?;
            let rows: Vec<crate::kpis::k_crane_q_daily::Row> = fetch(target, &sql).await?;
            let num = sum(rows.iter().map(|r| match (r.k_crane_q_avg_sec, r.in_range) { (Some(a), Some(n)) => Some(a*n), _ => None }));
            let den = sum(rows.iter().map(|r| r.in_range));
            (num, den, rows.len())
        };
        let value = if den > 0.0 { Some((num/den*10.0).round()/10.0) } else { None };
        // weight = Σin_range (= sample_n here)
        upsert_shift(pool, date, sh, "K_RTG_Q", "s", value, Some(den as i64), Some(den), start, end).await?;
        if kpi_parity_enabled() {
            let (ov, on) = if local { (None, 0) } else { (value, den as i64) };
            parity_step!("K_RTG_Q", parity_craneq(pool, date, sh, start, end, ov, on));
        }
        Ok(n as u64)
    }).await.map(|_| ())
}
