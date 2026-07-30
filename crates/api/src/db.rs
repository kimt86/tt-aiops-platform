//! Read-only PostgreSQL pool. This crate has NO Oracle access by construction.

use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::postgres::{PgPool, PgPoolOptions};

pub async fn pool() -> Result<PgPool> {
    let url = std::env::var("DATABASE_URL").context("DATABASE_URL not set")?;
    PgPoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await
        .context("connecting to PostgreSQL")
}

/// Detection ceilings for the growth watchdog. These are NOT policy — they sit far above
/// anything normal, so crossing one means a write went wrong, not that we got busy.
/// Measured 2026-07-28: whole DB 5.7GB, largest table (truck_pos_hifreq) 830MB.
const DB_WARN_BYTES: i64 = 50 * 1024 * 1024 * 1024; // ~9x today
const TABLE_WARN_BYTES: i64 = 10 * 1024 * 1024 * 1024; // ~12x the largest table today
/// Filesystem holding both the API's working directory and the Postgres volume (same /dev/sda2).
/// Measured 2026-07-28: 142GiB free of 878GB, and our own growth is ~17MB/day — so this fires
/// either for a runaway write or for someone else filling the shared box. Both are worth knowing:
/// a full filesystem stops Postgres, and then nothing else in this file can report anything.
const DISK_FREE_CRIT_BYTES: u64 = 20 * 1024 * 1024 * 1024;
/// 2 minutes, not 30. No interval catches a runaway write in flight — 10M rows inserts in 6.3s
/// (measured) — so the interval only decides how long a COMPLETED blowup goes unnoticed. 30
/// minutes was an arbitrary number; these are three cheap queries.
const SIZE_CHECK_EVERY: Duration = Duration::from_secs(120);
/// Prune ops_alert itself every Nth cycle (30 min at the interval above).
const ALERT_PRUNE_EVERY_N: u32 = 15;

/// Dead-man checks: "this table should have gained a row recently". They answer the two questions
/// the 2026-07-28 timeline could not: the road graph ran degraded for five days, and
/// road_route_eval went three full days with zero rows (07-17..19) — and nothing said either
/// thing out loud.
///
/// Every entry is GATED on the GPS feed being fresh. When the upstream feed dies (the 2026-07-16
/// cable cut) all of these would fire at once and add nothing — the feed banner already covers
/// that. Gating keeps this signal meaning "a job of OURS is broken".
const DEADMAN: &[(&str, &str, i64)] = &[
    // table, timestamp column, alert if the newest row is older than N minutes
    ("congestion_edge", "hour", 240), // hourly cron; `hour` lags ~1-2h by design, so 4h = 2+ misses
    ("road_route_eval", "ts", 90),    // spawn_roadgraph_eval, 10min → 9 misses
    ("stage2_match_shadow", "ts", 30), // spawn_stage2_shadow, 60s → 30 misses
    // The two crane handover logs. These are TOS ground truth for every cycle timestamp, they poll
    // every 60s, and until now NOTHING watched them: absent from DEADMAN and RETENTION, and no code
    // alerts on etl_run_log.status='FAILED' or data_freshness.last_status (the only reader is a plain
    // GET endpoint). A stalled move poll is therefore silent — and it does not stay merely late:
    // populate_cycle_pred_shadow works a 90-minute window and spawn_cycle_pickup_correct a 2-hour
    // one, so past those, rows are lost permanently rather than caught up. On comp_ts, not
    // captured_at: comp_ts is indexed so max() is an index scan, while captured_at would seq-scan
    // 509MB every cycle. Both tables gain 36-68 rows per minute around the clock, so 30 min of
    // nothing means our poll is broken, not that the terminal went quiet.
    ("qc_move_log", "comp_ts", 30),
    ("rtg_move_log", "comp_ts", 30),
];

/// Retention checks: the prune ran without error but did not actually delete anything. A prune
/// that silently no-ops is the same blindness as one that silently fails, and the far end of it
/// is a consumer reading a table far past its design size — which is exactly what the road-network
/// raster does (it reads whole tables). Thresholds are ~1.6x the design window so normal lag
/// never fires.
const RETENTION: &[(&str, &str, i64)] = &[
    // table, timestamp column, alert if the OLDEST row is older than N days (design window)
    ("truck_pos_hifreq", "ts", 8),      // 5d
    ("truck_pos_hist", "ts", 4),        // 2d
    ("zone_density", "ts", 7),          // 4d
    ("stage2_match_shadow", "ts", 34),  // 21d
];

/// Raise (or refresh) a persisted operational alert — see mig 0107.
///
/// Returns true only on the FIRST occurrence, so callers log the edge instead of every cycle: the
/// DB row carries the state, the log only needs to mark the transition.
///
/// There is deliberately no ack. Consumers show alerts whose `last_ts` is recent, so a condition
/// that clears stops being refreshed and drops off the banner by itself. Manual acks get
/// neglected, and a neglected crit becomes a permanent banner — which is how alert plumbing stops
/// being read at all.
pub async fn alert(
    pool: &PgPool,
    source: &str,
    subject: &str,
    severity: &str,
    message: &str,
    detail: Option<&str>,
) -> bool {
    let r: Result<Option<(i64,)>, _> = sqlx::query_as(
        "INSERT INTO ops_alert (source, subject, severity, message, detail)
              VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (source, subject) DO UPDATE
            SET last_ts = now(), occurrences = ops_alert.occurrences + 1,
                severity = EXCLUDED.severity, message = EXCLUDED.message, detail = EXCLUDED.detail
         RETURNING occurrences",
    )
    .bind(source)
    .bind(subject)
    .bind(severity)
    .bind(message)
    .bind(detail)
    .fetch_optional(pool)
    .await;
    match r {
        Ok(Some((n,))) => n == 1,
        // the row cap swallowed a brand-new key; the cap itself raises its own alert
        Ok(None) => {
            tracing::warn!(source, subject, "alert dropped by the ops_alert row cap");
            false
        }
        Err(e) => {
            // The alert path itself is down. Only the log is left — and if Postgres is the reason,
            // the feed banner (cause='api') is the channel that still reaches a person.
            tracing::error!(source, subject, error = %e, "RAISING AN ALERT FAILED");
            false
        }
    }
}

/// Bytes free on the filesystem holding `path`, or None if it cannot be read.
///
/// PostgreSQL exposes no free-space function and /proc does not carry it either, so this is a
/// direct statvfs. `libc` is already in the tree (tokio → signal-hook-registry → errno), so this
/// adds no download and no new version to resolve.
fn fs_free_bytes(path: &str) -> Option<u64> {
    let c = std::ffi::CString::new(path).ok()?;
    // SAFETY: `c` outlives the call and is NUL-terminated; `st` is an owned, correctly sized
    // buffer that statvfs only writes into. Return value is checked before `st` is read.
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return None;
        }
        Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
    }
}

/// Watchdog for the failure class a memory cap cannot contain: a write whose ROW COUNT is
/// decided by unvalidated input.
///
/// 2026-07-28 produced both twins of this class. The in-memory twin — an array sized by the
/// farthest GPS fix — OOM-killed the box, and `MemoryMax=` on every unit now bounds it. The DB
/// twin — `generate_series(1, tier)` with `tier` straight off Oracle — is not bounded by any of
/// that: the insert is executed by Postgres, which lives in a different cgroup, so a process
/// memory cap does nothing; the damage lands on disk and WAL; and it is merely large rather than
/// slow, so a statement timeout would not catch it either.
///
/// Physical bounds at the write site are the fix — MAX_TIER in code, CHECK constraints in the
/// schema (mig 0106) so backfills and manual SQL are bound too. This is the detector for the
/// site nobody has bounded yet. It cannot prevent the write; it makes the growth impossible to
/// miss, which is precisely what failed on 2026-07-28 — the road graph ran degraded for five
/// days and nothing said so.
pub fn spawn_size_watchdog(pool: PgPool) {
    tokio::spawn(async move {
        let mut cycle: u32 = 0;
        loop {
            cycle = cycle.wrapping_add(1);

            // ── whole-database size ──
            match sqlx::query_as::<_, (i64,)>("SELECT pg_database_size(current_database())")
                .fetch_one(&pool)
                .await
            {
                Ok((n,)) if n > DB_WARN_BYTES => {
                    let msg = format!(
                        "데이터베이스가 상한을 넘었다 — {}GB > {}GB",
                        n / 1_073_741_824,
                        DB_WARN_BYTES / 1_073_741_824
                    );
                    if alert(&pool, "size_watchdog", "database", "crit", &msg, None).await {
                        tracing::warn!(gb = n / 1_073_741_824, "DATABASE OVER SIZE CEILING");
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "size watchdog: database size query failed"),
            }

            // ── per-table size ──
            let big: Vec<(String, i64)> = sqlx::query_as(
                "SELECT schemaname || '.' || relname, pg_total_relation_size(relid)
                   FROM pg_stat_all_tables
                  WHERE schemaname NOT IN ('pg_catalog', 'information_schema')
                    AND pg_total_relation_size(relid) > $1
                  ORDER BY 2 DESC",
            )
            .bind(TABLE_WARN_BYTES)
            .fetch_all(&pool)
            .await
            .unwrap_or_default();
            for (t, n) in big {
                let msg = format!("테이블이 상한을 넘었다 — {}GB", n / 1_073_741_824);
                if alert(&pool, "size_watchdog", &t, "crit", &msg, None).await {
                    tracing::warn!(table = %t, gb = n / 1_073_741_824, "TABLE OVER SIZE CEILING");
                }
            }

            // ── disk headroom ──
            if let Some(free) = fs_free_bytes(".") {
                if free < DISK_FREE_CRIT_BYTES {
                    let msg = format!("디스크 여유 {}GiB 미만 — Postgres 가 멈추면 알림 자체가 죽는다", free / 1_073_741_824);
                    if alert(&pool, "disk", "filesystem", "crit", &msg, None).await {
                        tracing::warn!(free_gib = free / 1_073_741_824, "DISK NEARLY FULL");
                    }
                }
            }

            // ── dead-man + retention, gated on the GPS feed actually being alive ──
            // A dead upstream makes every one of these fire and say nothing new; the feed banner
            // already covers that case, so gating keeps the meaning "one of OUR jobs is broken".
            let feed_age_s: Option<f64> = sqlx::query_scalar(
                "SELECT extract(epoch FROM now() - max(ts))::float8 FROM truck_pos_hifreq",
            )
            .fetch_one(&pool)
            .await
            .ok()
            .flatten();
            let feed_fresh = feed_age_s.is_some_and(|s| s < 900.0);

            if feed_fresh {
                for (table, col, max_age_min) in DEADMAN {
                    let age: Option<f64> = sqlx::query_scalar(&format!(
                        "SELECT extract(epoch FROM now() - max({col}))::float8 / 60.0 FROM {table}"
                    ))
                    .fetch_one(&pool)
                    .await
                    .ok()
                    .flatten();
                    let Some(age_min) = age else { continue };
                    if age_min > *max_age_min as f64 {
                        let msg = format!(
                            "{table} 이 {:.0}분간 새 행이 없다(허용 {max_age_min}분) — 이 표를 쓰는 작업이 멈췄다",
                            age_min
                        );
                        if alert(&pool, "deadman", table, "crit", &msg, None).await {
                            tracing::warn!(table = %table, age_min, "DEAD-MAN: table stopped gaining rows");
                        }
                    }
                }

                for (table, col, max_age_days) in RETENTION {
                    let age: Option<f64> = sqlx::query_scalar(&format!(
                        "SELECT extract(epoch FROM now() - min({col}))::float8 / 86400.0 FROM {table}"
                    ))
                    .fetch_one(&pool)
                    .await
                    .ok()
                    .flatten();
                    let Some(age_d) = age else { continue };
                    if age_d > *max_age_days as f64 {
                        let msg = format!(
                            "{table} 최고령 행이 {:.1}일(허용 {max_age_days}일) — 보존정책이 오류 없이 아무것도 안 지우고 있다",
                            age_d
                        );
                        if alert(&pool, "retention_stuck", table, "warn", &msg, None).await {
                            tracing::warn!(table = %table, age_d, "RETENTION NOT DELETING");
                        }
                    }
                }
            }

            // ops_alert is itself a rolling table — bound it like every other one.
            if cycle % ALERT_PRUNE_EVERY_N == 0 {
                prune(
                    &pool,
                    "ops_alert",
                    "DELETE FROM ops_alert WHERE last_ts < now() - interval '30 days'",
                )
                .await;
            }

            tokio::time::sleep(SIZE_CHECK_EVERY).await;
        }
    });
}

/// Run a retention prune, surfacing failure instead of swallowing it.
///
/// These pruners are the only thing keeping every rolling table bounded, and they used to be
/// written `let _ = sqlx::query("DELETE ...").execute(&pool).await;` — which discards the error
/// completely. A prune could then fail every tick for weeks with nothing logged and nothing
/// alerting; the first symptom would be a downstream job reading a table far past its design
/// size. That is the shape of the 2026-07-28 OOM (an array sized by its input), so failure is
/// logged loudly here and the deleted count is available at debug level.
/// A failure here is also persisted as an alert (mig 0107): a warn line alone reaches nobody,
/// which is the gap the 2026-07-28 audit found in the first version of this function.
pub async fn prune(pool: &PgPool, table: &str, sql: &str) {
    match sqlx::query(sql).execute(pool).await {
        Ok(r) => tracing::debug!(table, deleted = r.rows_affected(), "retention prune"),
        Err(e) => {
            let msg = format!("{table} 보존정책 삭제가 실패했다 — 이 표는 무한히 자란다");
            if alert(pool, "retention_prune", table, "crit", &msg, Some(&e.to_string())).await {
                tracing::warn!(table, error = %e, "RETENTION PRUNE FAILED");
            }
        }
    }
}

/// Latest business date present in kpi_daily (used when ?as_of is omitted).
pub async fn latest_as_of(pool: &PgPool) -> Result<Option<chrono::NaiveDate>> {
    let row: Option<(chrono::NaiveDate,)> =
        sqlx::query_as("SELECT max(snapshot_date) FROM kpi_daily")
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|r| r.0))
}
