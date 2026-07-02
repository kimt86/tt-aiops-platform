//! Live work pool: per-QC work-queue sequence + the active (in-flight) container moves
//! that need / have a TT. Reads ONLY the Postgres snapshot tables (`live_workqueue`,
//! `live_workpool`) that the extractor refreshes ~every 90s from TOS — the API crate
//! never touches Oracle. The frontend fuses this with the live websocket PLC/GPS.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use axum::{extract::State, Json};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;

use crate::routes::AppError;

#[derive(sqlx::FromRow)]
struct QueueRow {
    qc: String,
    vessel: String,
    voyage: Option<String>,
    queuename: String,
    disload: Option<String>,
    seq: Option<i32>,
    total_qty: Option<i32>,
    comp_qty: Option<i32>,
}

#[derive(sqlx::FromRow)]
struct MoveRow {
    qc: Option<String>,
    queuename: String,
    vessel: String,
    jobtype: Option<String>,
    yt_status: Option<String>,
    ytno: Option<String>,
    armgc: Option<String>,
    etw_ts: Option<DateTime<Utc>>,
    etw_accurate: Option<DateTime<Utc>>,
    etw_expires: Option<DateTime<Utc>>,
    actv_ts: Option<DateTime<Utc>>,
    contno: Option<String>,
    yt_topos: Option<String>,
    from_pos: Option<String>,
    to_pos: Option<String>,
    twintandem: Option<String>,
    upd_ts: Option<DateTime<Utc>>,
}

#[derive(Serialize, Clone)]
struct MoveOut {
    qc: Option<String>,
    queuename: String,
    vessel: String,
    jobtype: Option<String>,
    yt_status: Option<String>,
    ytno: Option<String>,
    armgc: Option<String>,
    etw_ts: Option<DateTime<Utc>>,
    /// accurate ETW from the TOS ETW RPC gateway (qc_etw_utc, else vessel_etw_utc)
    etw_accurate: Option<DateTime<Utc>>,
    /// when that accurate ETW snapshot expires (stale after this)
    etw_expires: Option<DateTime<Utc>>,
    /// RTG/order activation (JOB_ODR_ACTV_DT) — DS soon-idle handover-start signal.
    /// NOTE: activation, not the ±1s physical lift (can lead by minutes).
    actv_ts: Option<DateTime<Utc>>,
    contno: Option<String>,
    yt_topos: Option<String>,
    from_pos: Option<String>,
    to_pos: Option<String>,
    twintandem: Option<String>,
    /// TOS row last-update ≈ truck-assignment time (D_tos); internal (validation logger), not in JSON
    #[serde(skip)]
    upd_ts: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
struct CandidateRow {
    qc: Option<String>,
    queuename: String,
    vessel: String,
    jobtype: Option<String>,
    src_block: Option<String>,
    rtg: Option<String>,
    n: i32,
}

#[derive(Serialize)]
struct CandidateOut {
    qc: Option<String>,
    queuename: String,
    vessel: String,
    jobtype: Option<String>,
    /// load: source yard block (pickup); discharge: null (pickup = the QC)
    src_block: Option<String>,
    rtg: Option<String>,
    n: i32,
    /// derived urgency: moves the QC must still do before reaching this work
    /// (0 = the QC is working this queue right now)
    moves_until: i64,
    active: bool,
}

#[derive(Serialize)]
struct QueueOut {
    queuename: String,
    vessel: String,
    voyage: Option<String>,
    disload: Option<String>,
    seq: Option<i32>,
    total: i32,
    done: i32,
    remaining: i32,
    /// SHADOW: when this bay/queue must complete so the vessel departs on time (deadline
    /// distribution = ESTDEP minus the work still after it). NULL if the vessel has no ESTDEP.
    deadline_ts: Option<DateTime<Utc>>,
    /// SHADOW: when the QC will START this bay (now + work scheduled before it). With proc_s the
    /// frontend staggers per-container consistently (avoids reconstructing from deadline_ts with a
    /// mismatched move time). NULL if the vessel has no ESTDEP.
    work_eta_ts: Option<DateTime<Utc>>,
    /// SHADOW: this bay's total processing seconds (moves + transition overhead).
    proc_s: Option<i64>,
}

#[derive(Serialize)]
struct QcOut {
    qc: String,
    vessels: Vec<String>,
    active_moves: usize,
    remaining: i64,
    queues: Vec<QueueOut>,
    moves: Vec<MoveOut>,
    /// SHADOW deadline fields (primary/active vessel): departure time, remaining work seconds,
    /// and slack = ESTDEP − now − work_left (negative = behind schedule for departure).
    estdep_ts: Option<DateTime<Utc>>,
    work_left_s: Option<i64>,
    slack_s: Option<i64>,
}

#[derive(Serialize)]
pub struct WorkpoolOut {
    as_of: Option<DateTime<Utc>>,
    qc_count: usize,
    active_moves: usize,
    total_remaining: i64,
    qcs: Vec<QcOut>,
    /// global active-move front, soonest ETW first (the urgent work), capped
    pool: Vec<MoveOut>,
    /// candidate job pool — UNASSIGNED demand needing a truck, urgency-ranked.
    /// discharge grouped by QC, load grouped by source block (pickup location).
    candidates: Vec<CandidateOut>,
    candidate_total: i64,
}

const POOL_CAP: usize = 80;

/// `GET /api/workpool` — the live per-QC work pool (Postgres snapshot, ~90s fresh).
pub async fn workpool(State(pool): State<PgPool>) -> Result<Json<WorkpoolOut>, AppError> {
    Ok(Json(build_workpool(pool).await?))
}

/// The full per-QC work pool + shadow deadline computation, shared by the HTTP handler and the
/// dispatch-prediction logger (so the logged predictions exactly match what the page computes).
pub(crate) async fn build_workpool(pool: PgPool) -> Result<WorkpoolOut, AppError> {
    let queues: Vec<QueueRow> = sqlx::query_as(
        "SELECT qc, vessel, voyage, queuename, disload, seq, total_qty, comp_qty
           FROM live_workqueue",
    )
    .fetch_all(&pool)
    .await?;

    let moves: Vec<MoveRow> = sqlx::query_as(
        "SELECT w.qc, w.queuename, w.vessel, w.jobtype, w.yt_status, w.ytno, w.armgc, w.etw_ts,
                coalesce(e.qc_etw_utc, e.vessel_etw_utc) AS etw_accurate,
                e.expires_at_utc AS etw_expires, w.actv_ts,
                w.contno, w.yt_topos, w.from_pos, w.to_pos, w.twintandem, w.upd_ts
           FROM live_workpool w
           LEFT JOIN tos_etw_cntr e
                  ON e.vessel = w.vessel AND e.voyage = w.voyage AND e.cntr_no = w.contno",
    )
    .fetch_all(&pool)
    .await?;

    let as_of: Option<(Option<DateTime<Utc>>,)> =
        sqlx::query_as("SELECT max(as_of_ts) FROM live_workpool")
            .fetch_optional(&pool)
            .await?;
    let as_of = as_of.and_then(|r| r.0);

    let to_move = |m: &MoveRow| MoveOut {
        qc: m.qc.clone(),
        queuename: m.queuename.clone(),
        vessel: m.vessel.clone(),
        jobtype: m.jobtype.clone(),
        yt_status: m.yt_status.clone(),
        ytno: m.ytno.clone(),
        armgc: m.armgc.clone(),
        etw_ts: m.etw_ts,
        etw_accurate: m.etw_accurate,
        etw_expires: m.etw_expires,
        actv_ts: m.actv_ts,
        contno: m.contno.clone(),
        yt_topos: m.yt_topos.clone(),
        from_pos: m.from_pos.clone(),
        to_pos: m.to_pos.clone(),
        twintandem: m.twintandem.clone(),
        upd_ts: m.upd_ts,
    };

    // which QCs are "working now": have an active move, or a started queue (comp>0).
    let mut active_qcs: BTreeMap<String, ()> = BTreeMap::new();
    for m in &moves {
        if let Some(qc) = m.qc.as_deref().filter(|s| !s.is_empty()) {
            active_qcs.insert(qc.to_string(), ());
        }
    }
    for q in &queues {
        if q.comp_qty.unwrap_or(0) > 0 && !q.qc.is_empty() {
            active_qcs.insert(q.qc.clone(), ());
        }
    }

    // group queues + moves by QC
    let mut q_by_qc: BTreeMap<String, Vec<&QueueRow>> = BTreeMap::new();
    for q in &queues {
        if active_qcs.contains_key(&q.qc) {
            q_by_qc.entry(q.qc.clone()).or_default().push(q);
        }
    }
    let mut m_by_qc: BTreeMap<String, Vec<&MoveRow>> = BTreeMap::new();
    for m in &moves {
        if let Some(qc) = m.qc.as_deref().filter(|s| !s.is_empty()) {
            m_by_qc.entry(qc.to_string()).or_default().push(m);
        }
    }

    let mut qcs: Vec<QcOut> = Vec::new();
    for qc in active_qcs.keys() {
        let mut qrows = q_by_qc.remove(qc).unwrap_or_default();
        qrows.sort_by_key(|q| q.seq.unwrap_or(i32::MAX));
        let mut mrows = m_by_qc.remove(qc).unwrap_or_default();
        // soonest ETW first — prefer the accurate gateway ETW, fall back to the DB ETW.
        let etw = |m: &MoveRow| m.etw_accurate.or(m.etw_ts);
        mrows.sort_by(|a, b| match (etw(a), etw(b)) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });

        let remaining: i64 = qrows
            .iter()
            .map(|q| (q.total_qty.unwrap_or(0) - q.comp_qty.unwrap_or(0)).max(0) as i64)
            .sum();
        let mut vessels: Vec<String> = Vec::new();
        for m in &mrows {
            if !vessels.contains(&m.vessel) {
                vessels.push(m.vessel.clone());
            }
        }
        // fall back to queue vessels if no active moves
        if vessels.is_empty() {
            for q in &qrows {
                if q.comp_qty.unwrap_or(0) > 0 && !vessels.contains(&q.vessel) {
                    vessels.push(q.vessel.clone());
                }
            }
        }

        let queues_out: Vec<QueueOut> = qrows
            .iter()
            .map(|q| {
                let total = q.total_qty.unwrap_or(0);
                let done = q.comp_qty.unwrap_or(0);
                QueueOut {
                    queuename: q.queuename.clone(),
                    vessel: q.vessel.clone(),
                    voyage: q.voyage.clone(),
                    disload: q.disload.clone(),
                    seq: q.seq,
                    total,
                    done,
                    remaining: (total - done).max(0),
                    deadline_ts: None,
                    work_eta_ts: None,
                    proc_s: None,
                }
            })
            .collect();
        let moves_out: Vec<MoveOut> = mrows.iter().map(|m| to_move(m)).collect();

        qcs.push(QcOut {
            qc: qc.clone(),
            vessels,
            active_moves: moves_out.iter().filter(|m| m.ytno.as_deref().is_some_and(|s| !s.is_empty())).count(),
            remaining,
            queues: queues_out,
            moves: moves_out,
            estdep_ts: None,
            work_left_s: None,
            slack_s: None,
        });
    }
    // busiest QCs first (most active moves, then most remaining)
    qcs.sort_by(|a, b| b.active_moves.cmp(&a.active_moves).then(b.remaining.cmp(&a.remaining)));

    // ── SHADOW: deadline distribution ──────────────────────────────────────────────────────
    // Work backward from each vessel's departure (ESTDEP): the last bay must finish by ESTDEP,
    // earlier bays sooner. Per-bay work = remaining containers × move time (discharge/load) +
    // transition overhead (gantry between bays; hatch-cover at deck↔hold). Slack = ESTDEP − now
    // − total remaining work (negative = behind). Display-only (not wired to dispatch).
    // moves = containers × (1 − twin_frac/2) per vessel (a twin lift = 2 containers in 1 move);
    // work must finish a buffer before departure; move time per jobtype constant (per-crane below).
    {
        // ESTDEP (departure) + ESTWKC (all-crane-work-complete) per vessel. ESTWKC is the terminal's
        // planned finish — often EARLIER than departure — so the real work deadline = the tighter of
        // the two. Verified vs TOS source (VSB_VOYAGE): both fields are authoritative.
        let sched: Vec<(String, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> =
            sqlx::query_as("SELECT vessel, estdep_ts, estwkc_ts FROM live_vessel_schedule WHERE estdep_ts IS NOT NULL")
                .fetch_all(&pool).await.unwrap_or_default();
        let estdep: std::collections::HashMap<String, DateTime<Utc>> =
            sched.iter().filter_map(|(v, d, _)| d.map(|d| (v.clone(), d))).collect();
        let estwkc: std::collections::HashMap<String, DateTime<Utc>> =
            sched.iter().filter_map(|(v, _, w)| w.map(|w| (v.clone(), w))).collect();
        // per-vessel twin fraction of remaining containers (from the dispatchable pool) — a proxy
        // for the whole remaining queue. moves ≈ containers × (1 − frac/2).
        let twin_frac: std::collections::HashMap<String, f64> =
            sqlx::query_as::<_, (String, Option<f64>)>(
                "SELECT vessel, avg(CASE WHEN twintandem='W' THEN 1.0 ELSE 0.0 END)::float8
                   FROM live_workpool GROUP BY vessel")
                .fetch_all(&pool).await.unwrap_or_default()
                .into_iter().filter_map(|(v, f)| f.map(|f| (v, f))).collect();
        // per-crane per-jobtype median move time (rolling 3-day, from learn_qc_move_time); key
        // ('D'=discharge,'L'=load). SHIFT-aware: prefer the current terminal shift (D=06–17 / N=18–05
        // MYT, ~±30% day/night), fall back to 'ALL' when a shift is sparse, then to the jobtype constant.
        let cur_shift = if (6..18).contains(&(((Utc::now().timestamp() / 3600) + 8) % 24)) { "D" } else { "N" };
        let mt_rows: Vec<(String, String, String, Option<i32>)> =
            sqlx::query_as("SELECT qc, jobtype, shift, med_sec FROM learn_qc_move_time WHERE med_sec IS NOT NULL")
                .fetch_all(&pool).await.unwrap_or_default();
        let mut move_time: std::collections::HashMap<(String, char), f64> = std::collections::HashMap::new();
        for pass in ["ALL", cur_shift] { // ALL first (base), then overwrite with the current shift where present
            for (qc, jt, sh, ms) in &mt_rows {
                if sh == pass {
                    if let Some(ms) = ms {
                        move_time.insert((qc.clone(), if jt == "LD" { 'L' } else { 'D' }), *ms as f64);
                    }
                }
            }
        }
        // learned work-ETA residual: median(actual − predicted) per (crane, jobtype) from the shadow
        // validation, DISPATCH-BAND horizon only (5–45 min; far predictions are re-plan-polluted).
        // mig 0083, refreshed ~20 min by the pred logger. Self-recalibrating: predictions embed this
        // term, so the residual converges toward 0 (integral controller, damped by the 7-day window).
        // Measured need: LD near-horizon ran +21 min optimistic with NO static correction, and the DS
        // static +600s was tuned on top of the ghost-transition bug fixed alongside this.
        let bias_rows: Vec<(String, String, i32, Option<i32>)> =
            sqlx::query_as("SELECT qc, jobtype, n, med_err_s FROM learn_work_eta_bias")
                .fetch_all(&pool).await.unwrap_or_default();
        let mut eta_bias: std::collections::HashMap<(String, char), i64> = std::collections::HashMap::new();
        for (bqc, jt, n, med) in bias_rows {
            let min_n = if bqc.is_empty() { 100 } else { 30 }; // '' = global-jobtype fallback row
            if n < min_n {
                continue;
            }
            if let Some(med) = med {
                let j = if jt == "LD" { 'L' } else { 'D' };
                eta_bias.insert((bqc, j), (med as i64).clamp(-900, 1800));
            }
        }
        let now = Utc::now();
        // work-ETA is a FIXED future instant (when the QC reaches a bay); anchor it to the data
        // snapshot (as_of), NOT now — else every poll re-anchors to a later "now" and the countdown
        // jumps back up ~poll-interval each refresh. (slack/deadline below stay now/ESTDEP-anchored.)
        let eta_anchor = as_of.unwrap_or(now);
        const DS_MOVE_S: f64 = 90.0;
        const LD_MOVE_S: f64 = 110.0;
        const BAY_CHANGE_S: f64 = 180.0;   // gantry travel between bays (extra)
        const HATCH_DS_S: f64 = 340.0;     // discharge deck→hold cover removal (extra)
        const HATCH_LD_S: f64 = 390.0;     // load hold→deck cover placement (extra)
        const FINISH_BUFFER_S: i64 = 1800; // work should finish ~30 min before departure
        // Empirical work-ETA calibration: the active crane's CURRENT in-progress operation is not
        // modeled at the bay anchor, so DS work-ETAs ran ~+10 min optimistic vs actual in the near
        // (dispatch-relevant) range — confirmed by shadow validation (resolved_at − pred). Shift DS
        // work-ETA by this. The departure-based deadline_ts and slack_s use `total` and are
        // UNAFFECTED. Far-out predictions remain limited by queue-order reliability (seq), not this.
        // NB tuned while the ghost-transition bug inflated ETAs — the learned residual layer
        // (eta_bias, mig 0083) absorbs the recalibration automatically; keep this as a static prior.
        const DS_WORK_ETA_BIAS_S: i64 = 600;
        // Scheduled crane stops: the terminal pauses at the MYT 00/08/16 shift/meal boundaries —
        // measured on the shadow validation as prediction-error spikes of +436..+608s exactly at
        // those three hours (the move-time cadence deliberately excludes gaps>300s, so work-ETA
        // otherwise assumes a crane that never rests). Charge one stall per boundary the work waits
        // through. MYT 00/08/16 = UTC 16/00/08 = epoch multiples of 8h, so a division counts them.
        const SHIFT_BREAK_S: i64 = 500;
        fn shift_breaks_between(a: DateTime<Utc>, b: DateTime<Utc>) -> i64 {
            if b <= a {
                return 0;
            }
            b.timestamp().div_euclid(28_800) - a.timestamp().div_euclid(28_800)
        }
        // "10D-D" → (bay "10", deck/hold 'D', job 'D')
        fn parse_q(qn: &str) -> Option<(String, char, char)> {
            let dash = qn.find('-')?;
            if dash < 2 { return None; }
            let dh = qn.as_bytes()[dash - 1] as char;
            let job = *qn.as_bytes().get(dash + 1)? as char;
            Some((qn[..dash - 1].to_string(), dh, job))
        }
        for qc in &mut qcs {
            let qc_id = qc.qc.clone();
            let mut by_vessel: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for (i, q) in qc.queues.iter().enumerate() {
                by_vessel.entry(q.vessel.clone()).or_default().push(i);
            }
            for (vessel, mut idxs) in by_vessel {
                let Some(&dep) = estdep.get(&vessel) else { continue };
                // must-finish target = the tighter of (departure − buffer) and ESTWKC. GUARD: this
                // terminal's ESTWKC is often stale garbage (verified: many vessels show work-complete
                // DAYS before berthing — impossible), so only trust it when it's a plausible
                // work-complete time, i.e. 0–6h before departure. Otherwise fall back to departure−buffer.
                let dep_target = dep - chrono::Duration::seconds(FINISH_BUFFER_S);
                let finish_by = match estwkc.get(&vessel) {
                    Some(&w) if w < dep_target && (dep - w) <= chrono::Duration::hours(6) => w,
                    _ => dep_target,
                };
                let twin = twin_frac.get(&vessel).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                let move_factor = 1.0 - twin / 2.0; // containers → moves
                idxs.sort_by_key(|&i| qc.queues[i].seq.unwrap_or(i32::MAX));
                let mut procs: Vec<f64> = Vec::with_capacity(idxs.len());
                let mut prev: Option<(String, char, char)> = None;
                for &i in &idxs {
                    let cur = parse_q(&qc.queues[i].queuename);
                    let job = cur.as_ref().map(|c| c.2).unwrap_or('D');
                    let move_s = move_time
                        .get(&(qc_id.clone(), job))
                        .copied()
                        .unwrap_or(if job == 'L' { LD_MOVE_S } else { DS_MOVE_S });
                    let remaining = qc.queues[i].remaining.max(0);
                    let mut p = (remaining as f64) * move_factor * move_s;
                    // Transition overhead (gantry/hatch) only for queues the crane still has to WORK.
                    // A completed queue (remaining=0) is behind the crane: charging it a transition
                    // added a GHOST ~180s per finished bay that inflated every later bay's work-ETA
                    // (~21 min on an 8-bay-done vessel; measured: DS med error −579s on 4+-done
                    // vessels vs −189s on 0–3) — the crane looked busier than it is, so Stage-1
                    // under-ranked its urgency. `prev` still advances through completed queues so the
                    // first remaining queue is charged the one REAL transition from the crane's
                    // current bay.
                    if remaining > 0 {
                        if let (Some((pb, pdh, _)), Some((cb, cdh, _))) = (&prev, &cur) {
                            if pb != cb {
                                p += BAY_CHANGE_S;
                            } else if job == 'L' && *pdh == 'H' && *cdh == 'D' {
                                p += HATCH_LD_S;
                            } else if job != 'L' && *pdh == 'D' && *cdh == 'H' {
                                p += HATCH_DS_S;
                            }
                        }
                    }
                    procs.push(p);
                    prev = cur;
                }
                let total: f64 = procs.iter().sum();
                let mut cum_after = 0.0_f64; // work scheduled after bay k
                for k in (0..idxs.len()).rev() {
                    let qi = idxs[k];
                    qc.queues[qi].deadline_ts = Some(finish_by - chrono::Duration::seconds(cum_after as i64));
                    // when the QC starts this bay = now + work scheduled before it (+ DS calibration)
                    let before = (total - cum_after - procs[k]).max(0.0);
                    let job = parse_q(&qc.queues[qi].queuename).map(|c| c.2).unwrap_or('D');
                    let bias = if job == 'L' { 0 } else { DS_WORK_ETA_BIAS_S };
                    // + learned residual (per-crane, else global-jobtype, else 0) + shift-break stalls
                    let learned = eta_bias
                        .get(&(qc_id.clone(), job))
                        .or_else(|| eta_bias.get(&(String::new(), job)))
                        .copied()
                        .unwrap_or(0);
                    let raw = eta_anchor + chrono::Duration::seconds(before as i64 + bias + learned);
                    let brk = shift_breaks_between(eta_anchor, raw) * SHIFT_BREAK_S;
                    qc.queues[qi].work_eta_ts = Some(raw + chrono::Duration::seconds(brk));
                    qc.queues[qi].proc_s = Some(procs[k] as i64);
                    cum_after += procs[k];
                }
                if qc.vessels.first().map(|v| v == &vessel).unwrap_or(false) {
                    qc.estdep_ts = Some(dep);
                    qc.work_left_s = Some(total as i64);
                    // slack vs the must-finish target = min(departure − buffer, ESTWKC); work-ETA stays buffer-free
                    qc.slack_s = Some((finish_by - now).num_seconds() - total as i64);
                }
            }
        }
    }

    // global urgent front: active moves with a QC + ETW, soonest first, capped.
    // (drops the few orphan rows whose queue is gone and whose ETW is stale)
    let mut front: Vec<MoveOut> = moves
        .iter()
        .filter(|m| (m.etw_accurate.or(m.etw_ts)).is_some() && m.qc.as_deref().is_some_and(|s| !s.is_empty()))
        .map(to_move)
        .collect();
    front.sort_by_key(|m| m.etw_accurate.or(m.etw_ts));
    front.truncate(POOL_CAP);

    let active_moves = moves.iter().filter(|m| m.ytno.as_deref().is_some_and(|s| !s.is_empty())).count();
    let total_remaining: i64 = qcs.iter().map(|q| q.remaining).sum();

    // ── candidate job pool (unassigned demand), urgency-ranked ──
    let cand_rows: Vec<CandidateRow> = sqlx::query_as(
        "SELECT qc, queuename, vessel, jobtype, src_block, rtg, n FROM live_candidate",
    )
    .fetch_all(&pool)
    .await?;

    // per-QC queue list (queuename, seq, done, total) for deriving urgency
    struct QInfo { queuename: String, seq: i32, done: i32, total: i32 }
    let mut qc_queues: BTreeMap<String, Vec<QInfo>> = BTreeMap::new();
    for q in &queues {
        qc_queues.entry(q.qc.clone()).or_default().push(QInfo {
            queuename: q.queuename.clone(),
            seq: q.seq.unwrap_or(i32::MAX),
            done: q.comp_qty.unwrap_or(0),
            total: q.total_qty.unwrap_or(0),
        });
    }

    let candidate_total: i64 = cand_rows.iter().map(|c| c.n as i64).sum();
    let mut candidates: Vec<CandidateOut> = cand_rows
        .iter()
        .map(|c| {
            // "moves until this work is reached" = remaining in the QC's active queue(s)
            // + total of not-yet-started queues that come before this one. 0 if this is
            // the queue the QC is working right now.
            let (mut moves_until, mut active) = (i64::MAX, false);
            if let Some(qc) = c.qc.as_deref() {
                if let Some(qs) = qc_queues.get(qc) {
                    if let Some(mine) = qs.iter().find(|q| q.queuename == c.queuename) {
                        let active_rem: i64 = qs.iter()
                            .filter(|q| q.done > 0 && q.done < q.total)
                            .map(|q| (q.total - q.done) as i64)
                            .sum();
                        if mine.done > 0 && mine.done < mine.total {
                            active = true;
                            moves_until = 0;
                        } else {
                            let before: i64 = qs.iter()
                                .filter(|q| q.done == 0 && q.seq < mine.seq)
                                .map(|q| q.total as i64)
                                .sum();
                            moves_until = active_rem + before;
                        }
                    }
                }
            }
            CandidateOut {
                qc: c.qc.clone(),
                queuename: c.queuename.clone(),
                vessel: c.vessel.clone(),
                jobtype: c.jobtype.clone(),
                src_block: c.src_block.clone(),
                rtg: c.rtg.clone(),
                n: c.n,
                moves_until,
                active,
            }
        })
        .collect();
    // soonest-needed first (active queues first), then larger demand
    candidates.sort_by(|a, b| a.moves_until.cmp(&b.moves_until).then(b.n.cmp(&a.n)));

    Ok(WorkpoolOut {
        as_of,
        qc_count: qcs.len(),
        active_moves,
        total_remaining,
        qcs,
        pool: front,
        candidates,
        candidate_total,
    })
}

/// SHADOW VALIDATION: log Stage-1 predictions and their ground truth. Every 2 min, for the front
/// containers of each working QC, record (predicted work-ETA, dispatch deadline, assigned, slack);
/// mark a row resolved when its container leaves the pool (≈ actually worked). Each container is
/// logged once while open. Powers the accuracy (resolved vs predicted) + effect (late-unassigned vs
/// real starvation) evaluation. Display-only data; never drives dispatch.
pub fn spawn_dispatch_pred_logger(pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(120));
        let mut tick = 0u64;
        loop {
            ticker.tick().await;
            tick += 1;
            let Ok(wp) = build_workpool(pool.clone()).await else { continue };
            // every container we can currently see (across all QCs)
            let present: Vec<String> = {
                let mut s: HashSet<String> = HashSet::new();
                for qc in &wp.qcs {
                    for m in &qc.moves {
                        if let Some(c) = &m.contno { s.insert(c.clone()); }
                    }
                }
                s.into_iter().collect()
            };
            if present.is_empty() {
                continue;
            }
            // (0) D_tos capture: record the FIRST tick each open container is seen assigned (ytno
            // present) ≈ TOS's dispatch time. tos_upd_dt = the row's TOS UPD_DT (assignment-OR-LATER
            // upper bound — UPD_DT is a generic last-update, but at first-assigned sighting it's
            // usually the assignment); became_assigned_at = now() (poll-lagged, ≤~3.5min late). MUST
            // run BEFORE the resolve below, else a container assigned+worked within one tick gap
            // stays NULL and would be mis-read as "never assigned". GROUP BY dedups twin/duplicate
            // contno so each gets one deterministic upd.
            let (mut as_c, mut as_u): (Vec<String>, Vec<Option<DateTime<Utc>>>) = (Vec::new(), Vec::new());
            for qc in &wp.qcs {
                for m in &qc.moves {
                    if m.ytno.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false) {
                        if let Some(c) = &m.contno {
                            as_c.push(c.clone());
                            as_u.push(m.upd_ts);
                        }
                    }
                }
            }
            if !as_c.is_empty() {
                let _ = sqlx::query(
                    "UPDATE dispatch_pred_sample d
                        SET became_assigned_at = now(), became_assigned_tick = $3, tos_upd_dt = v.upd
                       FROM (SELECT contno, min(upd) AS upd
                               FROM (SELECT unnest($1::text[]) AS contno, unnest($2::timestamptz[]) AS upd) z
                              GROUP BY contno) v
                      WHERE d.contno = v.contno AND d.resolved_at IS NULL AND d.became_assigned_at IS NULL",
                )
                .bind(&as_c)
                .bind(&as_u)
                .bind(tick as i64)
                .execute(&pool)
                .await;
            }
            // (1a) DS containers are "worked" the instant the QC discharges (actv_ts ≈ QC move
            // complete, verified ~0s) — resolve those with their actv_ts (exact ground truth). LD
            // has no such per-container signal, so it falls through to pool-leave (1b, ~lagged).
            let (mut ds_c, mut ds_t): (Vec<String>, Vec<DateTime<Utc>>) = (Vec::new(), Vec::new());
            for qc in &wp.qcs {
                for m in &qc.moves {
                    if m.jobtype.as_deref() == Some("DS") {
                        if let (Some(c), Some(a)) = (&m.contno, m.actv_ts) {
                            ds_c.push(c.clone());
                            ds_t.push(a);
                        }
                    }
                }
            }
            if !ds_c.is_empty() {
                let _ = sqlx::query(
                    "UPDATE dispatch_pred_sample d SET resolved_at = v.actv
                       FROM (SELECT unnest($1::text[]) AS contno, unnest($2::timestamptz[]) AS actv) v
                      WHERE d.contno = v.contno AND d.resolved_at IS NULL",
                )
                .bind(&ds_c)
                .bind(&ds_t)
                .execute(&pool)
                .await;
            }
            // (1b) anything else that left the pool = worked (LD, or DS we missed) — now (≈ lagged)
            let _ = sqlx::query(
                "UPDATE dispatch_pred_sample SET resolved_at=now() WHERE resolved_at IS NULL AND contno <> ALL($1)",
            )
            .bind(&present)
            .execute(&pool)
            .await;
            // (2) containers already logged & still open → skip (log each once)
            let open: HashSet<String> = sqlx::query_scalar::<_, String>(
                "SELECT DISTINCT contno FROM dispatch_pred_sample WHERE resolved_at IS NULL",
            )
            .fetch_all(&pool)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
            // (3) log the front (≤6 new) containers of each QC's primary vessel
            for qc in &wp.qcs {
                let Some(prim) = qc.vessels.first() else { continue };
                let mut bay: HashMap<(&str, &str), (DateTime<Utc>, i64, i32)> = HashMap::new();
                for b in &qc.queues {
                    if let (Some(eta), Some(p)) = (b.work_eta_ts, b.proc_s) {
                        bay.insert((b.vessel.as_str(), b.queuename.as_str()), (eta, p, b.remaining.max(1)));
                    }
                }
                // order by genuine work order (bay sequence, then ETW within a bay) so the "front"
                // is the next containers to be worked — stable across ticks (ETW alone is unstable
                // when many near-term containers have no ETW yet, re-logging a different set each tick).
                let seq_of: HashMap<&str, i32> = qc
                    .queues
                    .iter()
                    .filter(|b| &b.vessel == prim)
                    .map(|b| (b.queuename.as_str(), b.seq.unwrap_or(i32::MAX)))
                    .collect();
                let mut fronts: Vec<&MoveOut> = qc
                    .moves
                    .iter()
                    .filter(|m| &m.vessel == prim && m.contno.is_some()
                        && !(m.jobtype.as_deref() == Some("DS") && m.actv_ts.is_some()))
                    .collect();
                fronts.sort_by_key(|m| (
                    seq_of.get(m.queuename.as_str()).copied().unwrap_or(i32::MAX),
                    m.etw_ts.unwrap_or(DateTime::<Utc>::MAX_UTC),
                    m.contno.as_deref().unwrap_or(""), // stable tiebreak so the front-6 is the SAME set each tick
                ));
                let mut idx: HashMap<(&str, &str), i32> = HashMap::new();
                let mut logged = 0;
                for m in fronts.into_iter().take(20) {
                    if logged >= 6 {
                        break;
                    }
                    let key = (m.vessel.as_str(), m.queuename.as_str());
                    let i = *idx.get(&key).unwrap_or(&0);
                    idx.insert(key, i + 1);
                    let contno = m.contno.as_ref().unwrap();
                    if open.contains(contno) {
                        continue;
                    }
                    let Some(&(eta, p, rem)) = bay.get(&key) else { continue };
                    let lead: i64 = if m.jobtype.as_deref() == Some("LD") { LEAD_LD_S } else { LEAD_DS_S };
                    let work_eta = eta + chrono::Duration::seconds(((i as f64 / rem as f64) * p as f64) as i64);
                    let deadline = work_eta - chrono::Duration::seconds(lead);
                    let assigned = m.ytno.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
                    // if already assigned at first log, seed D_tos now (else NULL → captured later by (0))
                    let (ba_at, ba_tick, ba_upd): (Option<DateTime<Utc>>, Option<i64>, Option<DateTime<Utc>>) =
                        if assigned { (Some(Utc::now()), Some(tick as i64), m.upd_ts) } else { (None, None, None) };
                    let _ = sqlx::query(
                        "INSERT INTO dispatch_pred_sample
                           (qc, vessel, contno, queuename, jobtype, pred_work_eta_ts, dispatch_deadline_ts, assigned, slack_s, lead_s, became_assigned_at, became_assigned_tick, tos_upd_dt)
                         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
                    )
                    .bind(&qc.qc)
                    .bind(&m.vessel)
                    .bind(contno)
                    .bind(&m.queuename)
                    .bind(&m.jobtype)
                    .bind(work_eta)
                    .bind(deadline)
                    .bind(assigned)
                    .bind(qc.slack_s.map(|v| v as i32))
                    .bind(lead as i32)
                    .bind(ba_at)
                    .bind(ba_tick)
                    .bind(ba_upd)
                    .execute(&pool)
                    .await;
                    logged += 1;
                }
            }
            if tick % 30 == 0 {
                let _ = sqlx::query("DELETE FROM dispatch_pred_sample WHERE logged_at < now() - interval '21 days'")
                    .execute(&pool)
                    .await;
            }
            // learned work-ETA residual layer (mig 0083): refit ~20 min from freshly resolved rows.
            if tick % 10 == 0 {
                let _ = sqlx::query("REFRESH MATERIALIZED VIEW CONCURRENTLY learn_work_eta_bias")
                    .execute(&pool)
                    .await;
            }
        }
    });
}

// ── Stage-2 matching support ─────────────────────────────────────────────────────────────────
/// One unassigned-demand bucket (a Stage-1 candidate) flattened for the Stage-2 shadow matcher,
/// with its QC's work-ETA (→ dispatch deadline) and pickup descriptor. pub(crate) so the matcher
/// (in livemap, which holds the live vehicle GPS) can read it across modules.
pub(crate) struct Stage2Work {
    pub(crate) qc: String,
    pub(crate) vessel: String,
    pub(crate) queuename: String,
    pub(crate) jobtype: String,            // "DS" | "LD"
    pub(crate) src_block: Option<String>,  // LD: pickup block; DS: None (pickup = the QC)
    pub(crate) n: i32,                      // containers in this bucket still needing a truck
    pub(crate) work_eta_ts: Option<DateTime<Utc>>, // when the QC reaches this work (deadline base)
}

/// Build the Stage-2 work-demand list from the same engine the dispatch page uses (build_workpool):
/// each unassigned candidate + its queue's work-ETA. Same-module access to the private WorkpoolOut.
// Dispatch lead = the journey a truck must complete before work_eta, by job type. Grounded in the p75
// of the MEASURED journey (tt_cycle_v2, 5-day, 2026-07-01), p75 = "the deadline should cover 3/4 of
// journeys": DS = 공차이동 to the pickup crane (p50 248 / p75 450); LD = 공차이동+받기+부하이동 to the
// delivery crane (p50 791 / p75 1182). NB the LD journey must be measured DIRECTLY (empty_travel_start →
// laden_arrived) — summing per-stage medians understates it. No crane-approach term (its measured median
// is ~0; see mig 0079). Old fixed values were DS 300 (~p60, a bit tight) / LD 1200 (~p75, already sound).
const LEAD_DS_S: i64 = 450;
const LEAD_LD_S: i64 = 1180;

pub(crate) async fn stage2_work_candidates(pool: PgPool) -> Result<Vec<Stage2Work>, AppError> {
    let wp = build_workpool(pool).await?;
    let mut eta: HashMap<(String, String, String), DateTime<Utc>> = HashMap::new();
    for qc in &wp.qcs {
        for q in &qc.queues {
            if let Some(e) = q.work_eta_ts {
                eta.insert((qc.qc.clone(), q.vessel.clone(), q.queuename.clone()), e);
            }
        }
    }
    let mut out = Vec::new();
    for c in &wp.candidates {
        let Some(qc) = c.qc.clone().filter(|s| !s.is_empty()) else { continue };
        let jt = c.jobtype.clone().unwrap_or_default();
        let work_eta = eta.get(&(qc.clone(), c.vessel.clone(), c.queuename.clone())).copied();
        out.push(Stage2Work {
            qc,
            vessel: c.vessel.clone(),
            queuename: c.queuename.clone(),
            jobtype: jt,
            src_block: c.src_block.clone(),
            n: c.n,
            work_eta_ts: work_eta,
        });
    }
    Ok(out)
}

// ── Stage-2 shadow validation dashboard feed ─────────────────────────────────────────────────
#[derive(Serialize, sqlx::FromRow)]
struct S2Summary {
    matches_30m: i64,
    switched_pct: Option<f64>,
    feasible_pct: Option<f64>,
    routed_pct: Option<f64>,
    median_arrival_s: Option<f64>,
    vehicles: i64,
    works: i64,
}
#[derive(Serialize, sqlx::FromRow)]
struct S2Match {
    ytno: String,
    qc: Option<String>,
    vessel: Option<String>,
    queuename: Option<String>,
    jobtype: Option<String>,
    src_block: Option<String>,
    veh_state: Option<String>,
    arrival_s: Option<i32>,
    deadline_slack_s: Option<i32>,
    feasible: Option<bool>,
    cost_tier: Option<String>,
    switched: Option<bool>,
}
/// "Free truck nearby but the QC is stuck" — the dispatch inefficiency Stage-2 targets: a working QC
/// idle past threshold with no truck at it (starving_real) WHILE empty+unassigned trucks sit within
/// ~600m (near_idle_tt > 0). These are cases TOS left on the table that optimal matching would serve.
#[derive(Serialize, sqlx::FromRow)]
struct S2Ineff {
    starve_ticks: i64,
    with_free_pct: Option<f64>,
    avg_free: Option<f64>,
    qcs: i64,
}

/// Phase-2 solver gain: the adopted deadline-aware optimum vs the simple greedy baseline, over the
/// recent window — total-arrival savings and deadline-miss counts for each.
#[derive(Serialize, sqlx::FromRow)]
struct S2Solver {
    ticks: i64,
    savings_pct: Option<f64>, // (greedy − optimal) / greedy total arrival, %
    greedy_miss: Option<i64>,
    optimal_miss: Option<i64>,
}

#[derive(Serialize)]
pub struct Stage2ShadowOut {
    summary: S2Summary,
    latest_ts: Option<DateTime<Utc>>,
    latest: Vec<S2Match>,
    inefficiency: S2Ineff,
    solver: S2Solver,
}

/// `GET /api/stage2/shadow` — live Stage-2 matching shadow: last-30min summary (thrash, feasibility,
/// OD tier, median arrival) + the most recent tick's recommended vehicle→work matches.
pub async fn stage2_shadow(State(pool): State<PgPool>) -> Result<Json<Stage2ShadowOut>, AppError> {
    let summary: S2Summary = sqlx::query_as(
        "SELECT count(*) AS matches_30m,
                (100.0*count(*) FILTER (WHERE switched)/nullif(count(*),0))::float8 AS switched_pct,
                (100.0*count(*) FILTER (WHERE feasible)/nullif(count(*),0))::float8 AS feasible_pct,
                (100.0*count(*) FILTER (WHERE cost_tier='R')/nullif(count(*),0))::float8 AS routed_pct,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY arrival_s))::float8 AS median_arrival_s,
                count(DISTINCT ytno) AS vehicles,
                count(DISTINCT (qc, queuename, vessel)) AS works
           FROM stage2_match_shadow WHERE ts > now() - interval '30 minutes'",
    )
    .fetch_one(&pool)
    .await?;
    let latest_ts: Option<(Option<DateTime<Utc>>,)> =
        sqlx::query_as("SELECT max(ts) FROM stage2_match_shadow")
            .fetch_optional(&pool)
            .await?;
    let latest_ts = latest_ts.and_then(|r| r.0);
    let latest: Vec<S2Match> = match latest_ts {
        Some(ts) => sqlx::query_as(
            "SELECT ytno, qc, vessel, queuename, jobtype, src_block, veh_state, arrival_s,
                    deadline_slack_s, feasible, cost_tier, switched
               FROM stage2_match_shadow WHERE ts = $1 ORDER BY arrival_s",
        )
        .bind(ts)
        .fetch_all(&pool)
        .await?,
        None => Vec::new(),
    };
    // inefficiency: QC idle-waiting for a truck while a free truck sat nearby (Stage-2 would serve it)
    let inefficiency: S2Ineff = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE starving_real) AS starve_ticks,
                (100.0*count(*) FILTER (WHERE starving_real AND near_idle_tt > 0)
                  / nullif(count(*) FILTER (WHERE starving_real), 0))::float8 AS with_free_pct,
                (avg(near_idle_tt) FILTER (WHERE starving_real AND near_idle_tt > 0))::float8 AS avg_free,
                count(DISTINCT qc) FILTER (WHERE starving_real) AS qcs
           FROM qc_wait_qc_sample WHERE ts > now() - interval '30 minutes'",
    )
    .fetch_one(&pool)
    .await?;
    // phase-2 solver gain: adopted optimum vs greedy baseline (efficiency + deadline misses)
    let solver: S2Solver = sqlx::query_as(
        "SELECT count(*) AS ticks,
                (100.0*sum(greedy_cost_s - optimal_cost_s)/nullif(sum(greedy_cost_s),0))::float8 AS savings_pct,
                sum(greedy_miss)::bigint AS greedy_miss,
                sum(optimal_miss)::bigint AS optimal_miss
           FROM stage2_solver_shadow WHERE ts > now() - interval '30 minutes'",
    )
    .fetch_one(&pool)
    .await?;
    Ok(Json(Stage2ShadowOut { summary, latest_ts, latest, inefficiency, solver }))
}

/// Stage-B advisory: the latest tick's recommended truck→work moves with endpoints, for the live
/// map overlay (operator-facing "send this truck here" — display only, never drives dispatch).
#[derive(Serialize, sqlx::FromRow)]
pub struct S2Advisory {
    ytno: String,
    qc: Option<String>,
    jobtype: Option<String>,
    src_block: Option<String>,
    dest_lat: Option<f64>,
    dest_lon: Option<f64>,
    src_lat: Option<f64>,
    src_lon: Option<f64>,
    arrival_s: Option<i32>,
    feasible: Option<bool>,
}

/// `GET /api/stage2/advisory` — latest recommended moves (with endpoints) for the live-map overlay.
pub async fn stage2_advisory(State(pool): State<PgPool>) -> Result<Json<Vec<S2Advisory>>, AppError> {
    let rows: Vec<S2Advisory> = sqlx::query_as(
        "SELECT ytno, qc, jobtype, src_block, dest_lat, dest_lon, src_lat, src_lon, arrival_s, feasible
           FROM stage2_match_shadow
          WHERE ts = (SELECT max(ts) FROM stage2_match_shadow) AND dest_lat IS NOT NULL",
    )
    .fetch_all(&pool)
    .await?;
    Ok(Json(rows))
}

// ── Dispatch Health (real data, replacing the mock) ──────────────────────────────────────────
#[derive(Serialize, sqlx::FromRow)]
struct HistBucket {
    label: String,
    n: i64,
}
#[derive(Serialize, sqlx::FromRow)]
struct TrendPt {
    hour: DateTime<Utc>,
    thrash_pct: Option<f64>,
    matches: i64,
}
#[derive(Serialize, sqlx::FromRow)]
struct HDecision {
    ts: DateTime<Utc>,
    ytno: String,
    qc: Option<String>,
    queuename: Option<String>,
    jobtype: Option<String>,
    arrival_s: Option<i32>,
    deadline_slack_s: Option<i32>,
    feasible: Option<bool>,
    cost_tier: Option<String>,
    switched: Option<bool>,
}
#[derive(Serialize)]
pub struct HealthDispatchOut {
    up: bool,
    last_tick_age_s: Option<i64>,
    ticks_1h: i64,
    matches_latest: i64,
    thrash_pct: Option<f64>,
    feasible_pct: Option<f64>,
    savings_pct: Option<f64>,
    routed_pct: Option<f64>,
    arr_p50_s: Option<f64>,
    arr_p90_s: Option<f64>,
    arrival_hist: Vec<HistBucket>,
    trend: Vec<TrendPt>,
    decisions: Vec<HDecision>,
}

/// `GET /api/health/dispatch` — real dispatch-engine health from the Stage-2 shadow matcher
/// (replaces the mock): liveness, recommendation volume/stability/feasibility/optimisation gain,
/// arrival-time distribution, hourly stability trend, and the latest recommended decisions.
pub async fn health_dispatch(State(pool): State<PgPool>) -> Result<Json<HealthDispatchOut>, AppError> {
    let (last_tick_age_s, ticks_1h, matches_latest): (Option<i64>, i64, i64) = sqlx::query_as(
        "SELECT extract(epoch FROM now() - max(ts))::int8 AS age,
                count(DISTINCT ts) FILTER (WHERE ts > now() - interval '1 hour') AS ticks_1h,
                count(*) FILTER (WHERE ts = (SELECT max(ts) FROM stage2_match_shadow)) AS latest
           FROM stage2_match_shadow",
    )
    .fetch_one(&pool)
    .await?;
    let up = last_tick_age_s.map(|a| a < 120).unwrap_or(false);

    let (thrash_pct, feasible_pct, routed_pct, arr_p50_s, arr_p90_s): (
        Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>,
    ) = sqlx::query_as(
        "SELECT (100.0*count(*) FILTER (WHERE switched)/nullif(count(*),0))::float8,
                (100.0*count(*) FILTER (WHERE feasible)/nullif(count(*),0))::float8,
                (100.0*count(*) FILTER (WHERE cost_tier='R')/nullif(count(*),0))::float8,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY arrival_s))::float8,
                (percentile_cont(0.9) WITHIN GROUP (ORDER BY arrival_s))::float8
           FROM stage2_match_shadow WHERE ts > now() - interval '30 minutes'",
    )
    .fetch_one(&pool)
    .await?;

    let savings_pct: Option<f64> = sqlx::query_scalar(
        "SELECT (100.0*sum(greedy_cost_s - optimal_cost_s)/nullif(sum(greedy_cost_s),0))::float8
           FROM stage2_solver_shadow WHERE ts > now() - interval '30 minutes'",
    )
    .fetch_one(&pool)
    .await
    .unwrap_or(None);

    let arrival_hist: Vec<HistBucket> = sqlx::query_as(
        "SELECT label, n FROM (
           SELECT CASE WHEN arrival_s < 120 THEN '0–2' WHEN arrival_s < 240 THEN '2–4'
                       WHEN arrival_s < 360 THEN '4–6' WHEN arrival_s < 480 THEN '6–8'
                       WHEN arrival_s < 600 THEN '8–10' WHEN arrival_s < 900 THEN '10–15'
                       ELSE '15+' END AS label,
                  count(*)::int8 AS n, min(arrival_s) AS ord
             FROM stage2_match_shadow
            WHERE ts > now() - interval '1 hour' AND arrival_s IS NOT NULL
            GROUP BY 1) z ORDER BY ord",
    )
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    let trend: Vec<TrendPt> = sqlx::query_as(
        "SELECT date_trunc('hour', ts) AS hour,
                (100.0*count(*) FILTER (WHERE switched)/nullif(count(*),0))::float8 AS thrash_pct,
                count(*)::int8 AS matches
           FROM stage2_match_shadow WHERE ts > now() - interval '24 hours'
          GROUP BY 1 ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    let decisions: Vec<HDecision> = sqlx::query_as(
        "SELECT ts, ytno, qc, queuename, jobtype, arrival_s, deadline_slack_s, feasible, cost_tier, switched
           FROM stage2_match_shadow WHERE ts = (SELECT max(ts) FROM stage2_match_shadow)
          ORDER BY arrival_s LIMIT 12",
    )
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    Ok(Json(HealthDispatchOut {
        up, last_tick_age_s, ticks_1h, matches_latest,
        thrash_pct, feasible_pct, savings_pct, routed_pct, arr_p50_s, arr_p90_s,
        arrival_hist, trend, decisions,
    }))
}

// ── TOS-vs-ours dispatch comparison feed ─────────────────────────────────────────────────────
#[derive(Serialize, sqlx::FromRow)]
struct CompareSummary {
    n: i64,
    divergence_pct: Option<f64>,    // % where the chosen truck differs
    ours_faster_pct: Option<f64>,   // % where our truck would arrive sooner
    avg_delta_s: Option<f64>,       // avg (tos − ours); + = we'd be faster
    median_delta_s: Option<f64>,    // robust to mid-cycle-TOS-truck outliers
    avg_our_arrival_s: Option<f64>,
    avg_tos_arrival_s: Option<f64>,
    same_n: i64,
    ours_closer_n: i64,
    tos_closer_n: i64,
}
#[derive(Serialize, sqlx::FromRow)]
struct CompareRow {
    ts: DateTime<Utc>,
    qc: String,
    queuename: String,
    jobtype: Option<String>,
    tos_ytno: String,
    tos_arrival_s: Option<i32>,
    our_ytno: Option<String>,
    our_arrival_s: Option<i32>,
    agree: Option<bool>,
    reason: Option<String>,
    delta_s: Option<i32>,
}
#[derive(Serialize)]
pub struct DispatchCompareOut {
    summary: CompareSummary,
    recent: Vec<CompareRow>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct ComparePick {
    qc: String,
    queuename: String,
    tos_ytno: String,
    our_ytno: Option<String>,
    our_arrival_s: Option<i32>,
    tos_arrival_s: Option<i32>,
    agree: Option<bool>,
    delta_s: Option<i32>,
}

/// `GET /api/stage2/compare-picks` — per (qc, queuename, tos_ytno) the latest "who WE'd have picked"
/// for works TOS already assigned (from the timing-skew-free comparison). Lets the TT page show OUR
/// pick beside the TOS-assigned truck on assigned rows too (unassigned rows use /api/stage2/advisory).
pub async fn stage2_compare_picks(State(pool): State<PgPool>) -> Result<Json<Vec<ComparePick>>, AppError> {
    let rows = sqlx::query_as::<_, ComparePick>(
        "SELECT DISTINCT ON (qc, queuename, tos_ytno)
                qc, queuename, tos_ytno, our_ytno, our_arrival_s, tos_arrival_s, agree, delta_s
           FROM dispatch_compare_shadow
          WHERE ts > now() - interval '2 days'
          ORDER BY qc, queuename, tos_ytno, ts DESC",
    )
    .fetch_all(&pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Serialize, sqlx::FromRow)]
pub struct WorkPoint {
    qc: String,
    queuename: String,
    jobtype: Option<String>,
    lat: f64,
    lon: f64,
    src_block: Option<String>,
    tos_ytno: Option<String>,
    tos_arrival_s: Option<i32>,
    our_ytno: Option<String>,
    our_arrival_s: Option<i32>,
    agree: Option<bool>,
    delta_s: Option<i32>,
    avg_delta_s: Option<i32>,
    n: i64,
    agree_n: i64,
    tos_trucks: Vec<String>,
    our_trucks: Vec<String>,
}

/// `GET /api/stage2/work-points` — currently-dispatched work points (last hour) for the live map:
/// each point's coordinate (from the matcher's dest_lat/lon) joined with the timing-skew-free
/// TOS-vs-ours comparison (latest TOS truck + who WE'd have picked, agreement, gap). Clicking a
/// point on the map shows TOS's dispatch beside ours.
pub async fn stage2_work_points(State(pool): State<PgPool>) -> Result<Json<Vec<WorkPoint>>, AppError> {
    let rows = sqlx::query_as::<_, WorkPoint>(
        "WITH coords AS (
           SELECT DISTINCT ON (qc, queuename) qc, queuename, dest_lat AS lat, dest_lon AS lon, src_block
             FROM stage2_match_shadow
            WHERE dest_lat IS NOT NULL AND ts > now() - interval '60 minutes'
            ORDER BY qc, queuename, ts DESC
         ),
         agg AS (
           SELECT qc, queuename,
                  count(*) AS n,
                  count(*) FILTER (WHERE agree) AS agree_n,
                  max(jobtype) AS jobtype,
                  (array_agg(tos_ytno      ORDER BY tos_upd DESC))[1] AS tos_ytno,
                  (array_agg(tos_arrival_s ORDER BY tos_upd DESC))[1] AS tos_arrival_s,
                  (array_agg(our_ytno      ORDER BY tos_upd DESC))[1] AS our_ytno,
                  (array_agg(our_arrival_s ORDER BY tos_upd DESC))[1] AS our_arrival_s,
                  (array_agg(agree         ORDER BY tos_upd DESC))[1] AS agree,
                  (array_agg(delta_s       ORDER BY tos_upd DESC))[1] AS delta_s,
                  avg(delta_s)::int AS avg_delta_s,
                  array_agg(DISTINCT tos_ytno)                  AS tos_trucks,  -- all trucks TOS dispatched here (last hour)
                  array_remove(array_agg(DISTINCT our_ytno), NULL) AS our_trucks   -- all trucks WE'd have dispatched
             FROM dispatch_compare_shadow
            WHERE ts > now() - interval '60 minutes'
            GROUP BY qc, queuename
         )
         SELECT a.qc, a.queuename, a.jobtype, c.lat, c.lon, c.src_block,
                a.tos_ytno, a.tos_arrival_s, a.our_ytno, a.our_arrival_s, a.agree, a.delta_s, a.avg_delta_s, a.n, a.agree_n,
                a.tos_trucks, a.our_trucks
           FROM agg a JOIN coords c USING (qc, queuename)",
    )
    .fetch_all(&pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Serialize, sqlx::FromRow, Clone)]
pub struct FairCompare {
    ts: DateTime<Utc>,
    window_min: i32,
    n: i32,
    tos_total_s: i64,
    our_total_s: i64,
    savings_pct: f64,
    same_n: i32,
}
#[derive(Serialize)]
pub struct FairCompareOut {
    latest: Option<FairCompare>,
    avg_savings_pct: Option<f64>, // averaged over the recent rows (stabler than one window)
    recent: Vec<FairCompare>,
}

/// `GET /api/stage2/fair-compare` — the FAIR head-to-head: our solver's optimal 1:1 matching vs TOS's
/// actual matching on the SAME trucks+works+positions (reservation-respected). The honest efficiency
/// number, unlike the per-work "closest truck" metric which double-books the nearest truck.
pub async fn stage2_fair_compare(State(pool): State<PgPool>) -> Result<Json<FairCompareOut>, AppError> {
    let recent: Vec<FairCompare> = sqlx::query_as(
        "SELECT ts, window_min, n, tos_total_s, our_total_s, savings_pct, same_n
           FROM fair_compare_shadow ORDER BY ts DESC LIMIT 48",
    )
    .fetch_all(&pool)
    .await?;
    let latest = recent.first().cloned();
    let avg_savings_pct = if recent.is_empty() {
        None
    } else {
        Some(recent.iter().map(|r| r.savings_pct).sum::<f64>() / recent.len() as f64)
    };
    Ok(Json(FairCompareOut { latest, avg_savings_pct, recent }))
}

#[derive(Serialize, sqlx::FromRow)]
pub struct FairBucket {
    key: String,
    pairs: i64,
    savings_pct: Option<f64>, // % empty-travel saved (Σtos−Σour)/Σtos
    worse_pct: Option<f64>,   // % of pairs where OUR matching is worse than TOS (bias check)
}
#[derive(Serialize)]
pub struct FairBreakdown {
    by_job: Vec<FairBucket>,
    by_hour: Vec<FairBucket>,
    by_dist: Vec<FairBucket>,
    by_crane: Vec<FairBucket>,
    pairs: i64,
    worse_pct: Option<f64>,     // overall % of pairs we make worse
    same_pct: Option<f64>,      // overall % identical to TOS
    median_save_s: Option<f64>, // per-pair median saving (robust to outliers)
    mean_save_s: Option<f64>,
}

/// `GET /api/stage2/fair-breakdown` — breaks the headline empty-travel saving down by jobtype, hour,
/// distance tier and crane, plus bias stats (% of pairs we make WORSE, median per-pair saving) so the
/// ~25% number can be trusted, not taken on faith. Over the last 24h of per-pair fair-compare detail.
pub async fn stage2_fair_breakdown(State(pool): State<PgPool>) -> Result<Json<FairBreakdown>, AppError> {
    // shared aggregate columns; only the GROUP key expression changes per dimension
    const COLS: &str = "count(*) AS pairs,
        round((100.0*(sum(tos_s)-sum(our_s))/nullif(sum(tos_s),0))::numeric,1)::float8 AS savings_pct,
        round((100.0*count(*) FILTER (WHERE our_s>tos_s)/count(*))::numeric,1)::float8 AS worse_pct";
    let win = "ts > now() - interval '24 hours'";
    let by_job: Vec<FairBucket> = sqlx::query_as(&format!(
        "SELECT jobtype AS key, {COLS} FROM fair_compare_detail WHERE {win} GROUP BY jobtype ORDER BY jobtype"
    )).fetch_all(&pool).await?;
    let by_hour: Vec<FairBucket> = sqlx::query_as(&format!(
        "SELECT lpad((extract(hour FROM ts AT TIME ZONE 'Asia/Kuala_Lumpur'))::int::text,2,'0') AS key, {COLS}
           FROM fair_compare_detail WHERE {win} GROUP BY 1 ORDER BY 1"
    )).fetch_all(&pool).await?;
    let by_dist: Vec<FairBucket> = sqlx::query_as(&format!(
        "SELECT CASE WHEN tos_s<120 THEN '1·근거리 <2분' WHEN tos_s<300 THEN '2·중거리 2-5분' ELSE '3·원거리 >5분' END AS key,
           {COLS} FROM fair_compare_detail WHERE {win} GROUP BY 1 ORDER BY 1"
    )).fetch_all(&pool).await?;
    let by_crane: Vec<FairBucket> = sqlx::query_as(&format!(
        "SELECT qc AS key, {COLS} FROM fair_compare_detail WHERE {win} GROUP BY qc ORDER BY count(*) DESC LIMIT 12"
    )).fetch_all(&pool).await?;
    let (pairs, worse_pct, same_pct, median_save_s, mean_save_s): (i64, Option<f64>, Option<f64>, Option<f64>, Option<f64>) =
        sqlx::query_as(&format!(
            "SELECT count(*),
               round((100.0*count(*) FILTER (WHERE our_s>tos_s)/nullif(count(*),0))::numeric,1)::float8,
               round((100.0*count(*) FILTER (WHERE our_s=tos_s)/nullif(count(*),0))::numeric,1)::float8,
               (percentile_cont(0.5) WITHIN GROUP (ORDER BY (tos_s-our_s)))::float8,
               round(avg(tos_s-our_s)::numeric,0)::float8
             FROM fair_compare_detail WHERE {win}"
        )).fetch_one(&pool).await?;
    Ok(Json(FairBreakdown { by_job, by_hour, by_dist, by_crane, pairs, worse_pct, same_pct, median_save_s, mean_save_s }))
}

/// `GET /api/stage2/compare` — TOS's actual dispatch vs our recommendation, per work: divergence
/// rate, who'd arrive sooner, the performance gap, reason breakdown, and recent divergence examples.
pub async fn dispatch_compare(State(pool): State<PgPool>) -> Result<Json<DispatchCompareOut>, AppError> {
    let summary: CompareSummary = sqlx::query_as(
        "SELECT count(*) AS n,
                (100.0*count(*) FILTER (WHERE NOT agree)/nullif(count(*),0))::float8 AS divergence_pct,
                (100.0*count(*) FILTER (WHERE delta_s > 0)/nullif(count(*),0))::float8 AS ours_faster_pct,
                avg(delta_s)::float8 AS avg_delta_s,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY delta_s))::float8 AS median_delta_s,
                avg(our_arrival_s)::float8 AS avg_our_arrival_s,
                avg(tos_arrival_s)::float8 AS avg_tos_arrival_s,
                count(*) FILTER (WHERE reason='same') AS same_n,
                count(*) FILTER (WHERE reason='ours_closer') AS ours_closer_n,
                count(*) FILTER (WHERE reason='tos_closer') AS tos_closer_n
           FROM dispatch_compare_shadow WHERE ts > now() - interval '24 hours' AND reason <> 'now'",
    )
    .fetch_one(&pool)
    .await?;
    let recent: Vec<CompareRow> = sqlx::query_as(
        "SELECT ts, qc, queuename, jobtype, tos_ytno, tos_arrival_s, our_ytno, our_arrival_s, agree, reason, delta_s
           FROM dispatch_compare_shadow WHERE ts > now() - interval '24 hours' AND NOT agree AND reason <> 'now'
          ORDER BY ts DESC LIMIT 25",
    )
    .fetch_all(&pool)
    .await?;
    Ok(Json(DispatchCompareOut { summary, recent }))
}
