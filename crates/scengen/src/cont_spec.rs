//! Container size lookup -> scenario.container_spec, so volume can be reported in TEU.
//!
//! Asks TOSADM.CYY_CONTAINER for the ISO code of boxes we do not already know. Lookups go by
//! container number (the PK's leading column), so each is an index seek: 200 numbers came back in
//! 2.08s including the ~0.9s SSH round trip.
//!
//! ★WHAT WE ASK ABOUT, AND WHY IT IS THE YARD AND NOT A MOVE STREAM. CYY_CONTAINER *is* the current
//! yard inventory, so the question "is this box in the yard right now" and the question "will this
//! lookup answer" are the same question. Asking from `scenario.yard_cell` (our reconstruction of
//! that same inventory) therefore hits by construction — measured 200/200. Asking from a move
//! stream does not: a load move is the box LEAVING, and probing 140 containers seen in an `LD` move
//! within the previous 30 minutes answered for only 57 (40.7%), because the rest were already
//! aboard. A miss is not free either — with no miss table the box is re-asked every tick until it
//! ages out, so a low-hit source silently fills the batch with questions that can never answer.
//!
//! A box therefore gets its size while it SITS, not while it moves, which is also the earliest we
//! can ask and the longest window we have to ask in. Gate arrivals are covered because a gated-in
//! box is in the yard; ship arrivals were already covered by the BAPLIE manifest; and export boxes
//! are covered long before the crane comes for them. The recent-gate clause is kept as a second
//! source only so a box that gates straight back out between two ticks is not missed.
//!
//! It still does NOT backfill history. The yard is a present-tense fact: every box in it is one a
//! FUTURE window will contain. Windows predating this collector keep whatever the manifest seed
//! gave them, and the scenario publishes a coverage percentage so a partial total is never read as
//! the whole.
//!
//! Volume: the standing yard is ~101,000 slots and drains to zero unknowns once, after which only
//! genuinely new arrivals are asked about — a box is asked about exactly once in its life.

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
/// How far back to consider gate moves for the secondary source. This bound is also its retry
/// policy: a box that had already left when we asked stays unknown and gets a few more chances
/// while it is inside the window, then falls out on its own — no miss table, no permanent
/// re-asking. Wide enough that a short outage self-heals, narrow enough that it can never turn into
/// a historical sweep. The yard source needs no such bound: leaving the yard removes the row.
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

    // Two sources, deduped: the standing yard (hits by construction) plus boxes that crossed the
    // gate recently (catches one that arrives and leaves between two ticks, so is never in a yard
    // snapshot we take).
    //
    // ★The LIMIT must cut by RECENCY, which is why the ordering happens outside the dedup. The
    // previous form was `DISTINCT ON (contno) ... ORDER BY contno, ts DESC LIMIT`, and DISTINCT ON
    // forces contno to lead the sort — so the batch was actually the alphabetically first 1,000,
    // not the newest 1,000, contrary to its own comment. Harmless while there were ~30 candidates;
    // a landmine the moment there are more than a batch of them, because every tick would re-ask
    // the same head of the alphabet and never reach the rest.
    let todo: Vec<(String,)> = sqlx::query_as(
        "SELECT contno FROM (
           SELECT y.contno, coalesce(y.updated_ts, now()) AS ts
             FROM scenario.yard_cell y
            WHERE y.contno IS NOT NULL
           UNION ALL
           SELECT r.contno, r.comp_ts
             FROM rtg_move_log r
            WHERE r.comp_ts > now() - make_interval(hours => $1)
              AND r.jobtype IN ('GI','GO')
         ) c
          WHERE NOT EXISTS (SELECT 1 FROM scenario.container_spec s WHERE s.contno = c.contno)
          GROUP BY contno
          ORDER BY max(ts) DESC
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
