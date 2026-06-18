//! Live work pool: per-QC work-queue sequence + the active (in-flight) container moves
//! that need / have a TT. Reads ONLY the Postgres snapshot tables (`live_workqueue`,
//! `live_workpool`) that the extractor refreshes ~every 90s from TOS — the API crate
//! never touches Oracle. The frontend fuses this with the live websocket PLC/GPS.

use std::collections::BTreeMap;

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
                w.contno, w.yt_topos, w.from_pos, w.to_pos, w.twintandem
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
    // v1: containers≈moves (twin ignored ⇒ slight overestimate), move time = jobtype constant.
    {
        let estdep: std::collections::HashMap<String, DateTime<Utc>> =
            sqlx::query_as::<_, (String, Option<DateTime<Utc>>)>(
                "SELECT vessel, estdep_ts FROM live_vessel_schedule WHERE estdep_ts IS NOT NULL")
                .fetch_all(&pool).await.unwrap_or_default()
                .into_iter().filter_map(|(v, t)| t.map(|t| (v, t))).collect();
        let now = Utc::now();
        const DS_MOVE_S: f64 = 90.0;
        const LD_MOVE_S: f64 = 110.0;
        const BAY_CHANGE_S: f64 = 180.0; // gantry travel between bays (extra)
        const HATCH_DS_S: f64 = 340.0;   // discharge deck→hold cover removal (extra)
        const HATCH_LD_S: f64 = 390.0;   // load hold→deck cover placement (extra)
        // "10D-D" → (bay "10", deck/hold 'D', job 'D')
        fn parse_q(qn: &str) -> Option<(String, char, char)> {
            let dash = qn.find('-')?;
            if dash < 2 { return None; }
            let dh = qn.as_bytes()[dash - 1] as char;
            let job = *qn.as_bytes().get(dash + 1)? as char;
            Some((qn[..dash - 1].to_string(), dh, job))
        }
        for qc in &mut qcs {
            let mut by_vessel: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for (i, q) in qc.queues.iter().enumerate() {
                by_vessel.entry(q.vessel.clone()).or_default().push(i);
            }
            for (vessel, mut idxs) in by_vessel {
                let Some(&dep) = estdep.get(&vessel) else { continue };
                idxs.sort_by_key(|&i| qc.queues[i].seq.unwrap_or(i32::MAX));
                let mut procs: Vec<f64> = Vec::with_capacity(idxs.len());
                let mut prev: Option<(String, char, char)> = None;
                for &i in &idxs {
                    let cur = parse_q(&qc.queues[i].queuename);
                    let job = cur.as_ref().map(|c| c.2).unwrap_or('D');
                    let move_s = if job == 'L' { LD_MOVE_S } else { DS_MOVE_S };
                    let mut p = (qc.queues[i].remaining.max(0) as f64) * move_s;
                    if let (Some((pb, pdh, _)), Some((cb, cdh, _))) = (&prev, &cur) {
                        if pb != cb {
                            p += BAY_CHANGE_S;
                        } else if job == 'L' && *pdh == 'H' && *cdh == 'D' {
                            p += HATCH_LD_S;
                        } else if job != 'L' && *pdh == 'D' && *cdh == 'H' {
                            p += HATCH_DS_S;
                        }
                    }
                    procs.push(p);
                    prev = cur;
                }
                let total: f64 = procs.iter().sum();
                let mut cum_after = 0.0_f64;
                for k in (0..idxs.len()).rev() {
                    qc.queues[idxs[k]].deadline_ts = Some(dep - chrono::Duration::seconds(cum_after as i64));
                    cum_after += procs[k];
                }
                if qc.vessels.first().map(|v| v == &vessel).unwrap_or(false) {
                    qc.estdep_ts = Some(dep);
                    qc.work_left_s = Some(total as i64);
                    qc.slack_s = Some((dep - now).num_seconds() - total as i64);
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

    Ok(Json(WorkpoolOut {
        as_of,
        qc_count: qcs.len(),
        active_moves,
        total_remaining,
        qcs,
        pool: front,
        candidates,
        candidate_total,
    }))
}
