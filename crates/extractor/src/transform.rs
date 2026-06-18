//! Transform L0 (raw_* snapshots) -> L1 (kpi_daily, kpi_breakdown_qc).
//! Pure PostgreSQL aggregation — no Oracle access. Idempotent per date.
//!
//! Headline aggregation rules per KPI (plan §2.2). Each is an INSERT..SELECT with
//! a HAVING guard so a day with no source rows simply writes nothing (value is
//! NOT NULL). `is_provisional = false` marks these as authoritative (nightly) values.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use sqlx::PgPool;

/// Recompute all L1 rollups for one business date as authoritative (final) values.
pub async fn run(pool: &PgPool, date: NaiveDate) -> Result<()> {
    run_marked(pool, date, false).await
}

/// Recompute L1 rollups, marking them provisional (intra-day "today so far") or not.
pub async fn run_marked(pool: &PgPool, date: NaiveDate, provisional: bool) -> Result<()> {
    kpi_daily(pool, date, provisional).await?;
    breakdown_qc(pool, date).await?;
    tracing::info!(%date, provisional, "transform L0->L1 done");
    Ok(())
}

async fn kpi_daily(pool: &PgPool, date: NaiveDate, provisional: bool) -> Result<()> {
    // K_UTIL — TIME-BASED utilization: mean of the 60s assignment samples (assigned/on-duty)
    // over the day. Assignment from the TOS work pool (allocation→completion incl. queuing;
    // idle = unassigned). The TOS session value (raw_k_util_tt) is no longer displayed.
    upsert_daily(
        pool, date, "K_UTIL", "%", "mean(assigned/on_duty) over util_tt_sample",
        "SELECT round(avg(100.0*assigned/nullif(on_duty,0)), 1), count(*)
           FROM util_tt_sample WHERE business_date = $1 HAVING count(*) > 0",
        provisional,
    ).await?;

    // K_EMPTY — total empty metres / total jobs, as km/job.
    upsert_daily(
        pool, date, "K_EMPTY", "km/Job", "sum(total_empty_m)/sum(jobs)/1000",
        "SELECT round(sum(total_empty_m)/nullif(sum(jobs),0)/1000, 4), sum(jobs)::int
           FROM raw_k_empty WHERE snapshot_date = $1 HAVING sum(jobs) > 0",
        provisional,
    ).await?;

    // K_EMPTY_R — empty / (empty + laden), as %.
    upsert_daily(
        pool, date, "K_EMPTY_R", "%", "sum(empty)/sum(empty+laden)",
        "SELECT round(sum(total_empty_m)/nullif(sum(total_empty_m+total_laden_m),0)*100, 4), sum(jobs)::int
           FROM raw_k_empty WHERE snapshot_date = $1 HAVING sum(total_empty_m+total_laden_m) > 0",
        provisional,
    ).await?;

    // K_CYCLE — real TT cycle (per-truck QC-move interval, MCH_OPERATION), samples-weighted
    // median seconds. The container handling span stays in raw_k_cycle (internal, not shown).
    upsert_daily(
        pool, date, "K_CYCLE", "s", "samples-weighted median(med_sec) from raw_k_tt_cycle",
        "SELECT round(sum(med_sec*samples)/nullif(sum(samples),0), 1), sum(samples)::int
           FROM raw_k_tt_cycle WHERE snapshot_date = $1 HAVING sum(samples) > 0",
        provisional,
    ).await?;

    // K_CYCLE_DS / K_CYCLE_LD — per-jobtype TT cycle (trend-only, not a headline card).
    upsert_daily(
        pool, date, "K_CYCLE_DS", "s", "DS samples-weighted median from raw_k_tt_cycle",
        "SELECT round(sum(ds_med_sec*ds_samples)/nullif(sum(ds_samples),0), 1), sum(ds_samples)::int
           FROM raw_k_tt_cycle WHERE snapshot_date = $1 HAVING sum(ds_samples) > 0",
        provisional,
    ).await?;
    upsert_daily(
        pool, date, "K_CYCLE_LD", "s", "LD samples-weighted median from raw_k_tt_cycle",
        "SELECT round(sum(ld_med_sec*ld_samples)/nullif(sum(ld_samples),0), 1), sum(ld_samples)::int
           FROM raw_k_tt_cycle WHERE snapshot_date = $1 HAVING sum(ld_samples) > 0",
        provisional,
    ).await?;

    // K_RTG_Q — in-range-weighted mean wait seconds.
    upsert_daily(
        pool, date, "K_RTG_Q", "s", "in_range-weighted mean(avg_sec)",
        "SELECT round(sum(k_crane_q_avg_sec*in_range)/nullif(sum(in_range),0), 1), sum(in_range)::int
           FROM raw_k_crane_q_daily WHERE work_date = $1 HAVING sum(in_range) > 0",
        provisional,
    ).await?;

    // K_MPH — active-hours-weighted mean moves/hr; N = distinct voyages.
    upsert_daily(
        pool, date, "K_MPH", "move/hr", "active_hours-weighted mean(per_active_hour)",
        "SELECT round(sum(k_mph_per_active_hour*active_hours)/nullif(sum(active_hours),0), 2),
                count(distinct vessel||'/'||voyage)::int
           FROM raw_k_mph_realtime WHERE snapshot_date = $1 HAVING sum(active_hours) > 0",
        provisional,
    ).await?;

    // K_QC_NOMOVE — idle-periods-weighted mean quay-crane no-move idle seconds (backend-only/hidden).
    upsert_daily(
        pool, date, "K_QC_NOMOVE", "s", "idle_periods-weighted mean(avg_idle_sec)",
        "SELECT round(sum(avg_idle_sec*idle_periods)/nullif(sum(idle_periods),0),1), sum(idle_periods)::int
           FROM raw_k_qc_q WHERE snapshot_date = $1 HAVING sum(idle_periods) > 0",
        provisional,
    ).await?;

    // K_QC_TT_WAIT — same-bay QC idle (approx true QC-waits-for-truck), same_bay_periods-weighted.
    upsert_daily(
        pool, date, "K_QC_TT_WAIT", "s", "same_bay_periods-weighted mean(same_bay_avg_sec)",
        "SELECT round(sum(same_bay_avg_sec*same_bay_periods)/nullif(sum(same_bay_periods),0),1), sum(same_bay_periods)::int
           FROM raw_k_qc_q WHERE snapshot_date = $1 HAVING sum(same_bay_periods) > 0",
        provisional,
    ).await?;

    Ok(())
}

/// Run an aggregation SELECT (returning `value, sample_n`) and upsert into kpi_daily.
/// When `provisional`, the row is flagged and stamped with the current time so the
/// UI can label it "today so far"; a later authoritative run clears the flag.
async fn upsert_daily(
    pool: &PgPool,
    date: NaiveDate,
    kpi_key: &str,
    unit: &str,
    grain: &str,
    agg_sql: &str,
    provisional: bool,
) -> Result<()> {
    let as_of_ts = if provisional { "now()" } else { "NULL" };
    let sql = format!(
        "INSERT INTO kpi_daily
           (kpi_key, snapshot_date, value, sample_n, unit, source_grain, is_provisional, as_of_ts, computed_at)
         SELECT $2, $1, agg.value, agg.sample_n, $3, $4, $5, {as_of_ts}, now()
           FROM ({agg_sql}) AS agg(value, sample_n)
          WHERE agg.value IS NOT NULL
         ON CONFLICT (kpi_key, snapshot_date) DO UPDATE SET
           value=EXCLUDED.value, sample_n=EXCLUDED.sample_n, unit=EXCLUDED.unit,
           source_grain=EXCLUDED.source_grain, is_provisional=$5,
           as_of_ts={as_of_ts}, computed_at=now()"
    );
    sqlx::query(&sql)
        .bind(date)
        .bind(kpi_key)
        .bind(unit)
        .bind(grain)
        .bind(provisional)
        .execute(pool)
        .await
        .with_context(|| format!("kpi_daily upsert for {kpi_key}"))?;
    Ok(())
}

/// Rebuild the provisional "today so far" `kpi_daily` rows by combining the day's
/// `kpi_shift` rows — pure PostgreSQL, **zero extra Oracle scan**. The LIVE shift
/// ticks already pull the data; this folds those shift cumulatives into a full-day
/// value as Σ(value·weight)/Σ(weight) across the day's shifts.
///
/// Exact for the six sum-weighted-mean KPIs (the weight is linear over disjoint
/// shift windows). `COALESCE(agg_weight, sample_n)` lets shift rows written before
/// the `agg_weight` column existed degrade gracefully — exact for the four KPIs
/// whose weight already equals sample_n, approximate for K_MPH / K_EMPTY_R until a
/// fresh tick repopulates them. K_UTIL is intentionally excluded (avg-of-ratios is
/// not linearly combinable; nightly fills it authoritatively).
///
/// Only overwrites a provisional (or absent) row — never clobbers an authoritative
/// nightly value for the same date.
pub async fn rollup_today_from_shifts(pool: &PgPool, date: NaiveDate) -> Result<()> {
    let n = sqlx::query(
        "INSERT INTO kpi_daily
           (kpi_key, snapshot_date, value, sample_n, unit, source_grain, is_provisional, as_of_ts, computed_at)
         SELECT kpi_key, $1,
                round(sum(value * coalesce(agg_weight, sample_n))
                      / nullif(sum(coalesce(agg_weight, sample_n)), 0), 4),
                sum(sample_n)::int,
                max(unit), 'shift-rollup', true, now(), now()
           FROM kpi_shift
          WHERE business_date = $1
            AND value IS NOT NULL
            AND coalesce(agg_weight, sample_n) IS NOT NULL
            AND kpi_key IN ('K_MPH','K_EMPTY','K_EMPTY_R','K_CYCLE','K_RTG_Q','K_QC_NOMOVE','K_QC_TT_WAIT')
          GROUP BY kpi_key
         HAVING sum(coalesce(agg_weight, sample_n)) > 0
         ON CONFLICT (kpi_key, snapshot_date) DO UPDATE SET
           value=EXCLUDED.value, sample_n=EXCLUDED.sample_n, unit=EXCLUDED.unit,
           source_grain=EXCLUDED.source_grain, is_provisional=true,
           as_of_ts=now(), computed_at=now()
         WHERE kpi_daily.is_provisional",
    )
    .bind(date)
    .execute(pool)
    .await
    .context("rollup_today_from_shifts")?
    .rows_affected();
    tracing::info!(%date, kpis = n, "kpi_daily today rollup from shifts (no Oracle)");
    Ok(())
}

/// Per-QC breakdown. Phase 1: MPH is real per-QC; empty/crane-wait stay NULL until
/// per-QC source SQL exists. Status left NULL until targets are signed off.
async fn breakdown_qc(pool: &PgPool, date: NaiveDate) -> Result<()> {
    sqlx::query(
        "INSERT INTO kpi_breakdown_qc
           (snapshot_date, qc_machno, jobtype, mph, empty_km, crane_wait_sec, qc_wait_sec, status, computed_at)
         SELECT m.snapshot_date, m.qc_machno, NULL,
                round(sum(m.k_mph_per_active_hour*m.active_hours)/nullif(sum(m.active_hours),0), 2),
                NULL, NULL, q.qc_wait, NULL, now()
           FROM raw_k_mph_realtime m
           LEFT JOIN (SELECT qc, avg_idle_sec AS qc_wait FROM raw_k_qc_q WHERE snapshot_date = $1) q
                  ON q.qc = m.qc_machno
          WHERE m.snapshot_date = $1
          GROUP BY m.snapshot_date, m.qc_machno, q.qc_wait
         ON CONFLICT (snapshot_date, qc_machno) DO UPDATE SET
           mph=EXCLUDED.mph, qc_wait_sec=EXCLUDED.qc_wait_sec, computed_at=now()",
    )
    .bind(date)
    .execute(pool)
    .await
    .context("kpi_breakdown_qc upsert")?;
    Ok(())
}
