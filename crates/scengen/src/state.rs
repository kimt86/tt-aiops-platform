//! The observe/command contract for the scenario subsystem. scengen is the only writer;
//! crates/api reads these tables for the /sim UI. All JSONB is bound as text + `::jsonb`
//! cast so we don't need the sqlx `json` feature (which would change the shared workspace).

use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;

/// scenario.config singleton (kill switch + tuning). Read at the start of every tick.
#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    pub chunk_minutes: i32,
    pub offpeak_only: bool,
    pub offpeak_start_h: i16,
    pub offpeak_end_h: i16,
    pub oracle_timeout_s: i32,
    pub retention_days: i32,
}

pub async fn load_config(pool: &PgPool) -> Result<Config> {
    let row: (bool, i32, bool, i16, i16, i32, i32) = sqlx::query_as(
        "SELECT enabled, chunk_minutes, offpeak_only, offpeak_start_h, offpeak_end_h,
                oracle_timeout_s, retention_days
           FROM scenario.config WHERE id = 1",
    )
    .fetch_one(pool)
    .await
    .context("reading scenario.config")?;
    Ok(Config {
        enabled: row.0,
        chunk_minutes: row.1,
        offpeak_only: row.2,
        offpeak_start_h: row.3,
        offpeak_end_h: row.4,
        oracle_timeout_s: row.5,
        retention_days: row.6,
    })
}

pub async fn start_run(pool: &PgPool, kind: &str) -> Result<i64> {
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO scenario.gen_run (kind, state) VALUES ($1, 'running') RETURNING run_id",
    )
    .bind(kind)
    .fetch_one(pool)
    .await
    .context("inserting scenario.gen_run")?;
    Ok(row.0)
}

/// Append one row to the run-state event stream.
pub async fn emit(pool: &PgPool, run_id: i64, level: &str, kind: &str, payload: Value) -> Result<()> {
    sqlx::query(
        "INSERT INTO scenario.gen_event (run_id, level, kind, payload)
         VALUES ($1, $2, $3, $4::jsonb)",
    )
    .bind(run_id)
    .bind(level)
    .bind(kind)
    .bind(payload.to_string())
    .execute(pool)
    .await
    .context("inserting scenario.gen_event")?;
    Ok(())
}

pub async fn set_phase(pool: &PgPool, run_id: i64, phase: &str) -> Result<()> {
    sqlx::query("UPDATE scenario.gen_run SET phase = $2, updated_at = now() WHERE run_id = $1")
        .bind(run_id)
        .bind(phase)
        .execute(pool)
        .await
        .context("set_phase")?;
    Ok(())
}

/// Merge a JSON patch into one snapshot column (whitelisted): progress|load_stats|collection|health.
pub async fn merge_json(pool: &PgPool, run_id: i64, column: &str, patch: Value) -> Result<()> {
    let col = match column {
        "progress" | "load_stats" | "collection" | "health" => column,
        other => anyhow::bail!("unknown snapshot column '{other}'"),
    };
    // `col` is from a fixed whitelist above, so this format! is injection-safe.
    let sql =
        format!("UPDATE scenario.gen_run SET {col} = {col} || $2::jsonb, updated_at = now() WHERE run_id = $1");
    sqlx::query(&sql)
        .bind(run_id)
        .bind(patch.to_string())
        .execute(pool)
        .await
        .with_context(|| format!("merging {col}"))?;
    Ok(())
}

pub async fn finish_run(pool: &PgPool, run_id: i64, state: &str, error: Option<&str>) -> Result<()> {
    sqlx::query(
        "UPDATE scenario.gen_run
            SET state = $2, error_text = $3, finished_at = now(), updated_at = now()
          WHERE run_id = $1",
    )
    .bind(run_id)
    .bind(state)
    .bind(error)
    .execute(pool)
    .await
    .context("finish_run")?;
    Ok(())
}

/// Watermark = last collected TOS event as MYT text "YYYYMMDDHHMMSS" (lexicographic order),
/// matching JOB_HIST_DATE||JOB_HIST_TIME. Stored as text to avoid TZ round-trip bugs at the
/// window boundary (same approach as the extractor's etl_watermark).
pub async fn get_watermark(pool: &PgPool, source: &str) -> Result<Option<String>> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT cursor_evt FROM scenario.watermark WHERE source = $1")
            .bind(source)
            .fetch_optional(pool)
            .await
            .context("get_watermark")?;
    Ok(row.and_then(|r| r.0))
}

pub async fn set_watermark(pool: &PgPool, source: &str, cursor_evt: &str) -> Result<()> {
    // GREATEST guards against a regression if two ticks race.
    sqlx::query(
        "INSERT INTO scenario.watermark (source, cursor_evt, updated_at)
         VALUES ($1, $2, now())
         ON CONFLICT (source) DO UPDATE
           SET cursor_evt = GREATEST(scenario.watermark.cursor_evt, EXCLUDED.cursor_evt),
               updated_at = now()",
    )
    .bind(source)
    .bind(cursor_evt)
    .execute(pool)
    .await
    .context("set_watermark")?;
    Ok(())
}
