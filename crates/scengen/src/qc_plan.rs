//! Quay-crane work-plan archiver -> scenario.qc_plan (+ .qc_plan_call). ZERO Oracle: it reads only
//! public.live_workqueue and public.live_vessel_schedule, which the critical extractor already
//! refreshes every 90s. See mig 0110 for the seven measurements that shaped this; the three that
//! shape the CODE are:
//!
//!   * the plan keeps being edited before berthing (1471 -> 1465 in 4m50s on a call with no work
//!     done yet, right after matching its own declared total), so we append changed ROWS as revs
//!     rather than sealing one snapshot;
//!   * comp_qty is progress, not plan — it must never open a rev, or every tick writes one;
//!   * a row leaving live_workqueue is ambiguous (dropped from the plan vs completed-then-excluded
//!     by the extractor's 6h rule), so absence is never recorded, and a call stops being revised
//!     the moment it berths — which is exactly when that erosion begins.
//!
//! A call first seen AFTER it berthed is deliberately NOT archived from the local table: erosion has
//! already eaten part of it (measured: 32% of plan remaining at 16.5h post-berth). It is marked
//! `missed_preberth` for the Oracle backfill, because storing a short plan as if it were the plan is
//! the one failure here that would be silent and would poison every scenario built from it.

use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgPool;

use crate::state;

/// One row of the current plan, joined to its call's schedule. The join to live_vessel_schedule is
/// also the sentinel filter: live_workqueue carries vessel='RHXX' rows whose `qc` is not a crane
/// ('GRP A', 'POOLCS', …) and which are 15% of all qty. They have no schedule row, so they drop here.
#[derive(Debug)]
struct PlanRow {
    vessel: String,
    voyage: String,
    qc: String,
    queuename: String,
    disload: Option<String>,
    seq: Option<i32>,
    total_qty: Option<i32>,
    plan_qty: Option<i32>,
    comp_qty: Option<i32>,
    estber_ts: Option<DateTime<Utc>>,
    actber_ts: Option<DateTime<Utc>>,
    disvan: Option<i32>,
    loadvan: Option<i32>,
}

/// What defines a NEW revision. Deliberately excludes comp_qty (progress) and plan_qty (which is not
/// the remainder and wobbles between three different meanings — see mig 0110 note 6).
type RevKey = (String, Option<i32>, Option<i32>, Option<String>);

fn rev_key(r: &PlanRow) -> RevKey {
    (r.qc.clone(), r.seq, r.total_qty, r.disload.clone())
}

pub async fn run(pool: &PgPool) -> Result<()> {
    let cfg = state::load_config(pool).await?;
    if !cfg.enabled {
        tracing::info!("scenario collection disabled (kill switch) — skipping qc-plan");
        return Ok(());
    }
    let run_id = state::start_run(pool, "qc_plan").await?;
    match tick(pool, run_id).await {
        Ok(()) => {
            // Retention runs inside the same tick so this table cannot become the fifth scenario
            // table with no prune at all. It also makes scenario.config.retention_days actually
            // read by something for the first time — nothing consumed it before.
            match prune(pool, cfg.retention_days).await {
                Ok(n) if n > 0 => tracing::debug!(collapsed = n, "qc_plan superseded revs collapsed"),
                Ok(_) => {}
                // A failed prune must be loud, not swallowed: a silent no-op prune is the same
                // blindness as a silent failure, and this table only grows.
                Err(e) => tracing::warn!(error = %e, "QC PLAN PRUNE FAILED"),
            }
            state::finish_run(pool, run_id, "done", None).await?
        }
        Err(e) => {
            tracing::error!(error = %e, "scenario qc-plan failed (isolated — others unaffected)");
            let _ = state::emit(pool, run_id, "error", "qc_plan_failed", json!({ "error": e.to_string() })).await;
            let _ = state::finish_run(pool, run_id, "error", Some(&e.to_string())).await;
        }
    }
    Ok(()) // always Ok: non-critical subsystem must not cascade
}

async fn tick(pool: &PgPool, run_id: i64) -> Result<()> {
    state::set_phase(pool, run_id, "extract").await?;

    // ONE statement, so MVCC hands us a single consistent picture. That matters: the extractor
    // refreshes live_workqueue by DELETE + re-INSERT inside one transaction, so a reader sees either
    // the old set or the new one — never half — but only if it reads in one go.
    let rows: Vec<PlanRow> = sqlx::query_as::<_, (
        String, String, String, String, Option<String>, Option<i32>, Option<i32>, Option<i32>,
        Option<i32>, Option<DateTime<Utc>>, Option<DateTime<Utc>>, Option<i32>, Option<i32>,
    )>(
        "SELECT w.vessel, w.voyage, w.qc, w.queuename, w.disload, w.seq,
                w.total_qty, w.plan_qty, w.comp_qty,
                s.estber_ts, s.actber_ts, s.disvan, s.loadvan
           FROM public.live_workqueue w
           JOIN public.live_vessel_schedule s
             ON s.vessel = w.vessel AND s.voyage = w.voyage",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|t| PlanRow {
        vessel: t.0, voyage: t.1, qc: t.2, queuename: t.3, disload: t.4, seq: t.5,
        total_qty: t.6, plan_qty: t.7, comp_qty: t.8,
        estber_ts: t.9, actber_ts: t.10, disvan: t.11, loadvan: t.12,
    })
    .collect();
    let fetched = rows.len();

    // Group by call. BTreeMap-free: order does not matter, each call is independent.
    let mut calls: HashMap<(String, String), Vec<PlanRow>> = HashMap::new();
    for r in rows {
        calls.entry((r.vessel.clone(), r.voyage.clone())).or_default().push(r);
    }

    // The extractor's live_workqueue PK is (qc, vessel, queuename) — it drops voyage, which the TOS
    // PK has. Two voyages of one vessel inside the same window can therefore overwrite each other
    // upstream. We cannot fix that from here, but we can refuse to be silent about the condition.
    let mut multi_voyage: Vec<String> = Vec::new();
    let mut by_vessel: HashMap<&str, usize> = HashMap::new();
    for (v, _) in calls.keys() {
        *by_vessel.entry(v.as_str()).or_insert(0) += 1;
    }
    for (v, n) in by_vessel {
        if n > 1 {
            multi_voyage.push(v.to_string());
        }
    }

    state::set_phase(pool, run_id, "assemble").await?;
    let mut tx = pool.begin().await?;
    let (mut new_calls, mut revised, mut sealed, mut missed, mut skipped_sealed) = (0u64, 0u64, 0u64, 0u64, 0u64);

    for ((vessel, voyage), plan) in &calls {
        // Existing header decides what we are allowed to do with this call.
        let hdr: Option<(Option<DateTime<Utc>>, i32)> = sqlx::query_as(
            "SELECT sealed_ts, revs FROM scenario.qc_plan_call WHERE vessel=$1 AND voyage=$2",
        )
        .bind(vessel)
        .bind(voyage)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some((Some(_), _)) = hdr {
            skipped_sealed += 1;
            continue; // already sealed — never revise a sealed call
        }

        let s = &plan[0]; // schedule fields are per-call, identical across its rows
        let berthed = s.actber_ts.is_some();
        let known = hdr.is_some();

        // First sight of a call that has ALREADY berthed: the local plan is partially eroded, so
        // archiving it would record a short plan as the truth. Record the gap instead.
        if berthed && !known {
            sqlx::query(
                "INSERT INTO scenario.qc_plan_call
                   (vessel, voyage, estber_ts, actber_ts, disvan, loadvan, rows_latest, qty_latest,
                    sealed_ts, sealed_reason, source)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8, now(), 'missed_preberth', 'live')
                 ON CONFLICT (vessel, voyage) DO NOTHING",
            )
            .bind(vessel).bind(voyage).bind(s.estber_ts).bind(s.actber_ts)
            .bind(s.disvan).bind(s.loadvan)
            .bind(plan.len() as i32)
            .bind(plan.iter().filter_map(|r| r.total_qty).sum::<i32>())
            .execute(&mut *tx)
            .await?;
            missed += 1;
            continue;
        }

        // Latest archived rev per (qc, queuename), to diff against.
        let prev: Vec<(String, String, i32, Option<i32>, Option<i32>, Option<String>)> = sqlx::query_as(
            "SELECT DISTINCT ON (qc, queuename) qc, queuename, rev, seq, total_qty, disload
               FROM scenario.qc_plan WHERE vessel=$1 AND voyage=$2
              ORDER BY qc, queuename, rev DESC",
        )
        .bind(vessel)
        .bind(voyage)
        .fetch_all(&mut *tx)
        .await?;
        let mut latest: HashMap<(String, String), (i32, RevKey)> = HashMap::new();
        for (qc, qn, rev, seq, tq, dl) in prev {
            latest.insert((qc.clone(), qn), (rev, (qc, seq, tq, dl)));
        }

        let mut call_revs = 0u64;
        for r in plan {
            let k = (r.qc.clone(), r.queuename.clone());
            let next_rev = match latest.get(&k) {
                // Unchanged on the fields that define the plan — nothing to record. This is the
                // common case every tick, and why the archive stays at hundreds of rows a day.
                Some((_, prev_key)) if *prev_key == rev_key(r) => continue,
                Some((rev, _)) => rev + 1,
                None => 1,
            };
            sqlx::query(
                "INSERT INTO scenario.qc_plan
                   (vessel, voyage, qc, queuename, rev, disload, seq, total_qty, plan_qty,
                    comp_qty_at_capture)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
                 ON CONFLICT (vessel, voyage, qc, queuename, rev) DO NOTHING",
            )
            .bind(vessel).bind(voyage).bind(&r.qc).bind(&r.queuename).bind(next_rev)
            .bind(&r.disload).bind(r.seq).bind(r.total_qty).bind(r.plan_qty).bind(r.comp_qty)
            .execute(&mut *tx)
            .await?;
            call_revs += 1;
        }

        // Seal on berthing: from here the extractor's 6h exclusion starts removing completed rows,
        // and we cannot tell that apart from the plan actually shrinking.
        let (seal_ts, seal_reason) = if berthed {
            (Some(Utc::now()), Some("berthed"))
        } else {
            (None, None)
        };
        sqlx::query(
            "INSERT INTO scenario.qc_plan_call
               (vessel, voyage, last_rev_ts, revs, rows_latest, qty_latest,
                estber_ts, actber_ts, disvan, loadvan, sealed_ts, sealed_reason, source)
             VALUES ($1,$2, CASE WHEN $3::int > 0 THEN now() END, $3::int, $4, $5,
                     $6,$7,$8,$9,$10,$11,'live')
             ON CONFLICT (vessel, voyage) DO UPDATE SET
               last_rev_ts   = COALESCE(EXCLUDED.last_rev_ts, scenario.qc_plan_call.last_rev_ts),
               revs          = scenario.qc_plan_call.revs + EXCLUDED.revs,
               rows_latest   = EXCLUDED.rows_latest,
               qty_latest    = EXCLUDED.qty_latest,
               estber_ts     = EXCLUDED.estber_ts,
               actber_ts     = EXCLUDED.actber_ts,
               disvan        = COALESCE(EXCLUDED.disvan, scenario.qc_plan_call.disvan),
               loadvan       = COALESCE(EXCLUDED.loadvan, scenario.qc_plan_call.loadvan),
               sealed_ts     = EXCLUDED.sealed_ts,
               sealed_reason = EXCLUDED.sealed_reason",
        )
        .bind(vessel).bind(voyage)
        .bind(call_revs as i32)
        .bind(plan.len() as i32)
        .bind(plan.iter().filter_map(|r| r.total_qty).sum::<i32>())
        .bind(s.estber_ts).bind(s.actber_ts).bind(s.disvan).bind(s.loadvan)
        .bind(seal_ts).bind(seal_reason)
        .execute(&mut *tx)
        .await?;

        if !known {
            new_calls += 1;
        }
        if seal_ts.is_some() {
            sealed += 1;
        }
        revised += call_revs;
    }
    tx.commit().await?;

    state::merge_json(pool, run_id, "collection", json!({
        "fetched": fetched, "calls": calls.len(), "new_calls": new_calls,
        "rev_rows": revised, "sealed": sealed, "missed_preberth": missed,
        "skipped_sealed": skipped_sealed,
    })).await?;
    // Zero Oracle — recorded explicitly so the load dashboard does not have to infer it.
    state::merge_json(pool, run_id, "load_stats", json!({ "queries": 0, "oracle": false })).await?;

    if missed > 0 {
        state::emit(pool, run_id, "warn", "plan_missed_preberth", json!({
            "calls": missed,
            "note": "call first seen after berthing — local plan already eroded; needs oracle backfill",
        })).await?;
    }
    if !multi_voyage.is_empty() {
        state::emit(pool, run_id, "warn", "vessel_multi_voyage", json!({
            "vessels": multi_voyage,
            "note": "live_workqueue PK omits voyage — two voyages of one vessel can overwrite upstream",
        })).await?;
    }
    coverage_alert(pool).await?;

    tracing::info!(fetched, calls = calls.len(), new_calls, revised, sealed, missed, "scenario qc plan");
    Ok(())
}

/// Reach a human when the archive is short. Until this existed, nothing anywhere compared what we
/// captured against what the call declared, so a lost plan was completely silent — and a scenario
/// built on a short plan is wrong in a way that looks perfectly normal.
///
/// Two conditions, both measured against the call's own declared counts (disvan / loadvan):
///   * a berthed call we never archived pre-berth — its plan has to come from the Oracle backfill;
///   * a sealed call whose archived totals fall below what it declared.
/// The alert refreshes while the condition holds and stops being refreshed when it clears, so it
/// drops off the banner by itself (mig 0107's contract — there is deliberately no ack).
async fn coverage_alert(pool: &PgPool) -> Result<()> {
    // 95%, not 100%: plans get edited between our capture and the call's own van counters settling,
    // and a 1-3 box difference on a 400-box call is that, not a lost plan. Measured spread on
    // healthy calls was 99.2-100.0%.
    let bad: Option<(i64, i64)> = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE sealed_reason = 'missed_preberth'),
                count(*) FILTER (WHERE sealed_reason <> 'missed_preberth'
                                   AND (pct_d < 95 OR pct_l < 95))
           FROM scenario.qc_plan_coverage
          WHERE coalesce(actber_ts, sealed_ts) > now() - interval '7 days'",
    )
    .fetch_optional(pool)
    .await?;
    let Some((missed, short)) = bad else { return Ok(()) };
    if missed == 0 && short == 0 {
        return Ok(());
    }
    let msg = format!(
        "안벽 계획 아카이브 부족 — 접안 후에야 발견한 항차 {missed}건, 신고량보다 적게 저장된 항차 {short}건 (최근 7일). 소급 백필 필요"
    );
    // warn, not crit: scenarios for those calls are unavailable, which is a gap, not an outage.
    let _ = sqlx::query(
        "INSERT INTO ops_alert (source, subject, severity, message)
              VALUES ('scenario', 'qc_plan_coverage', 'warn', $1)
         ON CONFLICT (source, subject) DO UPDATE
            SET last_ts = now(), occurrences = ops_alert.occurrences + 1, message = EXCLUDED.message",
    )
    .bind(&msg)
    .execute(pool)
    .await;
    Ok(())
}

/// Collapse superseded revisions older than `keep_days`, keeping the newest rev per
/// (call, crane, bay-job) forever — that final one IS the scenario's work list, so it must not be
/// pruned. Only the churn from mid-flight plan edits ages out. Called from the same tick so this
/// table arrives with its own retention instead of joining the four scenario tables that have none.
pub async fn prune(pool: &PgPool, keep_days: i32) -> Result<u64> {
    let n = sqlx::query(
        "DELETE FROM scenario.qc_plan p
          WHERE p.captured_at < now() - make_interval(days => $1)
            AND EXISTS (SELECT 1 FROM scenario.qc_plan q
                         WHERE q.vessel=p.vessel AND q.voyage=p.voyage
                           AND q.qc=p.qc AND q.queuename=p.queuename
                           AND q.rev > p.rev)",
    )
    .bind(keep_days)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n)
}
