//! Integration test for `rollup_today_from_shifts` against real PostgreSQL.
//! Seeds known kpi_shift rows for a sentinel date's shifts, runs the Postgres-only
//! rollup, and asserts the day value = Σ(value·weight)/Σ(weight), that K_UTIL is
//! excluded, that the COALESCE(agg_weight, sample_n) fallback works, and that an
//! authoritative (non-provisional) row is never clobbered. Skipped without DATABASE_URL.

use chrono::NaiveDate;
use wp_extractor::transform;

const SENTINEL: &str = "2001-02-04";

async fn seed_shift(
    pool: &sqlx::PgPool, date: NaiveDate, shift: &str, kpi: &str,
    value: f64, sample_n: i32, agg_weight: Option<f64>,
) {
    sqlx::query(
        "INSERT INTO kpi_shift (business_date, shift, kpi_key, value, sample_n, agg_weight, unit, as_of_ts, window_start)
         VALUES ($1,$2,$3,$4,$5,$6,'x', now(), now())
         ON CONFLICT (business_date, shift, kpi_key) DO UPDATE SET
           value=EXCLUDED.value, sample_n=EXCLUDED.sample_n, agg_weight=EXCLUDED.agg_weight",
    )
    .bind(date).bind(shift).bind(kpi).bind(value).bind(sample_n).bind(agg_weight)
    .execute(pool).await.unwrap();
}

async fn day_value(pool: &sqlx::PgPool, kpi: &str, date: NaiveDate) -> Option<(f64, bool, String)> {
    sqlx::query_as::<_, (f64, bool, String)>(
        "SELECT value::float8, is_provisional, source_grain FROM kpi_daily WHERE kpi_key=$1 AND snapshot_date=$2",
    )
    .bind(kpi).bind(date).fetch_optional(pool).await.unwrap()
}

#[tokio::test]
async fn rollup_today_from_shifts_against_pg() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL not set — skipping");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let date = NaiveDate::parse_from_str(SENTINEL, "%Y-%m-%d").unwrap();
    cleanup(&pool, date).await;

    // K_CYCLE: exact, agg_weight present both shifts. (2000·100 + 3000·300)/400 = 2750
    seed_shift(&pool, date, "D", "K_CYCLE", 2000.0, 100, Some(100.0)).await;
    seed_shift(&pool, date, "E", "K_CYCLE", 3000.0, 300, Some(300.0)).await;
    // K_MPH: weight ≠ sample_n. (20·10 + 30·30)/40 = 27.5
    seed_shift(&pool, date, "D", "K_MPH", 20.0, 5, Some(10.0)).await;
    seed_shift(&pool, date, "E", "K_MPH", 30.0, 7, Some(30.0)).await;
    // K_RTG_Q: COALESCE fallback — D has NULL weight, falls back to sample_n=100.
    //            (500·100 + 700·300)/400 = 650
    seed_shift(&pool, date, "D", "K_RTG_Q", 500.0, 100, None).await;
    seed_shift(&pool, date, "E", "K_RTG_Q", 700.0, 300, Some(300.0)).await;
    // K_UTIL: avg-of-ratios — must be EXCLUDED from the rollup entirely.
    seed_shift(&pool, date, "D", "K_UTIL", 90.0, 50, None).await;
    seed_shift(&pool, date, "E", "K_UTIL", 80.0, 50, None).await;

    transform::rollup_today_from_shifts(&pool, date).await.unwrap();

    let (cyc, prov, grain) = day_value(&pool, "K_CYCLE", date).await.expect("K_CYCLE row");
    assert!((cyc - 2750.0).abs() < 1e-3, "K_CYCLE={cyc}");
    assert!(prov, "rollup rows must be provisional");
    assert_eq!(grain, "shift-rollup");

    let (mph, _, _) = day_value(&pool, "K_MPH", date).await.expect("K_MPH row");
    assert!((mph - 27.5).abs() < 1e-3, "K_MPH={mph}");

    let (crane, _, _) = day_value(&pool, "K_RTG_Q", date).await.expect("K_RTG_Q row");
    assert!((crane - 650.0).abs() < 1e-3, "K_RTG_Q={crane}");

    assert!(day_value(&pool, "K_UTIL", date).await.is_none(), "K_UTIL must be excluded from rollup");

    // No-clobber: an authoritative (nightly) row must survive a later rollup.
    sqlx::query(
        "INSERT INTO kpi_daily (kpi_key, snapshot_date, value, sample_n, unit, source_grain, is_provisional)
         VALUES ('K_EMPTY',$1, 9.99, 1, 'km/Job', 'nightly', false)",
    ).bind(date).execute(&pool).await.unwrap();
    seed_shift(&pool, date, "D", "K_EMPTY", 1.0, 10, Some(10.0)).await;
    seed_shift(&pool, date, "E", "K_EMPTY", 2.0, 10, Some(10.0)).await;
    transform::rollup_today_from_shifts(&pool, date).await.unwrap();
    let (empty, eprov, egrain) = day_value(&pool, "K_EMPTY", date).await.unwrap();
    assert!((empty - 9.99).abs() < 1e-6, "authoritative K_EMPTY clobbered: {empty}");
    assert!(!eprov && egrain == "nightly", "authoritative flag/grain changed");

    cleanup(&pool, date).await;
}

async fn cleanup(pool: &sqlx::PgPool, date: NaiveDate) {
    let _ = sqlx::query("DELETE FROM kpi_daily WHERE snapshot_date=$1").bind(date).execute(pool).await;
    let _ = sqlx::query("DELETE FROM kpi_shift WHERE business_date=$1").bind(date).execute(pool).await;
}
