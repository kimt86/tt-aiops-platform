//! Integration test for the K_UTIL TT upsert path against a real PostgreSQL.
//! Skipped unless DATABASE_URL is set (e.g. the dev container). Touches NO Oracle —
//! it feeds the golden fixture straight into the upsert. Uses a sentinel far-past
//! snapshot_date and cleans up after itself.

use chrono::NaiveDate;
use tt_extractor::{db, kpis::k_util_tt};

const SENTINEL: &str = "2000-01-01";

#[tokio::test]
async fn upsert_is_idempotent_against_pg() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL not set — skipping pg integration test");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .expect("connect dev pg");

    let date = NaiveDate::parse_from_str(SENTINEL, "%Y-%m-%d").unwrap();

    // clean any prior run of this test
    sqlx::query("DELETE FROM raw_k_util_tt WHERE snapshot_date = $1")
        .bind(date)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM etl_run_log WHERE business_date = $1 AND kpi_key = $2")
        .bind(date)
        .bind(k_util_tt::KPI_KEY)
        .execute(&pool)
        .await
        .unwrap();

    let raw = include_str!("fixtures/k_util_tt.stdout.json");
    let rows = k_util_tt::parse(raw).unwrap();
    assert_eq!(rows.len(), 3);

    // first upsert
    let run1 = db::start_run(&pool, k_util_tt::KPI_KEY, date).await.unwrap();
    let n1 = k_util_tt::upsert(&pool, date, run1, &rows).await.unwrap();
    assert_eq!(n1, 3);

    // second upsert (idempotent — same rows, different run_id)
    let run2 = db::start_run(&pool, k_util_tt::KPI_KEY, date).await.unwrap();
    let n2 = k_util_tt::upsert(&pool, date, run2, &rows).await.unwrap();
    assert_eq!(n2, 3);

    // exactly 3 rows remain (no duplication), values landed, run_id advanced
    let count: (i64,) =
        sqlx::query_as("SELECT count(*) FROM raw_k_util_tt WHERE snapshot_date = $1")
            .bind(date)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count.0, 3);

    let capped: (Option<f64>, i64) = sqlx::query_as(
        "SELECT k_util_capped::float8, run_id FROM raw_k_util_tt
          WHERE snapshot_date = $1 AND machno = 'TT1281'",
    )
    .bind(date)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(capped.0, Some(1.0));
    assert_eq!(capped.1, run2); // latest run wins

    // finish_run updates freshness
    db::finish_run(&pool, run2, k_util_tt::KPI_KEY, date, "OK", Some(3), None)
        .await
        .unwrap();
    let fresh: (String,) =
        sqlx::query_as("SELECT last_status FROM data_freshness WHERE kpi_key = $1")
            .bind(k_util_tt::KPI_KEY)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(fresh.0, "OK");

    // cleanup
    sqlx::query("DELETE FROM raw_k_util_tt WHERE snapshot_date = $1")
        .bind(date)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM etl_run_log WHERE business_date = $1 AND kpi_key = $2")
        .bind(date)
        .bind(k_util_tt::KPI_KEY)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM data_freshness WHERE kpi_key = $1")
        .bind(k_util_tt::KPI_KEY)
        .execute(&pool)
        .await
        .unwrap();
}
