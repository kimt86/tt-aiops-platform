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
use serde_json::{json, Value};
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::state;
use crate::toolbox::Toolbox;
use crate::util::{jstr, parse_myt};

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

/// How long AFTER berthing we keep archiving a call. Not zero, and this is the point:
/// a plan does not reliably exist before its ship arrives. Measured 2026-07-31 across every call
/// currently alongside — live plan total against the call's own declared counts, by hours since
/// berthing: 2.8h 100% · 4.8h 100% · 5.4h 100% · 9.3h 89.7% · 11.0h 67.4% · 13.8h 73.5% ·
/// 18.8h 25.9%. Nothing erodes inside ~5h because the extractor only drops a queue 6h after it
/// finishes, and discharge (which finishes first) is what goes missing first.
///
/// So "archive only before berthing" — the original rule — throws away every call whose plan is not
/// issued until its ship is at the quay, and those are permanent losses. Four hours keeps a full
/// margin under the measured cliff while the 5-minute tick gives ~48 chances to catch the call.
const POST_BERTH_ARCHIVE_H: i64 = 4;

/// Completeness bar for sealing early. Healthy calls land at 99.2-100.0% of their declared counts,
/// so 99 accepts the normal case without waiting out the window for a box or two of drift.
const COMPLETE_PCT: f64 = 99.0;

/// Band the coverage monitor calls healthy, as a percentage of the call's declared counts. Plans get
/// edited between our capture and the call's van counters settling, and a 1-3 box difference on a
/// 400-box call is that, not a lost plan — 246 of 254 archived calls sit inside this band.
///
/// ONE-SIDED, after trying the other way. An upper bound was added because the crane-double-count
/// bug (mig 0113) had inflated 26 of 80 calls to 316% of declared while the monitor said nothing —
/// a fair reaction, but the ratio cannot carry that meaning. An archive legitimately exceeds what a
/// ship declared whenever the ship leaves work undone, which is normal (41 departed calls left 805
/// containers unworked) and, worse, is indistinguishable from an in-progress call. Measured
/// 2026-08-04: planned-over-actual reaches 1,780% on CODV 003/2026 — a call that berthed the same
/// day and had barely started. Every one of the four calls the upper bound was firing on turned out
/// to be unexecuted bays, not inflation: WLHD 001/2026's excess is six load queues with zero moves
/// against a per-queue match of ±5 everywhere else.
///
/// The guard is not dropped, it is aimed properly — see AMBIGUOUS_QUEUE_WARN. Duplication is caught
/// where it happens (two cranes disagreeing about one queue) instead of being inferred from a total.
const COVERAGE_LO_PCT: f64 = 95.0;

/// Queues where two or more REAL cranes claim the same bay-job with DIFFERENT quantities. This is
/// the fold's blind spot made visible: it resolves them by preferring a real crane and then the
/// larger number, which is a guess, and a guess that grows is the shape the old double-count bug
/// actually had. Placeholder cranes (CR4, DC01..DC05) are excluded — a real crane and a placeholder
/// disagreeing is expected and the fold handles it by construction.
/// Measured 2026-08-04: 54 of 7,868 queues (0.7%), 500 of which carry more than one real crane at
/// all. The threshold sits above that so today's normal churn is silent and a step change is not.
const AMBIGUOUS_QUEUE_WARN: i64 = 150;

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
        let known = hdr.is_some();
        // Past the window the local copy has started losing finished queues, and we cannot tell that
        // apart from the plan shrinking. Inside it, the copy is whole.
        let past_window = s
            .actber_ts
            .is_some_and(|b| Utc::now() - b > chrono::Duration::hours(POST_BERTH_ARCHIVE_H));

        // First sight of a call only AFTER its safe window closed: what is left in the local table is
        // a partial plan, and storing that as the plan would quietly shorten every scenario built
        // from it. Record the gap instead of a wrong number.
        if past_window && !known {
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

        // Seal on completeness first, on the window only as a backstop. Sealing the moment a ship
        // berths — the original rule — stopped us exactly when the calls whose plan arrives late
        // were about to become archivable.
        let arch: Option<(Option<i64>, Option<i64>)> = sqlx::query_as(
            "SELECT sum(tq) FILTER (WHERE dl='D'), sum(tq) FILTER (WHERE dl='L')
               FROM (SELECT queuename, max(disload) dl, max(total_qty) tq
                       FROM (SELECT DISTINCT ON (qc, queuename) qc, queuename, disload, total_qty
                               FROM scenario.qc_plan WHERE vessel=$1 AND voyage=$2
                              ORDER BY qc, queuename, rev DESC) a
                      GROUP BY queuename) b",
        )
        .bind(vessel)
        .bind(voyage)
        .fetch_optional(&mut *tx)
        .await?;
        // Folded by queue name, MAX per queue — the same collapse the remaining() function does. A
        // straight sum over crane rows double-counts every reassigned bay and would declare calls
        // complete that are not.
        let pct = |got: Option<i64>, want: Option<i32>| -> bool {
            match (got, want) {
                (_, Some(0)) | (_, None) => true, // nothing declared -> nothing to be short of
                (Some(g), Some(w)) => 100.0 * g as f64 / w as f64 >= COMPLETE_PCT,
                (None, Some(_)) => false,
            }
        };
        let (ad, al) = arch.unwrap_or((None, None));
        let complete = pct(ad, s.disvan) && pct(al, s.loadvan);
        let (seal_ts, seal_reason) = if complete {
            (Some(Utc::now()), Some("complete"))
        } else if past_window {
            // Out of time and still short. Sealed anyway — leaving it open would keep appending from
            // an eroding source — but named so the coverage monitor can surface it.
            (Some(Utc::now()), Some("window_expired"))
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
/// Three conditions. Two compare against the call's own declared counts (disvan / loadvan) and only
/// in the SHORT direction; the third looks for duplication directly rather than through a total:
///   * a berthed call we never archived pre-berth — its plan has to come from the Oracle backfill;
///   * a sealed call whose archived totals fall below what it declared;
///   * queues where two real cranes disagree on the quantity, so the fold is guessing.
///
/// The alert refreshes while the condition holds and stops being refreshed when it clears, so it
/// drops off the banner by itself (mig 0107's contract — there is deliberately no ack).
async fn coverage_alert(pool: &PgPool) -> Result<()> {
    let bad: Option<(i64, i64)> = sqlx::query_as(
        // pct_* are numeric (round()), and a bare $1 next to one is inferred as numeric — which an
        // f64 bind cannot satisfy. Cast the column, not the parameter, so $1 lands as float8.
        "SELECT count(*) FILTER (WHERE sealed_reason IN ('missed_preberth', 'window_expired')),
                count(*) FILTER (WHERE sealed_reason NOT IN ('missed_preberth', 'window_expired')
                                   AND (pct_d::float8 < $1 OR pct_l::float8 < $1))
           FROM scenario.qc_plan_coverage
          WHERE coalesce(actber_ts, sealed_ts) > now() - interval '7 days'",
    )
    .bind(COVERAGE_LO_PCT)
    .fetch_optional(pool)
    .await?;
    // Duplication, looked for where it happens. Real cranes only: a real crane and a placeholder
    // disagreeing is the expected shape and the fold resolves it deterministically.
    let ambiguous: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (
           SELECT 1 FROM (
             SELECT DISTINCT ON (vessel, voyage, qc, queuename) vessel, voyage, qc, queuename, total_qty
               FROM scenario.qc_plan
              WHERE qc ~ '^(C|M|Z)[0-9]'
              ORDER BY vessel, voyage, qc, queuename, rev DESC) l
            GROUP BY vessel, voyage, queuename
           HAVING count(DISTINCT total_qty) > 1) z",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    let Some((missed, short)) = bad else { return Ok(()) };
    if missed == 0 && short == 0 && ambiguous < AMBIGUOUS_QUEUE_WARN {
        return Ok(());
    }
    let msg = format!(
        "안벽 계획 아카이브 점검 — 온전히 담지 못한 항차 {missed}건(늦게 발견했거나 창 안에 계획이 안 나옴), 신고량보다 적게 저장된 항차 {short}건, 크레인끼리 수량이 엇갈리는 큐 {ambiguous}개 (최근 7일)"
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

/// How many calls one backfill invocation pulls. They go in ONE Oracle round trip via a tuple
/// IN-list, so this is not a per-call cost — the SSH round trip (~0.9s) dominates and batching is
/// what makes the whole 174-call catch-up cheap. Kept modest so a single tick stays well inside the
/// toolbox timeout even for the largest calls (~60 plan rows each).
const BACKFILL_BATCH: usize = 12;

/// Recover the plan for calls the live path could not archive — either first seen after berthing
/// (already eroded) or from before this collector existed.
///
/// THIS IS POSSIBLE AT ALL because the earlier "the plan is destroyed every 90s" reading was wrong:
/// only OUR copy is. Oracle keeps JOB_QUEUE_SCHEDULE 6+ months (CRE_DT seen back to 2026-02-04,
/// DELT_FLG never used). Asking by (vessel, voyage) with the live query's two time predicates
/// REMOVED returned the complete plan for 12 of 12 sampled calls — totals matching each call's
/// declared disvan+loadvan exactly — including one that had departed 74.7h earlier.
///
/// And it is CHEAPER than what already runs: the live 90s query filters on UPD_DT, which has no
/// index, so it scans the table; (VESSEL, VOYAGE) is the leading edge of both the PK and
/// IDX_JOB_QUE_VESSEL, so this seeks.
///
/// What a backfilled row IS NOT: the plan as it stood at any past instant. It is the final edited
/// state. Live capture shows revisions (682 of 1,657 queues revised, up to 13 times); backfill
/// collapses them to one. Hence source='oracle_backfill' on the header — a reader comparing plans
/// across calls has to know which kind they hold.
pub async fn backfill(pool: &PgPool, target: &str) -> Result<()> {
    let cfg = state::load_config(pool).await?;
    if !cfg.enabled {
        tracing::info!("scenario collection disabled (kill switch) — skipping qc-plan backfill");
        return Ok(());
    }
    let run_id = state::start_run(pool, "qc_plan_backfill").await?;
    match backfill_tick(pool, run_id, target, &cfg).await {
        Ok(()) => state::finish_run(pool, run_id, "done", None).await?,
        Err(e) => {
            tracing::error!(error = %e, "scenario qc-plan backfill failed (isolated)");
            let _ = state::emit(pool, run_id, "error", "backfill_failed", json!({ "error": e.to_string() })).await;
            let _ = state::finish_run(pool, run_id, "error", Some(&e.to_string())).await;
        }
    }
    Ok(())
}

async fn backfill_tick(pool: &PgPool, run_id: i64, target: &str, cfg: &state::Config) -> Result<()> {
    state::set_phase(pool, run_id, "extract").await?;

    // Candidates, oldest berth first so the historical tail fills in a sensible order:
    //   * a call we recorded but could not archive (revs = 0 — the missed_preberth case), or
    //   * a berthed call we have no header for at all (everything from before this collector).
    // Calls WITH live revs are deliberately excluded: those were captured pre-berth, which is
    // strictly better evidence than the final edited state this path returns.
    let todo: Vec<(String, String, Option<i32>, Option<i32>)> = sqlx::query_as(
        "SELECT vc.vessel, vc.voyage, vc.disvan, vc.loadvan
           FROM scenario.vessel_call vc
           LEFT JOIN scenario.qc_plan_call c ON c.vessel = vc.vessel AND c.voyage = vc.voyage
          WHERE vc.actber IS NOT NULL
            AND ( c.vessel IS NULL
               OR (c.revs = 0 AND c.source = 'live')
               -- Revisit. A call backfilled while it was still alongside gets a SHORT plan: the plan
               -- keeps growing after berthing (measured ~1.4% per call), and the header was sealed at
               -- that moment, so without this clause the difference is lost for good. Once the ship
               -- has actually left, one more pass closes it.
               OR (c.source = 'oracle_backfill' AND vc.actdep IS NOT NULL
                   AND c.sealed_ts < vc.actdep) )
          -- Departed calls first: their plan is final, so one pass finishes them. Calls still
          -- alongside are worth deferring — they would only need the revisit above.
          ORDER BY (vc.actdep IS NULL), vc.actber
          LIMIT $1",
    )
    .bind(BACKFILL_BATCH as i64)
    .fetch_all(pool)
    .await?;
    if todo.is_empty() {
        state::merge_json(pool, run_id, "collection", json!({ "calls": 0, "note": "nothing to backfill" })).await?;
        tracing::info!("qc-plan backfill: nothing to do");
        return Ok(());
    }

    // Tuple IN-list -> one round trip, one INLIST ITERATOR over IDX_JOB_QUE_VESSEL. The two time
    // predicates of the live query are absent ON PURPOSE — they are exactly what erodes a plan once
    // work starts, and recovering that erosion is this function's whole job.
    // Dates go through TO_CHAR: the toolbox serialises Oracle DATE/NUMBER as JSON non-strings, and a
    // non-string into a String field fails parse_rows for the entire batch. Same lesson that made us
    // leave CRNT_PSN_IDX out of the extractor until it could be wrapped.
    let pairs = todo
        .iter()
        .map(|(v, y, _, _)| format!("('{}','{}')", v.replace('\'', "''"), y.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT s.JOB_QUE_VESSEL AS vessel, s.JOB_QUE_VOYAGE AS voyage,
                s.JOB_QUE_CRANENO AS qc, s.JOB_QUE_QUEUENAME AS queuename,
                s.JOB_QUE_DISLOAD AS disload, s.JOB_QUE_SEQ AS seq,
                s.JOB_QUE_TOTALQTY AS total_qty, s.JOB_QUE_COMPQTY AS comp_qty,
                s.JOB_QUE_PLANQTY AS plan_qty,
                TO_CHAR(s.CRE_DT,'YYYYMMDDHH24MISS') AS cre_dt,
                TO_CHAR(s.UPD_DT,'YYYYMMDDHH24MISS') AS upd_dt,
                TO_CHAR(s.ACT_DT,'YYYYMMDDHH24MISS') AS act_dt,
                s.CRE_USR_ID AS cre_usr
           FROM TOSADM.JOB_QUEUE_SCHEDULE s
          WHERE (s.JOB_QUE_VESSEL, s.JOB_QUE_VOYAGE) IN ({pairs})
            AND NVL(s.DELT_FLG,'N') <> 'Y'
            AND s.JOB_QUE_CRANENO IS NOT NULL
          ORDER BY s.JOB_QUE_VESSEL, s.JOB_QUE_VOYAGE, s.JOB_QUE_CRANENO, s.JOB_QUE_SEQ"
    );

    let t0 = std::time::Instant::now();
    let raw = Toolbox::from_env(target, cfg.oracle_timeout_s as u64)?.run_sql(&sql).await?;
    let query_ms = t0.elapsed().as_millis() as i64;
    let rows: Vec<Value> = parse_rows(&raw)?;
    let fetched = rows.len();

    state::set_phase(pool, run_id, "assemble").await?;
    let num = |r: &Value, k: &str| jstr(r, k).and_then(|s| s.parse::<i32>().ok());
    let ts = |r: &Value, k: &str| jstr(r, k).as_deref().and_then(parse_myt);

    let mut tx = pool.begin().await?;
    let mut per_call: HashMap<(String, String), i64> = HashMap::new();
    for r in &rows {
        let (Some(vessel), Some(voyage), Some(qc), Some(queuename)) =
            (jstr(r, "VESSEL"), jstr(r, "VOYAGE"), jstr(r, "QC"), jstr(r, "QUEUENAME"))
        else {
            continue;
        };
        // Append as the NEXT rev, not a hardcoded 1 — a revisited call already has rev 1 from the
        // pass that ran while it was alongside, and the whole point of the revisit is to record what
        // the plan grew into. Skipped entirely when nothing that defines a rev changed, so a revisit
        // that finds no growth writes zero rows (same rule as the live path).
        sqlx::query(
            "INSERT INTO scenario.qc_plan
               (vessel, voyage, qc, queuename, rev, disload, seq, total_qty, plan_qty,
                comp_qty_at_capture, act_dt, cre_dt, upd_dt, cre_usr)
             SELECT $1,$2,$3,$4,
                    COALESCE((SELECT max(rev) FROM scenario.qc_plan
                               WHERE vessel=$1 AND voyage=$2 AND qc=$3 AND queuename=$4), 0) + 1,
                    $5,$6,$7,$8,$9,$10,$11,$12,$13
              WHERE NOT EXISTS (
                    SELECT 1 FROM scenario.qc_plan p
                     WHERE p.vessel=$1 AND p.voyage=$2 AND p.qc=$3 AND p.queuename=$4
                       AND p.rev = (SELECT max(rev) FROM scenario.qc_plan
                                     WHERE vessel=$1 AND voyage=$2 AND qc=$3 AND queuename=$4)
                       AND p.seq       IS NOT DISTINCT FROM $6
                       AND p.total_qty IS NOT DISTINCT FROM $7
                       AND p.disload   IS NOT DISTINCT FROM $5)
             ON CONFLICT (vessel, voyage, qc, queuename, rev) DO NOTHING",
        )
        .bind(vessel.trim()).bind(voyage.trim()).bind(qc.trim()).bind(queuename.trim())
        .bind(jstr(r, "DISLOAD").as_deref().map(str::trim).filter(|s| !s.is_empty()))
        .bind(num(r, "SEQ")).bind(num(r, "TOTAL_QTY")).bind(num(r, "PLAN_QTY")).bind(num(r, "COMP_QTY"))
        .bind(ts(r, "ACT_DT")).bind(ts(r, "CRE_DT")).bind(ts(r, "UPD_DT"))
        .bind(jstr(r, "CRE_USR").as_deref().map(str::trim).filter(|s| !s.is_empty()))
        .execute(&mut *tx)
        .await?;
        *per_call.entry((vessel.trim().to_string(), voyage.trim().to_string())).or_insert(0) += 1;
    }

    // Header per candidate — including the ones Oracle returned nothing for. A call with a real
    // zero-row plan and a call we failed to fetch look identical downstream unless we say which is
    // which, so 'backfilled_empty' is recorded rather than leaving the candidate to be retried
    // forever.
    let (mut done, mut empty) = (0u64, 0u64);
    for (vessel, voyage, disvan, loadvan) in &todo {
        let n = per_call.get(&(vessel.clone(), voyage.clone())).copied().unwrap_or(0);
        let reason = if n > 0 { "backfilled" } else { "backfilled_empty" };
        if n > 0 { done += 1 } else { empty += 1 }
        sqlx::query(
            "INSERT INTO scenario.qc_plan_call
               (vessel, voyage, last_rev_ts, revs, rows_latest, qty_latest,
                actber_ts, disvan, loadvan, sealed_ts, sealed_reason, source)
             -- Counts come from the table, not from this pass's tally: a revisit adds to what an
             -- earlier pass wrote, and an accumulate-vs-overwrite choice here would be wrong in one
             -- of the two cases. qty_latest sums the LATEST rev per queue, so growth is reflected.
             SELECT $1, $2, now(),
                    (SELECT count(*) FROM scenario.qc_plan WHERE vessel=$1 AND voyage=$2),
                    (SELECT count(DISTINCT (qc, queuename)) FROM scenario.qc_plan
                      WHERE vessel=$1 AND voyage=$2),
                    -- Folded by queue name (mig 0113's fold): a bay reassigned between cranes has a
                    -- row under each, and summing them straight inflates qty_latest. The live path
                    -- sums one snapshot so it never saw this; backfill reads the whole call at once.
                    (SELECT sum(tq) FROM (
                        SELECT queuename, max(total_qty) tq FROM (
                            SELECT DISTINCT ON (qc, queuename) qc, queuename, total_qty
                              FROM scenario.qc_plan WHERE vessel=$1 AND voyage=$2
                             ORDER BY qc, queuename, rev DESC) z
                         GROUP BY queuename) y),
                    vc.actber, $3, $4, now(), $5, 'oracle_backfill'
               FROM scenario.vessel_call vc WHERE vc.vessel=$1 AND vc.voyage=$2
             ON CONFLICT (vessel, voyage) DO UPDATE SET
               last_rev_ts   = now(),
               revs          = EXCLUDED.revs,
               rows_latest   = EXCLUDED.rows_latest,
               qty_latest    = EXCLUDED.qty_latest,
               actber_ts     = COALESCE(EXCLUDED.actber_ts, scenario.qc_plan_call.actber_ts),
               disvan        = COALESCE(EXCLUDED.disvan, scenario.qc_plan_call.disvan),
               loadvan       = COALESCE(EXCLUDED.loadvan, scenario.qc_plan_call.loadvan),
               sealed_ts     = now(),
               sealed_reason = EXCLUDED.sealed_reason,
               source        = 'oracle_backfill'",
        )
        .bind(vessel).bind(voyage).bind(disvan).bind(loadvan).bind(reason)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    state::merge_json(pool, run_id, "load_stats", json!({
        "queries": 1, "rows_read": fetched, "query_ms": query_ms, "calls_in_batch": todo.len(),
    })).await?;
    state::merge_json(pool, run_id, "collection", json!({
        "calls": todo.len(), "backfilled": done, "empty": empty, "rows": fetched,
    })).await?;
    if empty > 0 {
        state::emit(pool, run_id, "warn", "backfill_empty", json!({
            "calls": empty, "note": "Oracle returned no assigned plan rows for these calls",
        })).await?;
    }
    coverage_alert(pool).await?;

    tracing::info!(calls = todo.len(), fetched, done, empty, query_ms, "scenario qc plan backfill");
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
