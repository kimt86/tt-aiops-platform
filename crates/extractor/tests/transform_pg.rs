//! Integration test for L0->L1 transform formulas against real PostgreSQL.
//! Seeds known L0 rows for a sentinel date, runs the transform, asserts the
//! headline kpi_daily values, then cleans up. Skipped unless DATABASE_URL is set.

use chrono::NaiveDate;
use tt_extractor::transform;

const SENTINEL: &str = "2001-02-03";

#[tokio::test]
async fn transform_formulas_against_pg() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL not set — skipping");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let date = NaiveDate::parse_from_str(SENTINEL, "%Y-%m-%d").unwrap();

    cleanup(&pool, date).await;

    // a run_id for the FK
    let run_id: (i64,) = sqlx::query_as(
        "INSERT INTO etl_run_log (kpi_key, business_date, status) VALUES ('TEST',$1,'OK') RETURNING run_id",
    )
    .bind(date).fetch_one(&pool).await.unwrap();
    let run_id = run_id.0;

    // K_UTIL: two TTs at 0.90 and 1.00 -> avg 0.95 -> 95.0 %
    for (m, u) in [("TT1", 0.90_f64), ("TT2", 1.00)] {
        sqlx::query("INSERT INTO raw_k_util_tt (snapshot_date,machno,k_util_capped,run_id) VALUES ($1,$2,$3,$4)")
            .bind(date).bind(m).bind(u).bind(run_id).execute(&pool).await.unwrap();
    }
    // K_EMPTY: empty 600+400=1000, laden 400+600=1000 -> ratio 50%, jobs 20,
    //          km/job = 1000/20/1000 = 0.05
    for (jt, sh, e, l, j) in [("LD","Day",600.0_f64,400.0_f64,10_i32), ("DS","Night",400.0,600.0,10)] {
        sqlx::query("INSERT INTO raw_k_empty (snapshot_date,jobtype,shift,jobs,total_empty_m,total_laden_m,run_id) VALUES ($1,$2,$3,$4,$5,$6,$7)")
            .bind(date).bind(jt).bind(sh).bind(j).bind(e).bind(l).bind(run_id).execute(&pool).await.unwrap();
    }

    transform::run(&pool, date).await.unwrap();

    let got = |k: &'static str| {
        let pool = pool.clone();
        async move {
            let r: (f64, Option<i32>) = sqlx::query_as(
                "SELECT value::float8, sample_n FROM kpi_daily WHERE kpi_key=$1 AND snapshot_date=$2",
            )
            .bind(k).bind(date).fetch_one(&pool).await.unwrap();
            r
        }
    };

    let (util, util_n) = got("K_UTIL").await;
    assert!((util - 95.0).abs() < 1e-6, "K_UTIL={util}");
    assert_eq!(util_n, Some(2));

    let (er, er_n) = got("K_EMPTY_R").await;
    assert!((er - 50.0).abs() < 1e-6, "K_EMPTY_R={er}");
    assert_eq!(er_n, Some(20));

    let (empty, _) = got("K_EMPTY").await;
    assert!((empty - 0.05).abs() < 1e-6, "K_EMPTY={empty}");

    cleanup(&pool, date).await;
}

async fn cleanup(pool: &sqlx::PgPool, date: NaiveDate) {
    for t in ["kpi_daily", "raw_k_util_tt", "raw_k_empty"] {
        let col = if t == "kpi_daily" { "snapshot_date" } else { "snapshot_date" };
        let _ = sqlx::query(&format!("DELETE FROM {t} WHERE {col}=$1")).bind(date).execute(pool).await;
    }
    let _ = sqlx::query("DELETE FROM etl_run_log WHERE business_date=$1 AND kpi_key='TEST'")
        .bind(date).execute(pool).await;
}
