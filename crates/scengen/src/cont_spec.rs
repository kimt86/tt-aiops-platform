//! Container size lookup -> scenario.container_spec, so landside volume can be reported in TEU.
//!
//! Asks TOSADM.CYY_CONTAINER for the ISO code of boxes we have just seen cross the gate and do not
//! already know. Lookups go by container number (the PK's leading column), so each is an index seek:
//! 200 numbers came back in 2.08s including the ~0.9s SSH round trip.
//!
//! ★WHY IT MUST RUN PROMPTLY. CYY_CONTAINER is CURRENT yard inventory — a box that has left is
//! simply not there. Measured hit rate: 91.4% for containers seen within 3 hours, 77% within 3 days.
//! So this is deliberately a small, frequent job rather than a big catch-up one, and it does NOT
//! backfill: chasing old unknowns would spend Oracle on exactly the population least likely to
//! answer. Windows predating this collector keep whatever the manifest seed gave them, and the
//! scenario publishes a coverage percentage so a partial total is never read as the whole.
//!
//! Volume, measured: ~9,000 gate moves a day covering ~8,000 distinct boxes, of which ~4,000 are
//! unknown — four or five batches a day, and falling, because a box only ever has to be asked about
//! once in its life.

use anyhow::Result;
use serde_json::{json, Value};
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::state::{self, Config};
use crate::toolbox::Toolbox;
use crate::util::jstr;

/// Containers per Oracle round trip. Same shape the gate collector uses against the same kind of
/// IN-list seek.
const BATCH: i64 = 1000;
/// How far back to consider gate moves. This bound is also the retry policy: a box that was already
/// gone from the yard when we asked stays unknown and gets a few more chances while it is inside the
/// window, then falls out on its own — no miss table, no permanent re-asking. Wide enough that a
/// short outage self-heals, narrow enough that it can never turn into a historical sweep.
const LOOKBACK_H: i64 = 12;

pub async fn run(pool: &PgPool, target: &str) -> Result<()> {
    let cfg = state::load_config(pool).await?;
    if !cfg.enabled {
        tracing::info!("scenario collection disabled (kill switch) — skipping container-spec");
        return Ok(());
    }
    let run_id = state::start_run(pool, "cont_spec").await?;
    match tick(pool, run_id, target, &cfg).await {
        Ok(()) => state::finish_run(pool, run_id, "done", None).await?,
        Err(e) => {
            tracing::error!(error = %e, "scenario container-spec failed (isolated — others unaffected)");
            let _ = state::emit(pool, run_id, "error", "cont_spec_failed", json!({ "error": e.to_string() })).await;
            let _ = state::finish_run(pool, run_id, "error", Some(&e.to_string())).await;
        }
    }
    Ok(()) // always Ok: non-critical subsystem must not cascade
}

async fn tick(pool: &PgPool, run_id: i64, target: &str, cfg: &Config) -> Result<()> {
    state::set_phase(pool, run_id, "extract").await?;

    // Newest first: a box seen minutes ago is far more likely to still be in the yard than one from
    // this morning, and when a batch cannot hold everything the fresh end is the half worth spending
    // the round trip on.
    let todo: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT ON (r.contno) r.contno
           FROM rtg_move_log r
          WHERE r.comp_ts > now() - make_interval(hours => $1)
            AND r.jobtype IN ('GI','GO')
            AND NOT EXISTS (SELECT 1 FROM scenario.container_spec s WHERE s.contno = r.contno)
          ORDER BY r.contno, r.comp_ts DESC
          LIMIT $2",
    )
    .bind(LOOKBACK_H as i32)
    .bind(BATCH)
    .fetch_all(pool)
    .await?;

    if todo.is_empty() {
        // The steady state once the dictionary has filled: no Oracle call at all, so an idle tick
        // costs nothing and there is no reason to ever turn this off.
        state::merge_json(pool, run_id, "load_stats", json!({ "queries": 0 })).await?;
        state::merge_json(pool, run_id, "collection", json!({ "asked": 0, "note": "nothing unknown" })).await?;
        tracing::info!("container-spec: nothing to look up");
        return Ok(());
    }

    let in_list = todo
        .iter()
        .map(|(c,)| format!("'{}'", c.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT CYY_CONT_CONTNO AS contno, CYY_CONT_ISO AS iso, CYY_CONT_CONTTYPE AS ctype
           FROM TOSADM.CYY_CONTAINER
          WHERE CYY_CONT_CONTNO IN ({in_list})"
    );

    let t0 = std::time::Instant::now();
    let raw = Toolbox::from_env(target, cfg.oracle_timeout_s as u64)?.run_sql(&sql).await?;
    let query_ms = t0.elapsed().as_millis() as i64;
    let rows: Vec<Value> = parse_rows(&raw)?;
    let found = rows.len();

    state::set_phase(pool, run_id, "assemble").await?;
    let mut tx = pool.begin().await?;
    let mut stored = 0u64;
    let mut no_size = 0u64;
    for r in &rows {
        let Some(contno) = jstr(r, "CONTNO") else { continue };
        let iso = jstr(r, "ISO").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        // size is derived by the SQL function so the mapping lives in exactly one place (mig 0114),
        // and it yields NULL for an unrecognised code rather than a guess — a wrong size silently
        // doubles or halves a TEU total.
        let res = sqlx::query(
            "INSERT INTO scenario.container_spec (contno, iso, size, conttype, source)
             VALUES ($1, $2, scenario.iso_size($2), $3, 'yard')
             ON CONFLICT (contno) DO NOTHING",
        )
        .bind(contno.trim())
        .bind(iso.as_deref())
        .bind(jstr(r, "CTYPE").as_deref().map(str::trim).filter(|s| !s.is_empty()))
        .execute(&mut *tx)
        .await?;
        stored += res.rows_affected();
        if iso.as_deref().and_then(iso_size).is_none() {
            no_size += 1;
        }
    }
    tx.commit().await?;

    let missing = todo.len() - found;
    state::merge_json(pool, run_id, "load_stats", json!({
        "queries": 1, "rows_read": found, "query_ms": query_ms,
    })).await?;
    state::merge_json(pool, run_id, "collection", json!({
        "asked": todo.len(), "found": found, "stored": stored,
        "not_in_yard": missing, "unmapped_iso": no_size,
    })).await?;
    // An ISO code we cannot map is worth hearing about: it means the mapping in mig 0114 has met a
    // code the manifest data never contained, and those boxes drop out of the TEU total silently.
    if no_size > 0 {
        state::emit(pool, run_id, "warn", "iso_unmapped", json!({ "containers": no_size })).await?;
    }

    tracing::info!(asked = todo.len(), found, stored, missing, query_ms, "container spec");
    Ok(())
}

/// Mirror of scenario.iso_size() for the local warn count only — the stored value always comes from
/// the SQL function so there is one authority for the mapping.
fn iso_size(iso: &str) -> Option<&'static str> {
    match iso.chars().next()? {
        '2' => Some("twenty"),
        '4' | '9' | '5' | '1' | 'P' => Some("forty"),
        'L' | 'M' => Some("forty_five"),
        _ => None,
    }
}
