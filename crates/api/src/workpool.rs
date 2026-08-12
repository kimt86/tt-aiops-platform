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
    /// TOS 가 이 트럭을 배차한 시각(YT_DIS_DT·mig 0148). `upd_ts` 는 행 갱신에 밀리는 대리값이다.
    yt_dis_ts: Option<DateTime<Utc>>,
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
    /// TOS row last-update; internal (validation logger), not in JSON.
    /// ⚠ 배차 시각이 **아니다** — 그건 아래 `yt_dis_ts` 다(mig 0148).
    #[serde(skip)]
    upd_ts: Option<DateTime<Utc>>,
    /// D_tos(= TOS 가 이 트럭을 배차한 시각)의 권위값. internal, not in JSON.
    #[serde(skip)]
    yt_dis_ts: Option<DateTime<Utc>>,
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
    /// work_eta_ts 에 실제로 더해진 보정 **전부**(초). 원본예측 = work_eta_ts - eta_bias_s.
    /// 이걸 함께 기록해야 보정을 되먹임 없이 추정할 수 있다(mig 0113): 예전에는 보정된 값으로
    /// 잔차를 재고 그 잔차로 보정을 '교체'해서 L_new = R - L_old 라는 진동이 생겼다.
    /// ⚠ mig 0117 전에는 학습항만 담겨 있어 양하에서 정적 상수 +600 이 빠져 있었다 —
    /// 즉 '원본예측'이 원본이 아니었다. 그 상수는 이제 학습값 부재 시의 폴백으로만 남는다.
    /// 두 판이 섞이면 매뷰 중앙값이 떠돌므로 판별자는 dispatch_pred_sample.bias_ver=2 다.
    eta_bias_s: i64,
    /// SHADOW: this bay's total processing seconds (moves + transition overhead).
    proc_s: Option<i64>,
    /// 크레인 단일 타임라인에서 이 큐의 자리(0-based) — 활성 선박 블록 먼저, 다음 블록은 마감
    /// 이른 순, 블록 안은 seq 순. 화면의 구역 나열은 seq 가 아니라 이 값으로 정렬해야 한다
    /// (seq 는 선박별로 1부터 다시 시작 + 큐이름이 선박 간 재사용된다).
    timeline_pos: Option<i32>,
    /// SHADOW(직렬화 제외·pool_mode=3): 크레인 타임라인에서 이 큐보다 앞에 남은 무브 수(들어올림 환산).
    #[serde(skip)]
    pace_before_n: Option<i64>,
    /// SHADOW(직렬화 제외·pool_mode=3): 이 큐의 선박 '마지막 베이'까지 크레인이 해야 할 누적 무브 수.
    #[serde(skip)]
    pace_total_n: Option<i64>,
    /// SHADOW(직렬화 제외·pool_mode=3): 이 선박의 출항 목표(finish_by = min(출항−버퍼, ESTWKC 가드)).
    #[serde(skip)]
    pace_finish_by: Option<DateTime<Utc>>,
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

/// 상자별 권위 배차 마감 + 순서 (P2 재편 2026-08-10, 같은 날 확장). 프론트가 옛 식으로
/// 로컬 재계산하던 것을 끊는다 — 마감·순번은 백엔드 한 곳에서만 계산한다
/// (마감 = 출항 요구 페이스 균등 배분 pool_mode=3 · 순번 = 적부계획 planseq 축 mig 0128).
#[derive(Serialize)]
pub struct BoxDeadlineOut {
    qc: String,
    vessel: String,
    queuename: String,
    /// 트윈 대표 상자(= min contno).
    contno: String,
    /// 이 트럭 몫의 상자 전부(트윈=2·단독=1) — 화면이 어느 상자 번호로도 이 행을 찾게 한다.
    contnos: Vec<String>,
    jobtype: String,
    /// 구역 안 순번(0-based) — 적부계획 planseq 축, 계획에 없는 상자만 발행순 폴백.
    slot_idx: Option<i32>,
    /// TOS 가 이미 트럭을 붙였는가 (배차 진척 표시용).
    tos_assigned: bool,
    dispatch_deadline_ts: Option<DateTime<Utc>>,
    dd_lead_s: Option<i64>,
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
    /// 상자별 권위 배차 마감 — `/api/workpool` 핸들러만 채운다(내부 소비자는 빈 벡터).
    box_deadlines: Vec<BoxDeadlineOut>,
}

const POOL_CAP: usize = 80;

/// `GET /api/workpool` — the live per-QC work pool (Postgres snapshot, ~90s fresh).
/// 상자별 권위 마감(box_deadlines)을 같이 실어 프론트의 로컬 마감 재계산을 없앤다(P2).
pub async fn workpool(State(pool): State<PgPool>) -> Result<Json<WorkpoolOut>, AppError> {
    // ── 표시 필터: 접안 중인 선박만 (2026-08-11 사용자 결정) ─────────────────────────────
    // 추출 창(workqueue.sql: UPD_DT ~1일)이 접안 전 선박의 계획 큐까지 담아, 화면 선박의
    // 절반이 미접안이었다(실측 15/28척·최대 +43h 뒤 접안 예정). 여기(HTTP 응답 조립)서만
    // 거른다 — 매처·예측 로거는 stage2_work_candidates() 를 직접 쓰므로 측정에 영향이 없고,
    // 접안 전 선박은 발행 지시가 0건이라(실측) 매칭 풀에는 원래 들어오지 않는다.
    // 가상선박(RHXX 등)은 스케줄 행 자체가 없어 같은 조건으로 함께 걸러진다.
    // 큐가 voyage 를 들고 있으면 (vessel,voyage) 정확 일치 — 같은 배의 지난 항차 잔재 큐가
    // 되살아나지 않게. voyage 없는 큐만 선박명 폴백.
    let berthed: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT vessel, voyage FROM live_vessel_schedule
          WHERE actber_ts IS NOT NULL AND actdep_ts IS NULL",
    )
    .fetch_all(&pool)
    .await
    .unwrap_or_default();
    let (mut wp, cand) = stage2_work_candidates(pool).await?;
    // 빈 접안 목록은 "터미널이 비었다"보다 스케줄 피드 문제일 가능성이 크다 — 그때는 거르지
    // 않는다(fail-open). 낡은 화면이 빈 화면보다 낫고, OutageBanner 가 피드 정지를 따로 알린다.
    let berth_names: std::collections::HashSet<String> = if berthed.is_empty() {
        Default::default()
    } else {
        let pairs: std::collections::HashSet<(&str, &str)> = berthed
            .iter()
            .filter_map(|(v, voy)| voy.as_deref().map(|y| (v.as_str(), y)))
            .collect();
        let names: std::collections::HashSet<&str> = berthed.iter().map(|(v, _)| v.as_str()).collect();
        for qc in &mut wp.qcs {
            qc.queues.retain(|q| match q.voyage.as_deref() {
                Some(voy) => pairs.contains(&(q.vessel.as_str(), voy)),
                None => names.contains(q.vessel.as_str()),
            });
            qc.remaining = qc.queues.iter().map(|q| q.remaining as i64).sum();
        }
        wp.qcs.retain(|qc| !qc.queues.is_empty() || !qc.moves.is_empty());
        wp.qc_count = wp.qcs.len();
        wp.total_remaining = wp.qcs.iter().map(|q| q.remaining).sum();
        berthed.iter().map(|(v, _)| v.clone()).collect()
    };
    wp.box_deadlines = cand
        .into_iter()
        .filter(|w| berth_names.is_empty() || berth_names.contains(&w.vessel))
        .filter_map(|w| {
            w.contno.map(|contno| BoxDeadlineOut {
                qc: w.qc,
                vessel: w.vessel,
                queuename: w.queuename,
                contnos: if w.contnos.is_empty() { vec![contno.clone()] } else { w.contnos },
                contno,
                jobtype: w.jobtype,
                slot_idx: w.slot_idx,
                tos_assigned: w.tos_assigned,
                dispatch_deadline_ts: w.dispatch_deadline_ts,
                dd_lead_s: w.dd_lead_s,
            })
        })
        .collect();
    Ok(Json(wp))
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
                w.contno, w.yt_topos, w.from_pos, w.to_pos, w.twintandem, w.upd_ts, w.yt_dis_ts
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
        yt_dis_ts: m.yt_dis_ts,
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
                    eta_bias_s: 0,
                    proc_s: None,
                    timeline_pos: None,
                    pace_before_n: None,
                    pace_total_n: None,
                    pace_finish_by: None,
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
        // live_vessel_schedule의 PK는 (vessel, voyage)다. vessel 키 HashMap으로 접으면 어느 항차가
        // 이기는지 비결정적 — 실측(2026-07-27) MTMH가 8/6 항차를 물어 마감이 +10.2일, MTSQ +8.3일이
        // 됐다(실제 출항은 각각 4.6h / 2.0h 후). 큐 행이 자기 voyage를 들고 있으므로(QueueRow.voyage
        // → QueueOut.voyage) 그 키로 조회한다. 검증: 안벽 33척 전부 live_workqueue.voyage ↔ 스케줄
        // 행이 1:1 매칭(행이 없는 건 가상선박 RHXX뿐).
        let sched: Vec<(String, String, Option<DateTime<Utc>>, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> =
            sqlx::query_as("SELECT vessel, voyage, estdep_ts, estwkc_ts, actdep_ts
                              FROM live_vessel_schedule WHERE estdep_ts IS NOT NULL")
                .fetch_all(&pool).await.unwrap_or_default();
        // (vessel,voyage) → (ESTDEP, ESTWKC). 두 값은 반드시 같은 항차 행에서 나와야 한다.
        let mut sched_v: HashMap<(String, String), (DateTime<Utc>, Option<DateTime<Utc>>)> = HashMap::new();
        // voyage가 NULL인 큐를 위한 선박 폴백: 미출항 행 우선, 그다음 가장 이른 ETD.
        // ⚠ `WHERE actdep_ts IS NULL`로 거르면 안 된다 — 실제 출항 후에도 큐가 남은 선박이 오늘 4척
        //   (XCAH/QSTH/TSKE/CUMS) 있고, 그 마감이 통째로 사라진다(커버리지 조용한 하락).
        let mut sched_fb: HashMap<String, (bool, DateTime<Utc>, Option<DateTime<Utc>>)> = HashMap::new();
        for (v, voy, dep, wkc, act) in &sched {
            let Some(dep) = *dep else { continue };
            sched_v.insert((v.clone(), voy.clone()), (dep, *wkc));
            let key = (act.is_some(), dep);
            match sched_fb.get(v) {
                Some((a, d, _)) if (*a, *d) <= key => {}
                _ => { sched_fb.insert(v.clone(), (act.is_some(), dep, *wkc)); }
            }
        }
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
        // 벽시계 리듬 오버레이 — learn_qc_move_time(활동만·300초 컷)은 낙관적이므로 learn_qc_wall_cadence
        // (같은 구역 연속 간격의 진짜 벽시계 평균)가 있으면 그 값이 이긴다 (mig 0131).
        let wall_rows: Vec<(String, String, Option<i32>)> = sqlx::query_as(
            "SELECT qc, jobtype, wall_s FROM learn_qc_wall_cadence WHERE wall_s IS NOT NULL")
            .fetch_all(&pool).await.unwrap_or_default();
        for (qc, jt, wall_s) in &wall_rows {
            if let Some(wall_s) = wall_s {
                move_time.insert((qc.clone(), if jt == "LD" { 'L' } else { 'D' }), *wall_s as f64);
            }
        }
        // learned work-ETA residual: median(crane start − RAW prediction) per (crane, jobtype) from
        // the shadow validation, DISPATCH-BAND horizon only (5–45 min; far predictions are
        // re-plan-polluted). mig 0083, rebuilt by 0113, truth corrected by 0115; refreshed ~20 min here.
        //
        // ⚠ NOT an integral controller — an earlier version of this comment claimed it was, and that
        // claim WAS the bug. The residual is now measured against the **raw** prediction
        // (pred − applied_bias_s), so this is a one-shot estimate of a fixed offset:
        // L = median(truth − raw). It does not converge toward 0 and must not. The old form measured
        // against the *corrected* prediction, giving L_new = R − L_old — an oscillator, not a
        // controller (measured pair: bias 693 ↔ residual 589 for LD). See db/migrations/0113.
        //
        // ⚠⚠ The numbers that used to sit here were measured against qc_move_log.st_ts, which mig 0113
        // wrongly took for the crane's physical start; it is the DISPATCH instant. They are retracted.
        // Truth is now comp_ts (mig 0115). First measurement on the corrected truth, 5–45 min window:
        //   DS median  +264s
        //   LD median +1004s
        // The DS/LD asymmetry is REAL — 0113's "they became identical" was an artifact of scoring both
        // against dispatch. Expect the spread to stay wide (the pre-correction MAE was ~790s = 13 min
        // and nothing about that came from the bias term), so a per-crane median off a few dozen rows
        // is still noise, not signal. Hence the per-crane floor below.
        // ⚠⚠ 작업도달 **보정은 폐기했다**(2026-08-04 사용자 지시). 예측에 더하지 않는다.
        //
        // 왜: 보정이 예측을 다듬는 게 아니라 **삼켰다**. 값이 +1,667~2,547초까지 커지자 각 크레인의
        // 첫 구역은 앞에 밀린 작업이 0이라 남는 게 보정값뿐이 되어, 크레인이 달라도 전부 같은
        // 값(≈36분 뒤)이 나왔다. 그 결과 "가장 급한 일"이 영원히 36분 뒤라 설계 ③이 아무것도
        // 담지 못했다.
        //
        // 그리고 보정 없이가 **더 정확했다**. 크레인 26대 대조(2026-08-04): 예측 vs 실제 다음 무브
        // 차이 **201초**, 어느 구역을 할지 **80.8%** 적중. 보정을 켰을 때는 28~46분씩 틀렸다.
        // ⇒ 보정이 없어서 부정확한 게 아니라 보정이 있어서 부정확했다.
        //
        // `learn_work_eta_bias` 매뷰는 **지우지 않는다** — 이제 보정기가 아니라 **정확도 계기**다.
        // median(정답 − 예측)을 계속 재므로 예측이 나빠지면 거기서 먼저 드러난다. 되살리려면
        // 이 자리에 조회를 되돌리면 되지만, 위 실측을 뒤집을 근거가 먼저 있어야 한다.
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
            // ── 크레인 단일 타임라인 ───────────────────────────────────────────────────────────
            // 크레인 하나가 여러 배를 맡는 건 예외가 아니라 기본이다(실측 2026-07-27: 잔여작업이
            // 있는 67개 크레인 중 41개가 2척 이상, 최대 3척). 예전에는 (크레인,선박) 그룹마다
            // 시간을 독립으로 쌓아서 **같은 크레인에 걸린 다른 배의 작업이 안 보였다** — 크레인은
            // 한 번에 한 베이만 하는데 배마다 자기 시계를 따로 갖는 셈이라 물리적으로 틀린 모델.
            // 실례(Z2): 크레인이 BWSS로 옮겨가 MTMH의 남은 적하 73개가 BWSS 186무브 뒤에 밀렸는데,
            // MTMH 그룹만 보면 앞선 작업이 전부 완료(procs≈0)라 work-ETA가 "지금", 여유 +6분으로
            // 나왔다. 실제로는 도달까지 ~5.7시간인데 출항까지 2.8시간 = 불가능.
            // ⇒ 시간축은 크레인당 하나. 다만 **배 사이 순서를 seq로 섞으면 안 된다** — seq는 선박별로
            //   1부터 다시 시작하므로 seq로 정렬하면 두 배가 번갈아 놓이고, 그러면 모든 배의
            //   '마지막 베이'가 타임라인 끝에 몰려 배마다 크레인 전체 작업을 통째로 뒤집어쓴다
            //   (시제품에서 실측: 뒤처진 크레인 16→42개로 과교정). 크레인은 실제로 한 배의 베이들을
            //   묶어서 처리하고 배를 옮긴다. 그래서 **배 블록 단위**로 세운다:
            //     ① 지금 실제로 붙어 있는 배(활성 무브 기준 vessels[0])가 맨 앞 — 관측된 사실
            //     ② 나머지는 마감이 이른 순 — 마감 인지형 터미널이라면 그렇게 처리해야 하고,
            //        우리가 없는 정보를 지어내는 것보다 낫다
            //   블록 안에서는 종전처럼 seq 순. 단일 선박 크레인은 종전과 완전히 동일하다.
            let mut voy_v: BTreeMap<String, String> = BTreeMap::new();
            for q in qc.queues.iter() {
                if let Some(v) = q.voyage.clone() {
                    voy_v.entry(q.vessel.clone()).or_insert(v);
                }
            }
            // 선박별 마감 목표. 스케줄이 없는 선박(가상선박 RHXX 등)은 예측을 내지 않지만, 그
            // 작업도 크레인 시간은 먹으므로 타임라인에서 빼지 않는다(다른 배를 그만큼 밀어낸다).
            let mut finish_by_v: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
            let mut dep_v: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
            let qc_vessels: Vec<String> = qc.queues.iter().map(|q| q.vessel.clone()).collect();
            for vessel in qc_vessels {
                if finish_by_v.contains_key(&vessel) {
                    continue;
                }
                // 큐가 들고 있는 voyage로 정확 조회 → 없으면 선박 폴백(미출항 우선/최이른 ETD)
                let Some((dep, wkc)) = voy_v.get(&vessel)
                    .and_then(|v| sched_v.get(&(vessel.clone(), v.clone())).copied())
                    .or_else(|| sched_fb.get(&vessel).map(|&(_, d, w)| (d, w)))
                else { continue };
                // must-finish target = the tighter of (departure − buffer) and ESTWKC. GUARD: this
                // terminal's ESTWKC is often stale garbage (verified: many vessels show work-complete
                // DAYS before berthing — impossible), so only trust it when it's a plausible
                // work-complete time, i.e. 0–6h before departure. Otherwise fall back to departure−buffer.
                let dep_target = dep - chrono::Duration::seconds(FINISH_BUFFER_S);
                let finish_by = match wkc {
                    Some(w) if w < dep_target && (dep - w) <= chrono::Duration::hours(6) => w,
                    _ => dep_target,
                };
                finish_by_v.insert(vessel.clone(), finish_by);
                dep_v.insert(vessel, dep);
            }
            // 배 블록 순서: 활성 배(0) → 마감 이른 순 → 마감 미상은 맨 뒤. 그 안에서 seq.
            let active_vessel = qc.vessels.first().cloned();
            let mut idxs: Vec<usize> = (0..qc.queues.len()).collect();
            idxs.sort_by(|&a, &b| {
                let rank = |i: usize| -> (u8, DateTime<Utc>, String) {
                    let v = &qc.queues[i].vessel;
                    let is_active = active_vessel.as_deref() == Some(v.as_str());
                    let fb = finish_by_v.get(v).copied().unwrap_or(DateTime::<Utc>::MAX_UTC);
                    (if is_active { 0 } else { 1 }, fb, v.clone())
                };
                rank(a).cmp(&rank(b)).then_with(|| {
                    qc.queues[a].seq.unwrap_or(i32::MAX).cmp(&qc.queues[b].seq.unwrap_or(i32::MAX))
                })
            });
            {
                let mut procs: Vec<f64> = Vec::with_capacity(idxs.len());
                // 무브 수(들어올림 환산) — pool_mode=3 균등 페이스 마감용. procs(초)와 달리
                // 시간이 아니라 개수다: (출항까지 남은 시간)÷(남은 무브 수)가 무브당 배정 시간.
                let mut moves_n: Vec<f64> = Vec::with_capacity(idxs.len());
                let mut prev: Option<(String, char, char)> = None;
                for &i in &idxs {
                    // 트윈 비율은 선박 속성이라 큐마다 그 큐의 선박 것으로 본다(통합 타임라인이라
                    // 그룹당 한 번 잡던 예전과 달리 여기서 조회해야 한다).
                    let twin = twin_frac.get(&qc.queues[i].vessel).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                    let move_factor = 1.0 - twin / 2.0; // containers → moves
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
                    moves_n.push((remaining as f64) * move_factor);
                    prev = cur;
                }
                // 접미합 suffix[j] = procs[j..] 총합 → 구간합을 O(1)로.
                let n = idxs.len();
                let mut suffix = vec![0.0_f64; n + 1];
                let mut suffix_n = vec![0.0_f64; n + 1];
                for k in (0..n).rev() {
                    suffix[k] = suffix[k + 1] + procs[k];
                    suffix_n[k] = suffix_n[k + 1] + moves_n[k];
                }
                // 각 선박의 '마지막 베이'가 통합 타임라인에서 어디인지 (그 배가 끝나는 지점)
                let mut last_of: BTreeMap<String, usize> = BTreeMap::new();
                for (k, &i) in idxs.iter().enumerate() {
                    last_of.insert(qc.queues[i].vessel.clone(), k);
                }
                for (k, &qi) in idxs.iter().enumerate() {
                    // 타임라인 자리는 스케줄 유무와 무관하게 모든 큐가 갖는다(화면 정렬용).
                    qc.queues[qi].timeline_pos = Some(k as i32);
                    let vessel = qc.queues[qi].vessel.clone();
                    // 스케줄 없는 선박은 예측을 내지 않는다(작업 시간은 위에서 이미 타임라인에 반영됨).
                    let Some(&finish_by) = finish_by_v.get(&vessel) else { continue };
                    // when the QC starts this bay = now + work scheduled before it (+ DS calibration).
                    // 앞선 작업 = 통합 타임라인의 procs[0..k] — **다른 배 작업 포함**이 이번 수정의 핵심.
                    let before = (suffix[0] - suffix[k]).max(0.0);
                    let learned = 0i64;
                    let raw = eta_anchor + chrono::Duration::seconds(before as i64 + learned);
                    let brk = shift_breaks_between(eta_anchor, raw) * SHIFT_BREAK_S;
                    qc.queues[qi].work_eta_ts = Some(raw + chrono::Duration::seconds(brk));
                    qc.queues[qi].eta_bias_s = learned;
                    qc.queues[qi].proc_s = Some(procs[k] as i64);
                    // 마감 = 출항 목표 − (이 베이 다음부터 '이 배의 마지막 베이'까지 크레인이 해야 할
                    // 일 전부). 사이에 낀 다른 배 작업도 그 배의 완료를 실제로 늦추므로 포함한다.
                    let last = last_of.get(&vessel).copied().unwrap_or(k);
                    let cum_after = (suffix[k + 1] - suffix[last + 1]).max(0.0);
                    qc.queues[qi].deadline_ts =
                        Some(finish_by - chrono::Duration::seconds(cum_after as i64));
                    // pool_mode=3 균등 페이스 재료: 이 큐 앞의 무브 수 / 선박 마지막 베이까지의
                    // 누적 무브 수 / 출항 목표. 마감 계산은 소비처(stage2_work_candidates)가
                    // 매 틱 fresh now 로 한다 — 값이 아니라 재료를 넘겨야 재앵커가 산다.
                    qc.queues[qi].pace_before_n = Some((suffix_n[0] - suffix_n[k]).round() as i64);
                    qc.queues[qi].pace_total_n =
                        Some((suffix_n[0] - suffix_n[last + 1]).round() as i64);
                    qc.queues[qi].pace_finish_by = Some(finish_by);
                }
                // QC 헤더는 '지금 작업 중인 배'(vessels[0] = 활성 무브 기준) 기준으로 낸다.
                if let Some(vessel) = qc.vessels.first().cloned() {
                    if let (Some(&finish_by), Some(&last)) =
                        (finish_by_v.get(&vessel), last_of.get(&vessel))
                    {
                        // 이 배를 끝내려면 크레인이 지금부터 해야 할 총 시간 = procs[0..=last].
                        // 예전엔 그 배의 작업만 셌다 → 크레인을 나눠 쓰면 여유가 과대평가됐다.
                        let need = (suffix[0] - suffix[last + 1]).max(0.0);
                        qc.estdep_ts = dep_v.get(&vessel).copied();
                        qc.work_left_s = Some(need as i64);
                        // slack vs the must-finish target = min(departure − buffer, ESTWKC); work-ETA stays buffer-free
                        qc.slack_s = Some((finish_by - now).num_seconds() - need as i64);
                    }
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
        box_deadlines: Vec::new(), // HTTP 핸들러만 채운다
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
            //
            // ★tos_dis_ts 도 여기서 같이 채운다(mig 0151). block(3) 과 **같은 두 컬럼을 같은
            //   뜻으로** 써야 한다 — 한쪽만 바꿨다가 tos_upd_dt 한 칸에 UPD_DT 와 YT_DIS_DT 가
            //   섞였고, 두 경로가 `became_assigned_at IS NULL` 로 상호배타라 눈에 안 띄었다.
            #[allow(clippy::type_complexity)]
            let (mut as_c, mut as_u, mut as_d): (Vec<String>, Vec<Option<DateTime<Utc>>>, Vec<Option<DateTime<Utc>>>) =
                (Vec::new(), Vec::new(), Vec::new());
            for qc in &wp.qcs {
                for m in &qc.moves {
                    if m.ytno.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false) {
                        if let Some(c) = &m.contno {
                            as_c.push(c.clone());
                            as_u.push(m.upd_ts);
                            as_d.push(m.yt_dis_ts);
                        }
                    }
                }
            }
            if !as_c.is_empty() {
                let _ = sqlx::query(
                    "UPDATE dispatch_pred_sample d
                        SET became_assigned_at = now(), became_assigned_tick = $4,
                            tos_upd_dt = v.upd, tos_dis_ts = v.dis
                       FROM (SELECT contno, min(upd) AS upd, min(dis) AS dis
                               FROM (SELECT unnest($1::text[]) AS contno,
                                            unnest($2::timestamptz[]) AS upd,
                                            unnest($3::timestamptz[]) AS dis) z
                              GROUP BY contno) v
                      WHERE d.contno = v.contno AND d.resolved_at IS NULL AND d.became_assigned_at IS NULL",
                )
                .bind(&as_c)
                .bind(&as_u)
                .bind(&as_d)
                .bind(tick as i64)
                .execute(&pool)
                .await;
            }
            // (1a) Resolve from the CRANE'S OWN RECORD = `qc_move_log.comp_ts`, the instant the
            // crane↔truck handover COMPLETED (DS: crane set the box on the truck · LD: crane lifted
            // it off). That is the event `work_eta_ts` predicts and the moment the truck must have
            // arrived by, so it is also the Stage-2 deadline.
            //
            // ⚠ Do NOT use `st_ts` here. mig 0113 did, on the assumption that "st" meant the crane's
            // physical start. It does not — `st_ts` (Oracle MCH_OPERATION.ST_DT) is the instant the
            // JOB WAS DISPATCHED. Measured 2026-08-03 over 24h: st_ts equals tt_move_log.dispatch_ts
            // (TOS YT_DIS_DT) in 100.00% of DS rows (n=10,142) and 99.63% of LD (n=9,933), while
            // comp_ts equals pickup_ts for DS and free_ts for LD in 100.00% of rows. The decisive
            // physical check: a crane lifts one box at a time, yet consecutive [st_ts, comp_ts]
            // intervals on the SAME crane overlap 74–91% of the time. See db/migrations/0115.
            //
            // ⚠ And do NOT read a job-type symmetry as proof the truth is right. 0113's success
            // signal was "DS and LD residuals became identical (−138s)". That symmetry was the
            // SYMPTOM: both were being measured against dispatch, which our prediction is itself
            // anchored near. On the real truth the asymmetry is real (DS +264s, LD +1004s).
            //
            // The pre-0113 history is still worth keeping: this used to apply to DS only, because a
            // comment claimed "LD has no such per-container signal". That was simply wrong — 75,201
            // LD rows carry contno + comp_ts, live, on par with DS's 69,399. The claim came from
            // looking at ONE source (the live work-pool snapshot, which only carries actv_ts for DS)
            // and concluding the signal existed nowhere.
            //
            // Overwrites a row already resolved as 'pool' or by the retired 'qc' rule: the pool-leave
            // tick can beat the extractor (90s vs ~2min), so the accurate value often arrives second
            // and must win. No index needed beyond qc_move_log_cont_idx — its leading (contno,
            // jobtype) is enough at 1–2 rows per container (measured plan: 9.5ms).
            let _ = sqlx::query(
                "WITH pick AS (
                   SELECT d.id, m.comp_ts
                     FROM dispatch_pred_sample d
                     JOIN LATERAL (
                       SELECT q.comp_ts FROM qc_move_log q
                        WHERE q.contno = d.contno AND q.jobtype = d.jobtype
                          AND q.comp_ts >= d.logged_at
                          AND q.comp_ts <  d.logged_at + interval '6 hours'
                        ORDER BY q.comp_ts LIMIT 1
                     ) m ON true
                    WHERE (d.resolved_at IS NULL OR d.resolved_src IN ('pool', 'qc'))
                      AND d.logged_at > now() - interval '12 hours'
                 )
                 UPDATE dispatch_pred_sample d
                    SET resolved_at = p.comp_ts, resolved_src = 'qc_comp'
                   FROM pick p WHERE d.id = p.id",
            )
            .execute(&pool)
            .await;
            // (1b) Left the pool but the crane record has not landed (or never will) — close the row
            // with the lagged pool-leave time and TAG it, so the bias matview can ignore it (0113/0115).
            // Letting this value teach the corrector is precisely what produced the runaway above.
            let _ = sqlx::query(
                "UPDATE dispatch_pred_sample SET resolved_at=now(), resolved_src='pool'
                  WHERE resolved_at IS NULL AND contno <> ALL($1)",
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
            // (3) log a spread-out sample (≤6 per QC: front/middle/back) of the wired per-box
            // Stage-2 prediction (Stage2Work.work_eta_ts) — replaces the old front-6 formula
            // logger, which recorded a different, unwired legacy calculation (bay-ETA + i/rem×p,
            // ETW order). Scoring already ran above (blocks 0/1a/1b/2); this only records new
            // predictions. pred_ver=2 tags these rows so analysis never mixes them with the
            // legacy population (mig 0130).
            let cand = match stage2_work_candidates(pool.clone()).await { Ok(v) => v.1, Err(_) => continue };
            // contno → (UPD_DT, 배차 시각) — same source/filter as block (0) above.
            //
            // ★두 값을 **각자의 컬럼에** 담는다(mig 0151). 2026-08-11 에 여기만 yt_dis_ts 로
            //   바꿨다가 tos_upd_dt 한 컬럼에 두 정의가 섞였다 — block(0) 은 UPD_DT 를 쓰는데
            //   두 경로가 `became_assigned_at IS NULL` 로 상호배타라 눈에 안 띄었고, 가르는 선이
            //   **배차 시점 자체와 상관**돼 D_tos 분석이 조용히 두 정의를 섞었다(2차 리뷰 지적).
            //   tos_upd_dt = UPD_DT(상한·원래 뜻) / tos_dis_ts = YT_DIS_DT(권위값).
            let dtos_of: HashMap<String, (Option<DateTime<Utc>>, Option<DateTime<Utc>>)> = {
                let mut m: HashMap<String, (Option<DateTime<Utc>>, Option<DateTime<Utc>>)> = HashMap::new();
                for qc in &wp.qcs {
                    for mv in &qc.moves {
                        if mv.ytno.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false) {
                            if let Some(c) = &mv.contno {
                                m.insert(c.clone(), (mv.upd_ts, mv.yt_dis_ts));
                            }
                        }
                    }
                }
                m
            };
            let mut by_qc: HashMap<&str, Vec<&Stage2Work>> = HashMap::new();
            for w in &cand {
                if w.contno.is_none() || w.work_eta_ts.is_none() {
                    continue;
                }
                by_qc.entry(w.qc.as_str()).or_default().push(w);
            }
            for (_, mut rows) in by_qc {
                rows.sort_by_key(|w| w.work_eta_ts);
                let n = rows.len();
                // front + middle + back so the residual sample isn't front-biased. n≤6 → keep all
                // (the {n/2-1, n-2, ...} formula underflows for tiny n, e.g. n=1).
                let idxs: Vec<usize> = if n <= 6 {
                    (0..n).collect()
                } else {
                    let mut v = vec![0, 1, n / 2 - 1, n / 2, n - 2, n - 1];
                    v.sort_unstable();
                    v.dedup();
                    v
                };
                for i in idxs {
                    let w = rows[i];
                    let contno = w.contno.as_ref().unwrap(); // filtered above
                    if open.contains(contno) {
                        continue;
                    }
                    // no default-guessing: skip the row rather than assume a lead time
                    let Some(lead) = w.dd_lead_s else { continue };
                    let assigned = w.tos_assigned;
                    // if already assigned at first log, seed D_tos now (else NULL → captured later by (0))
                    #[allow(clippy::type_complexity)]
                    let (ba_at, ba_tick, ba_upd, ba_dis): (Option<DateTime<Utc>>, Option<i64>, Option<DateTime<Utc>>, Option<DateTime<Utc>>) =
                        if assigned {
                            let (u, d) = dtos_of.get(contno).copied().unwrap_or((None, None));
                            (Some(Utc::now()), Some(tick as i64), u, d)
                        } else {
                            (None, None, None, None)
                        };
                    let _ = sqlx::query(
                        // pred_ver=3: 걸음이 learn_qc_slot_step 로 바뀐 판 (2026-08-10, mig 0139).
                        // 2와 slot 의미는 같다 — 집계는 pred_ver 로 가른다.
                        "INSERT INTO dispatch_pred_sample
                           (qc, vessel, contno, queuename, jobtype, pred_work_eta_ts, dispatch_deadline_ts, assigned, slack_s, lead_s, became_assigned_at, became_assigned_tick, tos_upd_dt, etw_qc_ts, applied_bias_s, bias_ver, pred_ver, slot_idx, tos_dis_ts)
                         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,2,3,$16,$17)",
                    )
                    .bind(&w.qc)
                    .bind(&w.vessel)
                    .bind(contno)
                    .bind(&w.queuename)
                    .bind(&w.jobtype)
                    .bind(w.work_eta_ts)
                    .bind(w.dispatch_deadline_ts)
                    .bind(assigned)
                    .bind(None::<i32>)
                    .bind(lead as i32)
                    .bind(ba_at)
                    .bind(ba_tick)
                    .bind(ba_upd)
                    .bind(None::<DateTime<Utc>>)
                    .bind(0i32)
                    .bind(w.slot_idx)
                    .bind(ba_dis) // mig 0151
                    .execute(&pool)
                    .await;
                }
            }
            if tick % 30 == 0 {
                crate::db::prune(&pool, "dispatch_pred_sample", "DELETE FROM dispatch_pred_sample WHERE logged_at < now() - interval '21 days'").await;
            }
            // learned work-ETA residual layer (mig 0083): refit ~20 min from freshly resolved rows.
            if tick % 10 == 0 {
                let _ = sqlx::query("REFRESH MATERIALIZED VIEW CONCURRENTLY learn_work_eta_bias")
                    .execute(&pool)
                    .await;
                let _ = sqlx::query("REFRESH MATERIALIZED VIEW CONCURRENTLY learn_qc_wall_cadence")
                    .execute(&pool)
                    .await;
                let _ = sqlx::query("REFRESH MATERIALIZED VIEW CONCURRENTLY learn_qc_slot_step")
                    .execute(&pool)
                    .await;
                // 채택률 시계열 (mig 0144) — 시간당 ~1점(최근 55분 내 점이 있으면 건너뜀).
                // 보드의 24h 즉석 계산과 같은 잣대·같은 쿼리를 그대로 박제한다.
                let _ = sqlx::query(
                    "INSERT INTO dispatch_adoption_metric
                       (captured_at, window_h, boxes_reco, boxes_dispatched, box_pct, ytno_match_pct)
                     WITH r AS (
                       SELECT contno, min(ts) AS first_ts
                         FROM stage2_match_shadow
                        WHERE ts > now() - interval '24 hours' AND contno IS NOT NULL
                        GROUP BY contno
                     ), d AS (
                       SELECT r.contno, t.ytno, t.dispatch_ts
                         FROM r
                         JOIN LATERAL (
                           SELECT ytno, dispatch_ts FROM tt_move_log t
                            WHERE t.contno = r.contno AND t.dispatch_ts >= r.first_ts
                              AND t.dispatch_ts < r.first_ts + interval '20 minutes'
                            ORDER BY t.dispatch_ts LIMIT 1) t ON true
                     )
                     SELECT now(), 24, (SELECT count(*) FROM r), count(*),
                            (100.0*count(*)/nullif((SELECT count(*) FROM r),0))::float8,
                            (100.0*count(*) FILTER (WHERE EXISTS (
                               SELECT 1 FROM stage2_match_shadow m
                                WHERE m.contno = d.contno AND m.ytno = d.ytno AND m.ts <= d.dispatch_ts))
                              /nullif(count(*),0))::float8
                       FROM d
                     HAVING NOT EXISTS (SELECT 1 FROM dispatch_adoption_metric
                                         WHERE captured_at > now() - interval '55 minutes')",
                )
                .execute(&pool)
                .await;
                let _ = sqlx::query("REFRESH MATERIALIZED VIEW CONCURRENTLY learn_dispatch_lead")
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
    /// SHADOW: 이 베이가 선박 출항을 지키려면 끝나 있어야 하는 시각(QueueOut.deadline_ts).
    /// work_eta와 달리 DS +600s / 학습잔차 / 교대정지 / as_of 앵커가 전혀 안 들어간다.
    pub(crate) deadline_ts: Option<DateTime<Utc>>,
    /// SHADOW: 이 베이의 총 처리 초(QueueOut.proc_s). 완료기한 → 시작기한 환산에 쓴다.
    pub(crate) proc_s: Option<i64>,
    /// 설계 ②(mig 0120): 크레인 시작시각 − 작업유형별 트럭 준비시간 = **배차를 해야 할 시각**.
    /// 이 값이 없어서 Stage-2 는 대신 "크레인 시작 + 크레인당 상한의 절반"을 마감으로 써 왔다 —
    /// 트럭을 크레인에 흩뿌리려고 둔 상한(NEED_HORIZON_S)이 마감까지 정하고 있었다는 뜻이다.
    /// 지금은 기록 전용(판정은 종전대로). 옛 축과 나란히 비교한 뒤 전환한다.
    pub(crate) dispatch_deadline_ts: Option<DateTime<Utc>>,
    /// 위에서 실제로 뺀 준비시간(초). 학습값(learn_dispatch_lead) 우선, 없으면 LEAD_*_S 상수.
    pub(crate) dd_lead_s: Option<i64>,
    /// 이 상자가 구역 안에서 몇 번째인가(0-based). **지시 생성시각(cre_ts) 순**으로 매긴다.
    /// 검증(2026-08-04·양하 n=451 짝): 생성순=처리순 **82.5%**(우연이면 50%). 선박 내 위치는
    /// 46.3%로 동전 던지기보다 못해 기각했다. 적하는 QC 가 드랍오프라 지시가 그 순간 닫혀
    /// 스냅샷에 안 남으므로 같은 방식으로 검증할 표본이 구조적으로 없다 — 양하 결과를 원용한다.
    /// ⚠ 이 값이 없으면(집계 버킷 등) None → 종전처럼 구역 시작 시각을 그대로 쓴다.
    pub(crate) slot_idx: Option<i32>,
    /// 이 구역의 무브 하나 시간(초). 상자별 시각 = 구역 시작 + slot_idx × 이 값.
    pub(crate) move_s: Option<i64>,
    /// TOS 가 이미 트럭을 붙여 둔 작업인가(live_workpool 유래). mig 0121 1단계용.
    /// ⚠ 우리 시스템은 TOS 배차와 무관하게 돌아야 하므로 **새 규칙 풀은 이 값을 무시**한다.
    /// 현행 Stage-1 은 종전 동작 보존을 위해 false 인 것만 담는다(= 옛 live_candidate 집합).
    /// 왜 이게 필요한가: TOS 는 크레인 필요 ~25분 전에 트럭을 붙인다. 그래서 '미배차'만 보면
    /// 우리 목록에는 **항상 25분 이상 남은 작업만** 남고, 배차 마감이 임박하는 일이 영원히 없다.
    /// 실측(2026-08-03): 마감까지 남은 시간의 최소가 829초에서 잘려 0 에 닿지 않았다.
    pub(crate) tos_assigned: bool,
    /// 트윈 대표 상자 = min(contno). 채점 조인 키.
    pub(crate) contno: Option<String>,
    /// 이 트럭 몫의 상자 전부(트윈=2·단독=1·집계 버킷=0). contno 가 대표 하나뿐이라
    /// 화면의 상자별 마감 칩이 트윈 두 번째 상자에서 사라졌었다(2026-08-10 실측: 행의 24%).
    pub(crate) contnos: Vec<String>,
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

pub(crate) async fn stage2_work_candidates(
    pool: PgPool,
) -> Result<(WorkpoolOut, Vec<Stage2Work>), AppError> {
    let pool_for_lead = pool.clone();
    let wp = build_workpool(pool).await?;
    // (qc,vessel,queuename) = live_workqueue의 PK(0012:22)라 1:1 — dedup 불필요. work-ETA와 나란히
    // 출항 역산 마감(deadline_ts)·베이 처리시간(proc_s)도 같이 담는다(신규 계산 0, :465/:479에서 이미
    // 산출됨). ⚠ QcOut.slack_s는 쓰지 않는다 — QC의 '첫 선박'에만 채워지고(:482) 64개 중 37 QC가
    // 다선박이라 같은 QC의 모든 베이가 동일값이 되며, dispatch_pred_sample.slack_s와 이름이 충돌한다.
    let mut eta: HashMap<
        (String, String, String),
        (DateTime<Utc>, Option<DateTime<Utc>>, Option<i64>, Option<i64>, Option<i64>, Option<DateTime<Utc>>),
    > = HashMap::new();
    for qc in &wp.qcs {
        for q in &qc.queues {
            if let Some(e) = q.work_eta_ts {
                eta.insert(
                    (qc.qc.clone(), q.vessel.clone(), q.queuename.clone()),
                    (e, q.deadline_ts, q.proc_s, q.pace_before_n, q.pace_total_n, q.pace_finish_by),
                );
            }
        }
    }
    // pool_mode=3 마감의 시계 원점 — 매 호출(60초 틱)마다 fresh. 완료·시각 경과가 반영된
    // 재료(pace_*)와 함께 "출항까지 남은 시간 기준 균등 배분"이 계속 다시 계산된다.
    let now_ts = Utc::now();
    // 설계 ②의 "트럭 준비시간" = 작업 할당부터 QC 작업지점 도착까지. 학습값을 쓴다(mig 0116의
    // realized_lead_s = TOS 실현 선행시간 = 배차 → 크레인 핸드오버, 7일 창으로 재측정).
    // ⚠ 같은 표의 extra_s 가 아니다 — 그건 "픽업 지점 도착 **이후**" 남은 몫이라 주행이 빠져 있다.
    // 학습값이 없으면 2026-07-01 실측 상수(LEAD_*_S)로 폴백. 실측 대조: 양하 455 vs 상수 450(일치),
    // 적하 1,448 vs 상수 1,180(학습값이 23% 큼 — 상수는 5주 전 p75).
    let lead_realized: HashMap<String, i64> = sqlx::query_as::<_, (String, i32)>(
        "SELECT jobtype, realized_lead_s FROM learn_dispatch_lead",
    )
    .fetch_all(&pool_for_lead).await.unwrap_or_default()
    .into_iter().map(|(j, v)| (j, (v as i64).clamp(60, 3600))).collect();
    // ⚠ 아래 live_candidate(집계) 경로는 **상자 단위 경로가 못 덮는 것만** 담는다.
    // live_workpool 은 배차됨(A)·미배차(Q) 를 모두 상자 단위로 갖고 있어 live_candidate 의
    // 상위집합이다. 둘 다 담으면 미배차 상자가 이중 계상된다. 상자 단위가 있으면 그쪽이 우선이고
    // (마감을 상자마다 매길 수 있으므로), 여기서는 상자 단위에 없는 (크레인,배,구역)만 보충한다.
    let boxed_keys: std::collections::HashSet<(String, String, String)> = sqlx::query_as::<_, (String, String, String)>(
        "SELECT DISTINCT qc, vessel, queuename FROM live_workpool
          WHERE jobtype IN ('DS','LD') AND qc IS NOT NULL AND qc <> '' AND contno IS NOT NULL AND contno <> ''",
    )
    .fetch_all(&pool_for_lead).await.unwrap_or_default().into_iter().collect();

    let mut out = Vec::new();
    for c in &wp.candidates {
        let Some(qc) = c.qc.clone().filter(|s| !s.is_empty()) else { continue };
        let jt = c.jobtype.clone().unwrap_or_default();
        if boxed_keys.contains(&(qc.clone(), c.vessel.clone(), c.queuename.clone())) {
            continue;   // 상자 단위로 이미 담았다 — 이중 계상 방지
        }
        let lead = lead_realized.get(&jt).copied()
            .unwrap_or(if jt == "LD" { LEAD_LD_S } else { LEAD_DS_S });
        let row = eta.get(&(qc.clone(), c.vessel.clone(), c.queuename.clone())).copied();
        // 균등 페이스 (pool_mode=3): 첫 상자 마감 = now + (앞 무브 수 × 무브당 배정 시간).
        // 목표 없으면 종전 전방 예측(work_eta) 폴백. 이 집계 경로는 상자 단위가 못 덮는 구역만.
        let first_due = row.and_then(|r| r.5).map(|fb| {
            let total_n = row.and_then(|r| r.4).unwrap_or(0).max(1);
            let before_n = row.and_then(|r| r.3).unwrap_or(0).max(0);
            let pace = ((fb - now_ts).num_seconds() / total_n).max(1);
            now_ts + chrono::Duration::seconds(before_n * pace)
        }).or(row.map(|r| r.0));
        out.push(Stage2Work {
            qc,
            vessel: c.vessel.clone(),
            queuename: c.queuename.clone(),
            jobtype: jt,
            src_block: c.src_block.clone(),
            n: c.n,
            work_eta_ts: row.map(|r| r.0),
            deadline_ts: row.and_then(|r| r.1),
            proc_s: row.and_then(|r| r.2),
            dispatch_deadline_ts: first_due.map(|e| e - chrono::Duration::seconds(lead)),
            dd_lead_s: Some(lead),
            slot_idx: None,   // live_candidate 는 집계 버킷이라 상자 단위가 아니다
            move_s: None,
            tos_assigned: false,
            contno: None,
            contnos: Vec::new(),
        });
    }
    // ── TOS 가 이미 배차한 작업도 담는다 (mig 0121) ──────────────────────────────────────────
    // 우리 시스템은 TOS 배차와 무관하게 "지금 배차해야 할 것"을 계산한다. 그런데 지금까지 작업
    // 소스가 live_candidate(= TOS 미배차)뿐이라, TOS 가 ~25분 전에 가져간 작업은 우리 눈에
    // 들어오기 전에 사라졌다. 그래서 배차 마감이 임박하는 작업이 구조적으로 존재하지 않았다.
    //
    // 추가 추출은 없다 — 같은 Oracle 질의가 A(배차됨)·Q(미배차)를 이미 둘 다 가져와
    // live_workpool / live_candidate 로 나눠 담고 있었고, 우리는 Q 만 쓰고 A 를 버리고 있었다.
    //
    // 트럭 수요 환산: 배차된 행은 ytno(트럭)가 있으므로 **트럭 대수 = distinct ytno** 가 정확하다
    // (live_candidate 쪽이 twinkey 로 트윈을 합치는 것과 같은 의미). 야드 블록은 미배차 쪽과
    // 똑같이 yt_topos 앞자리에서 얻는다(실측 채워짐 적하 99.6% / 양하 100%).
    // 무브 하나 시간 — **구역 시작 시각을 계산할 때 쓰는 것과 같은 학습값**을 쓴다(일관성).
    // 없으면 실측 중앙값으로 폴백(2026-08-04 연속 comp 간격: 양하 99초 / 적하 126초).
    const BOX_MOVE_DS_S: i64 = 99;
    const BOX_MOVE_LD_S: i64 = 126;
    let mut move_time: HashMap<(String, char), i64> = sqlx::query_as::<_, (String, String, Option<i32>)>(
        "SELECT qc, jobtype, med_sec FROM learn_qc_move_time WHERE med_sec IS NOT NULL",
    )
    .fetch_all(&pool_for_lead).await.unwrap_or_default()
    .into_iter()
    .filter_map(|(q, jt, m)| m.map(|m| ((q, if jt == "LD" { 'L' } else { 'D' }), (m as i64).clamp(30, 600))))
    .collect();
    // 벽시계 리듬 오버레이 — build_workpool 과 동일한 원천/우선순위 (mig 0131).
    let wall_rows: Vec<(String, String, Option<i32>)> = sqlx::query_as(
        "SELECT qc, jobtype, wall_s FROM learn_qc_wall_cadence WHERE wall_s IS NOT NULL")
        .fetch_all(&pool_for_lead).await.unwrap_or_default();
    for (qc, jt, wall_s) in &wall_rows {
        if let Some(wall_s) = wall_s {
            move_time.insert((qc.clone(), if jt == "LD" { 'L' } else { 'D' }), *wall_s as i64);
        }
    }
    // 순번당 걸음 (mig 0139) — 상자별 예측 전용. 벽시계 리듬(무브당)이 아니라 "잔여 순번
    // 하나당 실측 경과"의 중앙값이다: 크레인이 계획 순서를 그대로 따르지 않아 순번당 진행은
    // 무브당 리듬보다 빠르다(실측 DS 117 vs 126 / LD 133 vs 183초 — 그 차이 × 순번이 뒤
    // 순번의 늦은 예측으로 쌓였다, pred_ver 2→3 경계). 구역 시작(before) 누적은 계속 벽시계
    // 리듬을 쓴다 — 거기는 '크레인이 소화할 총 무브량'이라 무브당이 맞다.
    let mut slot_step: HashMap<(String, char), i64> = HashMap::new();
    let step_rows: Vec<(String, String, Option<i32>)> = sqlx::query_as(
        "SELECT qc, jobtype, step_s FROM learn_qc_slot_step WHERE step_s IS NOT NULL")
        .fetch_all(&pool_for_lead).await.unwrap_or_default();
    for (qc, jt, st) in &step_rows {
        if let Some(st) = st {
            slot_step.insert((qc.clone(), if jt == "LD" { 'L' } else { 'D' }), (*st as i64).clamp(30, 600));
        }
    }

    // 상자 단위로 가져온다 — 구역 하나에 한 줄이 아니라 **상자 하나에 한 줄**.
    // 그래야 "구역 j번째 상자가 언제 처리되나"를 각각 계산할 수 있다.
    //
    // 구역 안 순서(slot_idx)는 **적부계획의 상자 순번**(live_stow_plan.planseq)으로 매긴다
    // (mig 0128·0129). TOS 의 ITV 배차기도 이것으로 정렬한다 —
    //   RANK() OVER (PARTITION BY 크레인 ORDER BY JOB_QUE_PLND_DATE||TIME, VSP_SHP_PLANSEQ)
    //   (com.clt.tos.itv.supervisor-impl/.../LoadableJob.xml — 양하 444·499, 적하 757)
    //
    // ★이 배선의 핵심은 순서가 아니라 **분모**다. 우리 작업목록은 구역에 남은 일의 44% 만
    //   담는데(TOS 가 지시를 작업 직전에 다 만들지 않는다) 순번을 그 안에서 0부터 다시 매기고
    //   있었다. 그래서 어느 상자든 실제보다 앞자리를 받아, 작업 도달 예측이 **예측 거리와
    //   무관하게 일정하게** 일렀다(양하 +16~26분 · 적하 +37~39분). 계획은 남은 일의 99% 를
    //   덮으므로(실측: 계획 4,169 vs 큐 카운터 4,173) 그 안에서 세면 분모가 맞는다.
    //
    // ⚠ 계획은 **상자** 단위, 무브시간은 **들어올림** 단위다(learn_qc_move_time = 연속 완료
    //   간격의 중앙값). 트윈은 상자 2개에 들어올림 1회이므로 (1 − 트윈/2) 로 환산한다 —
    //   구역 단위 계산이 쓰는 것과 같은 식이다.
    //
    // ⚠ 작업지시 표에는 순서가 없다. 여기서 헤매지 말 것:
    //   · `msnseq` : 660/660 전부 비어 있다. "TOS 에 순번이 없다"는 오판이 여기서 나왔다
    //                (같은 표에 컬럼 92개인데 우리가 뽑는 것은 20개뿐이었다).
    //   · `seqno`  : 열린 지시에서는 **발행 시각**(cre_ts 와 ±18초·순서 97.6% 동일), 완료되면
    //                **완료 시각**으로 덮어쓰인다. 끝난 작업으로 채점하면 100% 로 보이는
    //                함정이다(st_ts → ETW → SEQNO, 같은 함정 세 번째). 폴백으로만 남긴다.
    //   · `point`  : 순서가 아니다(단독 49% = 무작위, 더해도 62.8→63.2%).
    //
    // 사전 검정(계획을 작업 **전에** 떠서 채점): 순서 78.6% vs 위약 47.2%(n=1,631),
    // "앞에 몇 개" 치우침 +0.8자리(≈+1.5분·n=221). 종전 축은 같은 기간 적하 +37분이었다.
    //
    // ⚠ 트윈은 상자 2개·트럭 1대다. 여기서는 트럭 대수가 아니라 **크레인 무브 순서**를 매기는
    //   것이므로 상자 단위가 맞다(크레인이 트윈을 한 번에 들면 두 상자가 같은 시각이 되는데,
    //   그 오차는 무브 하나(약 100초)라 감수한다).
    // ⚠ 양하의 배차된 지시 중 **이미 크레인을 지난 것**은 제외한다. 양하는 QC 가 픽업이라
    //   크레인이 내리는 순간 지시가 아니라 구역 카운터가 오르고, 지시는 트럭이 야드에
    //   드랍오프해야 닫힌다. 그 사이(중앙 11분) 상자는 "크레인은 끝났는데 지시는 열린" 상태다.
    //   실측: 양하 배차분 196건이 100% 이 상태였고, 빼지 않으면 구역별 산술이 9곳에서 깨졌다.
    // 트윈은 상자 2개·트럭 1대다. **트럭 한 대 몫**을 한 줄로 만들기 위해 twinkey 로 먼저 합친다
    // (mig 0124). twinkey 가 빈 행은 그 자체가 한 대 몫이다.
    let per_box: Vec<(String, String, String, String, Option<String>, String, i64, bool, Vec<String>)> = sqlx::query_as(
        "WITH loads AS (                            -- 트럭 한 대 몫 = 트윈 쌍 하나 또는 단독 상자 하나
           SELECT w.qc, w.vessel, w.voyage, w.queuename, w.jobtype,
                  min(CASE WHEN w.jobtype = 'LD' THEN NULLIF(left(w.yt_topos, 3), '') END) AS src_block,
                  min(w.seqno)   AS seqno,
                  min(w.cre_ts)  AS cre_ts,
                  min(w.contno)  AS contno,
                  array_agg(DISTINCT w.contno) AS contnos,   -- 트윈이면 두 상자 모두 (화면 조회용)
                  bool_or(w.ytno IS NOT NULL AND w.ytno <> '') AS tos_assigned
             FROM live_workpool w
            WHERE w.jobtype IN ('DS','LD') AND w.qc IS NOT NULL AND w.qc <> ''
              AND w.contno IS NOT NULL AND w.contno <> ''
              AND NOT EXISTS (                     -- 크레인이 이미 처리한 양하 지시 제외
                SELECT 1 FROM qc_move_log m
                 WHERE m.contno = w.contno AND m.jobtype = w.jobtype
                   AND m.comp_ts > now() - interval '12 hours')
            GROUP BY w.qc, w.vessel, w.voyage, w.queuename, w.jobtype,
                     COALESCE(NULLIF(w.twinkey, ''), w.contno)   -- 트윈이면 한 줄로
         ),
         pp AS (        -- 계획에서의 자리: 같은 구역에서 planseq 가 더 작은 상자가 몇 개인가.
                        -- 우리 목록이 아니라 **계획 전체**를 세는 것이 이 배선의 핵심이다.
                        -- ⚠ live_stow_plan 은 '남은 상자만의 거울'이다(완료 상자는 즉시 사라짐,
                        -- 2026-08-10 실측) — 그래서 pos 는 절대 순번이 아니라 잔여 순번이다.
           SELECT vessel, voyage, queuename, contno,
                  (row_number() OVER (PARTITION BY vessel, voyage, queuename
                                      ORDER BY planseq NULLS LAST, contno) - 1) AS pos
             FROM live_stow_plan
         ),
         tw AS (        -- 선박별 트윈 비율: 계획은 **상자** 단위인데 무브시간은 **들어올림** 단위라
                        -- 환산이 필요하다. 구역 단위 계산이 쓰는 것과 같은 식(1 − 트윈/2)이다.
           SELECT vessel, avg(CASE WHEN NULLIF(twinkey,'') IS NOT NULL THEN 1.0 ELSE 0.0 END)::float8 AS f
             FROM live_workpool GROUP BY vessel
         )
         SELECT l.qc, l.vessel, l.queuename, l.jobtype, l.src_block, l.contno,
                COALESCE(
                  floor(pp.pos * (1.0 - COALESCE(tw.f, 0.0) / 2.0))::int8,
                  -- 계획에 없는 상자만 종전 축으로 떨어진다(작업지시 발행 순서).
                  (row_number() OVER (PARTITION BY l.qc, l.vessel, l.queuename
                                      ORDER BY NULLIF(l.seqno,'') NULLS LAST,
                                               l.cre_ts NULLS LAST, l.contno) - 1)::int8
                ) AS slot_idx,
                l.tos_assigned, l.contnos
           FROM loads l
           LEFT JOIN pp ON pp.vessel = l.vessel AND pp.voyage = l.voyage
                       AND pp.queuename = l.queuename AND pp.contno = l.contno
           LEFT JOIN tw ON tw.vessel = l.vessel",
    )
    .fetch_all(&pool_for_lead).await.unwrap_or_default();
    for (qc, vessel, queuename, jt, src_block, contno, slot_idx, tos_assigned, contnos) in per_box {
        let lead = lead_realized.get(&jt).copied()
            .unwrap_or(if jt == "LD" { LEAD_LD_S } else { LEAD_DS_S });
        // 걸음 사슬: 크레인별 학습 → 전역('*') 학습 → 벽시계/활동 리듬 → 상수 (mig 0139)
        let jc = if jt == "LD" { 'L' } else { 'D' };
        let move_s = slot_step.get(&(qc.clone(), jc)).copied()
            .or_else(|| slot_step.get(&("*".to_string(), jc)).copied())
            .or_else(|| move_time.get(&(qc.clone(), jc)).copied())
            .unwrap_or(if jt == "LD" { BOX_MOVE_LD_S } else { BOX_MOVE_DS_S });
        let row = eta.get(&(qc.clone(), vessel.clone(), queuename.clone())).copied();
        // 상자별 시각 = 구역 시작 + slot_idx × 순번당 걸음
        let box_eta = row.map(|r| r.0 + chrono::Duration::seconds(slot_idx * move_s));
        // ── 배차 마감 = 출항 요구 페이스 균등 배분 (2026-08-10 재정의 · pool_mode=3) ────
        // 무브가 실제 몇 초 걸릴지는 스케줄에 넣지 않는다(그건 급함의 잣대일 뿐).
        //   무브당 배정 시간 = (출항 목표 − 지금) ÷ (선박 마지막 베이까지 남은 무브 수)
        //   j번째 무브 시작 시각 = now + j × 배정 시간,  j = 앞 무브 수 + 구역 안 순번
        // 시계가 흐르고 QC 완료가 쌓일 때마다(반영 ≤ ~2분) 이 나눗셈이 다시 되므로 마감은
        // 항상 현재 기준이다 — 크레인이 밀리면 저절로 급해지고, 빠르면 느슨해진다.
        // 모든 배의 첫 무브는 항상 '지금' 마감이라 활발한 구역이 풀에서 사라지지 않는다.
        // 늦은 배(목표 ≤ now)는 페이스 1초 바닥 → 전부 지금 마감·순번 순서 유지.
        // 출항 목표가 없는 배(스케줄 미상·가상선박)는 종전 전방 예측 마감으로 폴백.
        let box_due = row.and_then(|r| r.5).map(|fb| {
            let total_n = row.and_then(|r| r.4).unwrap_or(0).max(1);
            let before_n = row.and_then(|r| r.3).unwrap_or(0).max(0);
            let pace = ((fb - now_ts).num_seconds() / total_n).max(1);
            now_ts + chrono::Duration::seconds((before_n + slot_idx) * pace)
        }).or(box_eta);
        out.push(Stage2Work {
            qc,
            vessel,
            queuename,
            jobtype: jt,
            src_block,
            n: 1,                                   // 상자 하나 = 트럭 한 대 몫
            work_eta_ts: box_eta,
            deadline_ts: row.and_then(|r| r.1),
            proc_s: row.and_then(|r| r.2),
            dispatch_deadline_ts: box_due.map(|e| e - chrono::Duration::seconds(lead)),
            dd_lead_s: Some(lead),
            slot_idx: Some(slot_idx as i32),
            move_s: Some(move_s),
            tos_assigned,   // ⚠ 행마다 정확히 — live_workpool 은 배차됨(A)·미배차(Q) 를 둘 다 담는다
            contno: Some(contno),
            contnos,
        });
    }
    Ok((wp, out))
}

// ── Stage-2 shadow validation dashboard feed ─────────────────────────────────────────────────
#[derive(Serialize, sqlx::FromRow)]
struct S2Summary {
    matches_30m: i64,
    switched_pct: Option<f64>,
    feasible_pct: Option<f64>,
    /// mig 0116 크레인 기준 축 — feasible_pct(옛 축, 적재 구간 누락)를 화면에서 대체한다.
    feasible_crane_pct: Option<f64>,
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
    // ⚠ 30분 창은 반드시 pool_mode=3(현행 모집단)으로 가른다 — 마감 정의 전환(mig 0140·0141)
    // 직후 창이 경계를 걸치면 옛 모집단과 섞인 수치가 나간다(2026-08-10 P2 정비).
    let summary: S2Summary = sqlx::query_as(
        "SELECT count(*) AS matches_30m,
                (100.0*count(*) FILTER (WHERE switched)/nullif(count(*),0))::float8 AS switched_pct,
                (100.0*count(*) FILTER (WHERE feasible)/nullif(count(*),0))::float8 AS feasible_pct,
                (100.0*count(*) FILTER (WHERE feasible_crane)
                   /nullif(count(*) FILTER (WHERE feasible_crane IS NOT NULL),0))::float8 AS feasible_crane_pct,
                (100.0*count(*) FILTER (WHERE cost_tier='R')/nullif(count(*),0))::float8 AS routed_pct,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY arrival_s))::float8 AS median_arrival_s,
                count(DISTINCT ytno) AS vehicles,
                count(DISTINCT (qc, queuename, vessel)) AS works
           FROM stage2_match_shadow m WHERE m.ts > now() - interval '30 minutes'
            AND m.ts IN (SELECT ts FROM stage2_solver_shadow
                          WHERE ts > now() - interval '30 minutes' AND pool_mode = 3)",
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
           FROM stage2_solver_shadow WHERE ts > now() - interval '30 minutes' AND pool_mode = 3",
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
    /// 어느 상자에 보내는 추천인지 — 화면이 행 단위로 정확히 귀속하게 한다(종전엔 QC+방향
    /// 첫-적합 근사였다). ts 는 신선도 표시용(매처 정지 시 낡은 추천 흐리게).
    queuename: Option<String>,
    contno: Option<String>,
    ts: DateTime<Utc>,
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
        "SELECT ytno, qc, queuename, contno, ts, jobtype, src_block, dest_lat, dest_lon, src_lat, src_lon, arrival_s, feasible
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
    /// ⚠ 옛 축. 적하는 트럭이 **야드 블록**에 닿는 시간만 세고 마감은 **안벽 QC** 시각이라,
    /// 그 사이 적재 구간(~1,014초)이 빠져 있다. 정의 불변으로 보존하는 값이니 그대로 읽지 말 것.
    feasible_pct: Option<f64>,
    /// mig 0116 — 크레인 기준 축(= 위에 learn_dispatch_lead.extra_s 를 더해 판정). 현장 대조용은 이것.
    feasible_crane_pct: Option<f64>,
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
    // 임계 180초 = 하트비트(150초) + 틱 본체 여유. 2026-08-12 이전에는 매칭이 고정 60초 주기라
    // 120초(2회 결손)가 맞았으나, 지금은 **작업목록 착지마다** 돌고 착지가 없으면 150초 하트비트가
    // 받는다. 120초로 두면 하트비트가 뜰 때마다 정상인데 "죽었다"고 표시된다(리뷰 지적).
    // 진짜 총정지는 stage2_match_shadow DEADMAN(30분)이 별도로 잡는다.
    let up = last_tick_age_s.map(|a| a < 180).unwrap_or(false);

    let (thrash_pct, feasible_pct, feasible_crane_pct, routed_pct, arr_p50_s, arr_p90_s): (
        Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>,
    ) = sqlx::query_as(
        "SELECT (100.0*count(*) FILTER (WHERE switched)/nullif(count(*),0))::float8,
                (100.0*count(*) FILTER (WHERE feasible)/nullif(count(*),0))::float8,
                (100.0*count(*) FILTER (WHERE feasible_crane)
                   /nullif(count(*) FILTER (WHERE feasible_crane IS NOT NULL),0))::float8,
                (100.0*count(*) FILTER (WHERE cost_tier='R')/nullif(count(*),0))::float8,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY arrival_s))::float8,
                (percentile_cont(0.9) WITHIN GROUP (ORDER BY arrival_s))::float8
           FROM stage2_match_shadow WHERE ts > now() - interval '30 minutes'
            AND ts IN (SELECT ts FROM stage2_solver_shadow
                        WHERE ts > now() - interval '30 minutes' AND pool_mode = 3)",
    )
    .fetch_one(&pool)
    .await?;

    let savings_pct: Option<f64> = sqlx::query_scalar(
        "SELECT (100.0*sum(greedy_cost_s - optimal_cost_s)/nullif(sum(greedy_cost_s),0))::float8
           FROM stage2_solver_shadow WHERE ts > now() - interval '30 minutes' AND pool_mode = 3",
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
              AND ts IN (SELECT ts FROM stage2_solver_shadow
                          WHERE ts > now() - interval '1 hour' AND pool_mode = 3)
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
            AND ts IN (SELECT ts FROM stage2_solver_shadow
                        WHERE ts > now() - interval '24 hours' AND pool_mode = 3)
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
        thrash_pct, feasible_pct, feasible_crane_pct, savings_pct, routed_pct, arr_p50_s, arr_p90_s,
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
    // 창 2일 → 3시간 (2026-08-10). 화면은 **지금 배정된** 행에만 이 값을 붙이는데(배정은
    // ~1시간 안에 회전) 2일 창은 표가 쌓일수록 응답이 자랐다 — 실측 30,411행/4.2MB를 15초마다
    // 폴링 + 탭이 매 렌더마다 그 Map 을 재구축해 브라우저가 죽었다. 3시간이면 회전 주기의
    // 3배 여유. dispatch_compare_ts (ts) 인덱스가 있어 창 축소는 스캔이 아니라 인덱스를 탄다.
    let rows = sqlx::query_as::<_, ComparePick>(
        "SELECT DISTINCT ON (qc, queuename, tos_ytno)
                qc, queuename, tos_ytno, our_ytno, our_arrival_s, tos_arrival_s, agree, delta_s
           FROM dispatch_compare_shadow
          WHERE ts > now() - interval '3 hours' AND t1_ver = 1  -- mig 0149/0152
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
            WHERE ts > now() - interval '60 minutes' AND t1_ver = 1  -- mig 0149/0152
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
    /// ⚠ NOT a saving. The identity permutation (what TOS actually did) is always a feasible
    /// solution, so the min-cost matching can never cost more — this can never be negative
    /// (measured 2026-07-31: 0 negatives in 1,924 ticks, min +0.027%). Read it as "how far TOS's
    /// assignment sits from the optimum of the same pool" = the CEILING on improvement, not a
    /// realized gain.
    savings_pct: f64,
    same_n: i32,
    /// Cost of assigning the same pool at random (8 shuffles averaged). NULL for rows written
    /// before mig 0110. This is the third point that makes the other two interpretable.
    rand_total_s: Option<i64>,
}
#[derive(Serialize)]
pub struct FairCompareOut {
    latest: Option<FairCompare>,
    /// pair-weighted over the returned window: 100·(Σtos−Σour)/Σtos — see the warning on
    /// `savings_pct`. (Was a plain per-tick mean before 2026-08-10, which let a 4-pair tick
    /// weigh as much as a 120-pair one.)
    avg_savings_pct: Option<f64>,
    /// Σn over the returned window — the pair count that matches `avg_savings_pct`'s window,
    /// so the headline can quote one consistent denominator instead of the latest tick's n.
    pairs_total: i64,
    /// window-consistent same-pick share: 100·Σsame_n/Σn
    same_pct: Option<f64>,
    /// ★ the honest one: of the range a coin-flip assignment leaves open (random − optimal), what
    /// share does TOS already capture? 100% = TOS is already optimal and there is nothing to win;
    /// low = real headroom. None until enough rows carry the random baseline.
    avg_tos_capture_pct: Option<f64>,
    /// how many of the returned rows have the random baseline (0 right after deploy)
    rand_n: usize,
    recent: Vec<FairCompare>,
}

/// `GET /api/stage2/fair-compare` — the FAIR head-to-head: our solver's optimal 1:1 matching vs TOS's
/// actual matching on the SAME trucks+works+positions (reservation-respected). The honest efficiency
/// number, unlike the per-work "closest truck" metric which double-books the nearest truck.
pub async fn stage2_fair_compare(State(pool): State<PgPool>) -> Result<Json<FairCompareOut>, AppError> {
    let recent: Vec<FairCompare> = sqlx::query_as(
        "SELECT ts, window_min, n, tos_total_s, our_total_s, savings_pct, same_n, rand_total_s
           FROM fair_compare_shadow ORDER BY ts DESC LIMIT 48",
    )
    .fetch_all(&pool)
    .await?;
    let latest = recent.first().cloned();
    // pair-weighted, one window: totals over the same 48 rows the page quotes
    let tot_tos: i64 = recent.iter().map(|r| r.tos_total_s).sum();
    let tot_our: i64 = recent.iter().map(|r| r.our_total_s).sum();
    let pairs_total: i64 = recent.iter().map(|r| r.n as i64).sum();
    let same_total: i64 = recent.iter().map(|r| r.same_n as i64).sum();
    let avg_savings_pct = (tot_tos > 0).then(|| 100.0 * (tot_tos - tot_our) as f64 / tot_tos as f64);
    let same_pct = (pairs_total > 0).then(|| 100.0 * same_total as f64 / pairs_total as f64);
    // random >= TOS >= optimal, so (random-TOS)/(random-optimal) is the share of the achievable
    // range TOS already holds. Skip rows where the denominator is degenerate (random == optimal
    // means every permutation costs the same and there was nothing to decide).
    let caps: Vec<f64> = recent
        .iter()
        .filter_map(|r| {
            let rand = r.rand_total_s?;
            let span = rand - r.our_total_s;
            (span > 0).then(|| 100.0 * (rand - r.tos_total_s) as f64 / span as f64)
        })
        .collect();
    let avg_tos_capture_pct = (!caps.is_empty()).then(|| caps.iter().sum::<f64>() / caps.len() as f64);
    let rand_n = recent.iter().filter(|r| r.rand_total_s.is_some()).count();
    Ok(Json(FairCompareOut { latest, avg_savings_pct, pairs_total, same_pct, avg_tos_capture_pct, rand_n, recent }))
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
    // bucket by the DISPATCH time, not the 5-min loader's write time (up to 5 min late and
    // wrong at hour boundaries). dispatch_ts exists since mig 0110; COALESCE covers older rows.
    let by_hour: Vec<FairBucket> = sqlx::query_as(&format!(
        "SELECT lpad((extract(hour FROM COALESCE(dispatch_ts, ts) AT TIME ZONE 'Asia/Kuala_Lumpur'))::int::text,2,'0') AS key, {COLS}
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
                (100.0*count(*) FILTER (WHERE delta_s > 0)
                      /nullif(count(*) FILTER (WHERE delta_s IS NOT NULL),0))::float8 AS ours_faster_pct,
                avg(delta_s)::float8 AS avg_delta_s,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY delta_s))::float8 AS median_delta_s,
                avg(our_arrival_s)::float8 AS avg_our_arrival_s,
                avg(tos_arrival_s)::float8 AS avg_tos_arrival_s,
                count(*) FILTER (WHERE reason='same') AS same_n,
                count(*) FILTER (WHERE reason='ours_closer') AS ours_closer_n,
                count(*) FILTER (WHERE reason='tos_closer') AS tos_closer_n
           FROM dispatch_compare_shadow WHERE ts > now() - interval '24 hours' AND reason <> 'now'
                  -- t1_ver 로 가른다(mig 0149/0152): NULL 구간은 T1 이 upd_ts 라 절반가량이
                  -- 실제 배차가 아닌 순간으로 되감겨 있다. 판별자를 달아놓고 안 거르면 계약이 아니다.
                  AND t1_ver = 1",
    )
    .fetch_one(&pool)
    .await?;
    let recent: Vec<CompareRow> = sqlx::query_as(
        "SELECT ts, qc, queuename, jobtype, tos_ytno, tos_arrival_s, our_ytno, our_arrival_s, agree, reason, delta_s
           FROM dispatch_compare_shadow WHERE ts > now() - interval '24 hours' AND NOT agree AND reason <> 'now'
            AND t1_ver = 1   -- mig 0149/0152
          ORDER BY ts DESC LIMIT 25",
    )
    .fetch_all(&pool)
    .await?;
    Ok(Json(DispatchCompareOut { summary, recent }))
}

// ── 상자 단위 시스템 비교 — VS TOS 헤드라인 (2026-08-10) ─────────────────────────────────────
// 같은 상자(contno)에 대해 우리 추천(트럭·시점)과 TOS 실배차(트럭·시점)를 직접 짝짓는다.
// 시점 격차는 정오 판정이 아니라 계기다: 우리 마감은 출항 요구 페이스 규범(pool_mode=3)이라
// TOS 배차시각과 다르다는 사실만으로 어느 쪽이 틀렸다고 말할 수 없다. 분모 2개를 응답에
// 그대로 싣는다(boxes_reco → boxes_joined). contno 는 mig 0142(2026-08-10)부터 기록되므로
// 그 이전 추천은 분모에 들어오지 않는다 — 표본이 차오르는 중이라는 라벨이 프론트에 있다.

#[derive(Serialize, sqlx::FromRow)]
struct BoxJobStat {
    jobtype: Option<String>,
    n: i64,
    /// TOS 배차시각 − 우리 최초 추천시각 (초; + = TOS 가 우리보다 늦게 배차)
    gap_p25_s: Option<f64>,
    gap_p50_s: Option<f64>,
    gap_p75_s: Option<f64>,
    /// 우리 마감선 − TOS 배차시각 (초; + = 우리 마감 안쪽 배차)
    margin_p50_s: Option<f64>,
    truck_match_pct: Option<f64>,
}
#[derive(Serialize, sqlx::FromRow)]
struct BoxTiming {
    jobtype: Option<String>,
    n: i64,
    /// TOS 실현: 배차 → QC 처리완료 (DS=pickup_ts, LD=free_ts — 트럭 준비시간의 실측판)
    realized_lead_p50_s: Option<f64>,
    realized_lead_p90_s: Option<f64>,
    /// 모형 무부하주행 p50 (learn_dispatch_lead.modeled_arrival_s) — 물리적으로 꼭 드는 몫
    modeled_travel_s: Option<i32>,
    /// 학습된 실현 리드 — ⚠ TOS 실현치에서 학습되므로 이것으로 TOS 를 채점하면 동어반복.
    /// 눈금(참고)으로만 내보낸다.
    learned_lead_s: Option<i32>,
}
#[derive(Serialize, sqlx::FromRow)]
struct BoxRow {
    contno: String,
    qc: Option<String>,
    jobtype: Option<String>,
    first_ts: DateTime<Utc>,
    our_ytno: Option<String>,
    tos_ytno: Option<String>,
    dispatch_ts: Option<DateTime<Utc>>,
    gap_s: Option<f64>,
    margin_s: Option<f64>,
    truck_match: Option<bool>,
}
#[derive(Serialize)]
pub struct BoxCompareOut {
    /// 분모①: 최근 24h 우리가 추천한 상자 수 (contno 단위·최초 추천 기준)
    boxes_reco: i64,
    /// 분모②: 그중 TOS 도 최초 추천 앞뒤 3시간 안에 실배차해 짝지어진 상자 수
    boxes_joined: i64,
    truck_match_pct: Option<f64>,
    /// TOS 배차가 우리 최초 추천 이후였던 비율
    tos_after_pct: Option<f64>,
    gap_p25_s: Option<f64>,
    gap_p50_s: Option<f64>,
    gap_p75_s: Option<f64>,
    /// TOS 배차가 우리 마감선 안쪽(margin ≥ 0)이었던 비율
    margin_in_pct: Option<f64>,
    by_job: Vec<BoxJobStat>,
    timing: Vec<BoxTiming>,
    recent: Vec<BoxRow>,
}

/// 공용 CTE: 우리 추천(상자별 최초 행) ⨝ TOS 실배차(±3h 최근접). 프로브는
/// stage2_match_shadow PK(ts) 범위 + tt_move_log(contno) 인덱스(mig 0092 계열)로 간다.
const BOX_JOIN_CTE: &str = "WITH r AS (
       SELECT contno,
              min(ts) AS first_ts,
              (array_agg(ytno    ORDER BY ts))[1] AS first_ytno,
              (array_agg(qc      ORDER BY ts))[1] AS qc,
              (array_agg(jobtype ORDER BY ts))[1] AS jobtype,
              (array_agg(dispatch_deadline_ts ORDER BY ts))[1] AS first_deadline
         FROM stage2_match_shadow
        WHERE ts > now() - interval '24 hours' AND contno IS NOT NULL
        GROUP BY contno
     ), j AS (
       SELECT r.contno, r.qc, r.jobtype, r.first_ts, r.first_ytno, r.first_deadline,
              t.ytno AS tos_ytno, t.dispatch_ts,
              EXTRACT(EPOCH FROM (t.dispatch_ts - r.first_ts))::float8       AS gap_s,
              EXTRACT(EPOCH FROM (r.first_deadline - t.dispatch_ts))::float8 AS margin_s,
              EXISTS (SELECT 1 FROM stage2_match_shadow m
                       WHERE m.contno = r.contno AND m.ytno = t.ytno AND m.ts <= t.dispatch_ts) AS truck_match
         FROM r
         JOIN LATERAL (
           SELECT ytno, dispatch_ts FROM tt_move_log t
            WHERE t.contno = r.contno
              AND t.dispatch_ts >= r.first_ts - interval '3 hours'
              AND t.dispatch_ts <  r.first_ts + interval '3 hours'
            ORDER BY abs(EXTRACT(EPOCH FROM (t.dispatch_ts - r.first_ts))) LIMIT 1
         ) t ON true
     )";

/// `GET /api/stage2/box-compare`
pub async fn stage2_box_compare(State(pool): State<PgPool>) -> Result<Json<BoxCompareOut>, AppError> {
    let (boxes_reco, boxes_joined, truck_match_pct, tos_after_pct, gap_p25_s, gap_p50_s, gap_p75_s, margin_in_pct): (
        i64, i64, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>,
    ) = sqlx::query_as(&format!(
        "{BOX_JOIN_CTE}
         SELECT (SELECT count(*) FROM r)::int8,
                count(*)::int8,
                (100.0*count(*) FILTER (WHERE truck_match)/nullif(count(*),0))::float8,
                (100.0*count(*) FILTER (WHERE gap_s >= 0)/nullif(count(*),0))::float8,
                (percentile_cont(0.25) WITHIN GROUP (ORDER BY gap_s))::float8,
                (percentile_cont(0.5)  WITHIN GROUP (ORDER BY gap_s))::float8,
                (percentile_cont(0.75) WITHIN GROUP (ORDER BY gap_s))::float8,
                (100.0*count(*) FILTER (WHERE margin_s >= 0)/nullif(count(*),0))::float8
           FROM j"
    ))
    .fetch_one(&pool)
    .await?;
    let by_job: Vec<BoxJobStat> = sqlx::query_as(&format!(
        "{BOX_JOIN_CTE}
         SELECT jobtype, count(*)::int8 AS n,
                (percentile_cont(0.25) WITHIN GROUP (ORDER BY gap_s))::float8 AS gap_p25_s,
                (percentile_cont(0.5)  WITHIN GROUP (ORDER BY gap_s))::float8 AS gap_p50_s,
                (percentile_cont(0.75) WITHIN GROUP (ORDER BY gap_s))::float8 AS gap_p75_s,
                (percentile_cont(0.5)  WITHIN GROUP (ORDER BY margin_s))::float8 AS margin_p50_s,
                (100.0*count(*) FILTER (WHERE truck_match)/nullif(count(*),0))::float8 AS truck_match_pct
           FROM j WHERE jobtype IN ('DS','LD') GROUP BY jobtype ORDER BY jobtype"
    ))
    .fetch_all(&pool)
    .await?;
    let recent: Vec<BoxRow> = sqlx::query_as(&format!(
        "{BOX_JOIN_CTE}
         SELECT contno, qc, jobtype, first_ts, first_ytno AS our_ytno,
                tos_ytno, dispatch_ts, gap_s, margin_s, truck_match
           FROM j ORDER BY dispatch_ts DESC LIMIT 20"
    ))
    .fetch_all(&pool)
    .await?;
    // 시점 실측 — 상자 조인과 무관하게 TOS 전체(최근 24h 완결 무브)로 잰다. business_date
    // 인덱스 선두 컬럼으로 범위를 좁힌 뒤 dispatch_ts 필터(단독 인덱스 없음).
    let timing: Vec<BoxTiming> = sqlx::query_as(
        "SELECT t.jobtype, count(*)::int8 AS n,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM
                   (CASE WHEN t.jobtype='DS' THEN t.pickup_ts ELSE t.free_ts END) - t.dispatch_ts)))::float8 AS realized_lead_p50_s,
                (percentile_cont(0.9) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM
                   (CASE WHEN t.jobtype='DS' THEN t.pickup_ts ELSE t.free_ts END) - t.dispatch_ts)))::float8 AS realized_lead_p90_s,
                max(l.modeled_arrival_s)::int4 AS modeled_travel_s,
                max(l.realized_lead_s)::int4   AS learned_lead_s
           FROM tt_move_log t
           LEFT JOIN learn_dispatch_lead l ON l.jobtype = t.jobtype
          WHERE t.business_date >= ((now() AT TIME ZONE 'Asia/Kuala_Lumpur')::date - 1)
            AND t.dispatch_ts > now() - interval '24 hours'
            AND t.jobtype IN ('DS','LD')
          GROUP BY t.jobtype ORDER BY t.jobtype",
    )
    .fetch_all(&pool)
    .await?;
    Ok(Json(BoxCompareOut {
        boxes_reco,
        boxes_joined,
        truck_match_pct,
        tos_after_pct,
        gap_p25_s,
        gap_p50_s,
        gap_p75_s,
        margin_in_pct,
        by_job,
        timing,
        recent,
    }))
}

// ── 배차 추천 보드 (P1, 2026-08-10) ──────────────────────────────────────────────────────────
// 관제/배차 담당자용 실행 화면의 단일 원천. 최신 틱 추천(상자 단위·급한 순) + 풀 요약 +
// 신선도 + 채택률(추천 vs TOS 실배차 — TOS 배차 기록은 이미 추출 중이라 피드백이 공짜다).
// TOS 무접촉 공유안의 1단계(사람이 다리) 화면이 이걸 읽고, 2단계(HTTP Pull)도 같은 내용이다.

#[derive(Serialize, sqlx::FromRow)]
struct BoardReco {
    ytno: String,
    contno: Option<String>,
    qc: Option<String>,
    vessel: Option<String>,
    queuename: Option<String>,
    jobtype: Option<String>,
    src_block: Option<String>,
    dispatch_deadline_ts: Option<DateTime<Utc>>,
    dd_slack_s: Option<i32>,
    arrival_s: Option<i32>,
    switched: Option<bool>,
}
#[derive(Serialize, sqlx::FromRow)]
struct BoardPoolStat {
    n_works: Option<i32>,
    n_trucks: Option<i32>,
    trucks_held: Option<i32>,
    overdue: Option<i32>,
}
/// 배차 깔때기 — "추천이 왜 이 수뿐인가"의 답을 단계별 계수로. 발행/미배차/도래는
/// 상자(트럭 몫) 단위, planned_backlog_cont 만 컨테이너 단위(계획 카운터 − 발행분).
/// '마감 도래'는 매처와 같은 잣대(livemap::POOL_MARGIN_S)로 센 라이브 값이다.
#[derive(Serialize)]
struct BoardFunnel {
    planned_backlog_cont: i64,
    issued: i32,
    unassigned: i32,
    due_now: i32,
    overdue_now: i32,
}
#[derive(Serialize, sqlx::FromRow)]
struct BoardAdoption {
    /// 분모: 최근 24시간에 우리가 추천한 상자 수(contno 단위, 최초 추천 시각 기준)
    boxes_reco: i64,
    /// 그중 최초 추천 후 20분 안에 TOS 가 실제로 배차한 상자 수
    boxes_dispatched: i64,
    box_pct: Option<f64>,
    /// 배차된 상자 중 트럭까지 우리 추천과 일치한 비율(배차 이전 추천 행 기준)
    ytno_match_pct: Option<f64>,
}
#[derive(Serialize, sqlx::FromRow)]
struct AdoptionPt {
    captured_at: DateTime<Utc>,
    box_pct: Option<f64>,
    ytno_match_pct: Option<f64>,
}
#[derive(Serialize)]
pub struct DispatchBoardOut {
    mode: String,
    generated_at: Option<DateTime<Utc>>,
    age_s: Option<i64>,
    recos: Vec<BoardReco>,
    pool: Option<BoardPoolStat>,
    funnel: Option<BoardFunnel>,
    adoption: Option<BoardAdoption>,
    /// 채택률 시계열(시간당 ~1점, 최근 7일 — mig 0144)
    adoption_trend: Vec<AdoptionPt>,
}

/// `GET /api/dispatch/board`
pub async fn dispatch_board(State(pool): State<PgPool>) -> Result<Json<DispatchBoardOut>, AppError> {
    let generated_at: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT max(ts) FROM stage2_match_shadow")
            .fetch_one(&pool)
            .await?;
    let age_s = generated_at.map(|t| (Utc::now() - t).num_seconds());
    let recos: Vec<BoardReco> = match generated_at {
        Some(ts) => sqlx::query_as(
            "SELECT ytno, contno, qc, vessel, queuename, jobtype, src_block,
                    dispatch_deadline_ts, dd_slack_s, arrival_s, switched
               FROM stage2_match_shadow WHERE ts = $1
              ORDER BY dd_slack_s ASC NULLS LAST LIMIT 40",
        )
        .bind(ts)
        .fetch_all(&pool)
        .await?,
        None => Vec::new(),
    };
    let pool_stat: Option<BoardPoolStat> = sqlx::query_as(
        "SELECT n_works, n_trucks, trucks_held_n AS trucks_held, pool_overdue_n AS overdue
           FROM stage2_solver_shadow WHERE pool_mode = 3 ORDER BY ts DESC LIMIT 1",
    )
    .fetch_optional(&pool)
    .await?;
    // 깔때기 — 매처 틱과 무관하게 지금 시점 라이브로 센다(같은 산식·같은 여유 상수).
    // 발행 상자는 stage2_work_candidates 의 상자 경로(contno 有), 마감 도래는 미배차 중
    // dispatch_deadline_ts ≤ now + POOL_MARGIN_S. 잔여 계획은 컨테이너 단위라 따로 표기.
    let funnel = match stage2_work_candidates(pool.clone()).await {
        Ok((wpo, cand)) => {
            let now = Utc::now();
            let margin = chrono::Duration::seconds(crate::livemap::POOL_MARGIN_S);
            let (mut issued, mut unassigned, mut due_now, mut overdue_now) = (0i32, 0i32, 0i32, 0i32);
            let mut issued_cont: i64 = 0;
            for w in &cand {
                if w.contno.is_none() { continue }
                issued += 1;
                issued_cont += w.contnos.len().max(1) as i64;
                if w.tos_assigned { continue }
                unassigned += 1;
                if let Some(dd) = w.dispatch_deadline_ts {
                    if dd <= now + margin {
                        due_now += 1;
                        if dd < now { overdue_now += 1; }
                    }
                }
            }
            Some(BoardFunnel {
                planned_backlog_cont: (wpo.total_remaining - issued_cont).max(0),
                issued, unassigned, due_now, overdue_now,
            })
        }
        Err(_) => None,
    };
    // 채택률 — contno 는 mig 0142(2026-08-10)부터 기록되므로 24h 창은 그때부터 찬다.
    // 프로브 인덱스: stage2_match_shadow(contno, ts) = mig 0143.
    let adoption: Option<BoardAdoption> = sqlx::query_as(
        "WITH r AS (
           SELECT contno, min(ts) AS first_ts
             FROM stage2_match_shadow
            WHERE ts > now() - interval '24 hours' AND contno IS NOT NULL
            GROUP BY contno
         ), d AS (
           SELECT r.contno, t.ytno, t.dispatch_ts
             FROM r
             JOIN LATERAL (
               SELECT ytno, dispatch_ts FROM tt_move_log t
                WHERE t.contno = r.contno AND t.dispatch_ts >= r.first_ts
                  AND t.dispatch_ts < r.first_ts + interval '20 minutes'
                ORDER BY t.dispatch_ts LIMIT 1) t ON true
         )
         SELECT (SELECT count(*) FROM r)::int8 AS boxes_reco,
                count(*)::int8 AS boxes_dispatched,
                (100.0*count(*)/nullif((SELECT count(*) FROM r),0))::float8 AS box_pct,
                (100.0*count(*) FILTER (WHERE EXISTS (
                   SELECT 1 FROM stage2_match_shadow m
                    WHERE m.contno = d.contno AND m.ytno = d.ytno AND m.ts <= d.dispatch_ts))
                  /nullif(count(*),0))::float8 AS ytno_match_pct
           FROM d",
    )
    .fetch_optional(&pool)
    .await
    .unwrap_or(None);
    let adoption_trend: Vec<AdoptionPt> = sqlx::query_as(
        "SELECT captured_at, box_pct, ytno_match_pct FROM dispatch_adoption_metric
          WHERE captured_at > now() - interval '7 days' ORDER BY captured_at",
    )
    .fetch_all(&pool)
    .await
    .unwrap_or_default();
    Ok(Json(DispatchBoardOut {
        mode: std::env::var("DISPATCH_MODE").unwrap_or_else(|_| "shadow".into()),
        generated_at,
        age_s,
        recos,
        pool: pool_stat,
        funnel,
        adoption,
        adoption_trend,
    }))
}
