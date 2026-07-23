//! Transform L1 (kpi_daily history) -> L2 (kpi_baseline). For one as-of date and
//! each KPI: 4-week baseline mean, delta vs the as-of value, and a Welch two-sample
//! significance test of the recent window vs the baseline window. No Oracle access.
//!
//! Windowing (tunable, plan open-decision #3 — using daily-series two-sample as the
//! pragmatic phase-1 choice instead of vessel-paired):
//!   baseline window = [as_of-28, as_of-1]   (the 4 weeks before)
//!   recent window   = [as_of-6,  as_of]      (the last 7 days)

use anyhow::{Context, Result};
use chrono::{Duration, NaiveDate};
use sqlx::PgPool;
use tt_core::kpi::KpiKey;
use tt_core::stats::welch_t_test;

const BASELINE_DAYS: i64 = 28;
const RECENT_DAYS: i64 = 7;

pub async fn run(pool: &PgPool, as_of: NaiveDate) -> Result<()> {
    for kpi in KpiKey::ALL {
        compute_one(pool, kpi, as_of).await?;
    }
    tracing::info!(%as_of, "transform L1->L2 (baseline) done");
    Ok(())
}

async fn series(
    pool: &PgPool,
    kpi: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<f64>> {
    let rows: Vec<(f64,)> = sqlx::query_as(
        "SELECT value::float8 FROM kpi_daily
          WHERE kpi_key = $1 AND snapshot_date BETWEEN $2 AND $3
          ORDER BY snapshot_date",
    )
    .bind(kpi)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .with_context(|| format!("reading kpi_daily series for {kpi}"))?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

async fn compute_one(pool: &PgPool, kpi: KpiKey, as_of: NaiveDate) -> Result<()> {
    let key = kpi.as_str();

    // the as-of headline value (skip if today's value isn't computed yet)
    let today: Option<(f64,)> = sqlx::query_as(
        "SELECT value::float8 FROM kpi_daily WHERE kpi_key=$1 AND snapshot_date=$2",
    )
    .bind(key)
    .bind(as_of)
    .fetch_optional(pool)
    .await?;
    let Some((today_val,)) = today else { return Ok(()) };

    let base = series(
        pool,
        key,
        as_of - Duration::days(BASELINE_DAYS),
        as_of - Duration::days(1),
    )
    .await?;
    let recent = series(pool, key, as_of - Duration::days(RECENT_DAYS - 1), as_of).await?;

    let baseline_value = if base.is_empty() {
        None
    } else {
        Some(base.iter().sum::<f64>() / base.len() as f64)
    };
    let baseline_n_days = base.len() as i32;

    let (delta_abs, delta_pct) = match baseline_value {
        Some(b) if b != 0.0 => (Some(today_val - b), Some((today_val - b) / b * 100.0)),
        Some(b) => (Some(today_val - b), None),
        None => (None, None),
    };

    let test = welch_t_test(&recent, &base);
    let p_value = test.as_ref().map(|t| t.p_value);
    let cohens_d = test.as_ref().map(|t| t.cohens_d);
    let is_significant = p_value.map(|p| p < 0.05);

    // targets are NULL until sign-off, so meets_* stays NULL for now
    sqlx::query(
        "INSERT INTO kpi_baseline
           (kpi_key, as_of_date, baseline_value, baseline_n_days, delta_abs, delta_pct,
            p_value, cohens_d, is_significant, meets_target, meets_excellent, computed_at)
         SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9, mt.meets_target, mt.meets_excellent, now()
           FROM (
             SELECT
               CASE WHEN t.target_value IS NULL THEN NULL
                    WHEN t.direction='LOWER_BETTER'  THEN $10 <= t.target_value
                    ELSE $10 >= t.target_value END AS meets_target,
               CASE WHEN t.excellent_value IS NULL THEN NULL
                    WHEN t.direction='LOWER_BETTER'  THEN $10 <= t.excellent_value
                    ELSE $10 >= t.excellent_value END AS meets_excellent
             FROM kpi_target t WHERE t.kpi_key = $1
           ) mt
         ON CONFLICT (kpi_key, as_of_date) DO UPDATE SET
           baseline_value=EXCLUDED.baseline_value, baseline_n_days=EXCLUDED.baseline_n_days,
           delta_abs=EXCLUDED.delta_abs, delta_pct=EXCLUDED.delta_pct, p_value=EXCLUDED.p_value,
           cohens_d=EXCLUDED.cohens_d, is_significant=EXCLUDED.is_significant,
           meets_target=EXCLUDED.meets_target, meets_excellent=EXCLUDED.meets_excellent,
           computed_at=now()",
    )
    .bind(key)
    .bind(as_of)
    .bind(baseline_value)
    .bind(baseline_n_days)
    .bind(delta_abs)
    .bind(delta_pct)
    .bind(p_value)
    .bind(cohens_d)
    .bind(is_significant)
    .bind(today_val)
    .execute(pool)
    .await
    .with_context(|| format!("kpi_baseline upsert for {key}"))?;

    Ok(())
}
