//! TOS 가 "배차 가능"하다고 보는 트럭 목록을 사후에 정확히 복원한다 (mig 0162).
//!
//! TOS 배차기는 초당 여러 번 `tlc.mover.getMoverList` 로 목록을 뽑는다 — 로그인(ONOFF='O') ∧ 바쁜 작업지시
//! 없음 ∧ 열린 장비정지 없음. 트럭은 이 목록에 몇 초만 머물러 1분마다 찍어서는 거의 못 본다.
//! 그래서 1분에 한 번, **한 문장**으로 같은 순간의 스냅샷(트럭·바쁜 작업지시·장비정지)과 그 사이의
//! 변경 기록(JOB_ORDER_HISTORY I/U/D + 행마다의 **커밋 번호** ORA_ROWSCN)·로그인/로그아웃 시각을 받아,
//! 직전 스냅샷에서 **커밋 순서대로, 커밋 한 번을 한 걸음으로** 재생한다. 재생 끝을 이번 스냅샷과 대조해
//! 정확도를 `tos_avail_check` 에 남기고, 어긋난 것은 바로잡는다.
//!
//! 왜 커밋 단위인가(2026-10-01): 두 표 모두 ROWDEPENDENCIES 가 켜져 있어 ORA_ROWSCN 이 행마다의 커밋 번호다.
//! 스왑은 "떼기 → 붙이기"가 한 커밋 안에서 몇십 ms 간격으로 따로 찍히는데(15분 실측: 스왑 커밋 181개 중 163개가
//! 컨테이너 둘 이상을 한 커밋에서 바꿈) 그 사이 상태는 남에게 보인 적이 없다. 반대로 "작업 끝 → 신규 배차"는
//! 191쌍 전부 서로 다른 커밋이었다(1초 미만 2쌍 포함). 시간 문턱(한때 1초)으로는 둘을 가를 수 없었다.
//!
//! 재생 로직(`replay`)은 DB·Oracle 없이 도는 순수 함수라 단위 테스트로 고정한다.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, NaiveDateTime, SubsecRound, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tt_core::parse::parse_rows;

use crate::kpis::common::run_logged;
use crate::runner::Toolbox;

/// getMoverList 가 트럭을 "바쁨"으로 치는 YT_STATUS(NULL 은 'X'). 풀어주는 값은 'C' 하나다.
const BUSY: [&str; 6] = ["A", "B", "F", "R", "X", "Q"];
/// 변경 기록을 직전 스냅샷보다 이만큼 앞부터 다시 읽는다 — 기록 시각은 문장이 돈 순간이고 커밋은 그보다 늦을 수
/// 있어서다(한 커밋 안 시각 폭 p90 3.9초·최대 15초, 2026-10-01 15분 실측). 이미 반영된 것은 커밋 번호로 거르므로
/// 창을 넉넉히 잡아도 두 번 적용되지 않는다.
const OVERLAP_S: i64 = 120;
/// 첫 수집·공백 뒤에는 기준 시각을 서버 시계로 잡는다. Oracle 시계와의 어긋남을 이만큼 넉넉히 덮는다.
const CLOCK_MARGIN_S: i64 = 10;
/// 직전 스냅샷이 이보다 오래면(수집 공백) 재생하지 않고 새로 시작한다.
const MAX_REPLAY_S: i64 = 900;
/// 트럭 마스터가 이보다 적게 오면 조회가 잘못된 것 — 기록하지 않고 실패시킨다(전 트럭이 "로그아웃"으로
/// 보여 열린 구간을 전부 닫는 사고를 막는다). 평소 977대.
const MIN_MACHINES: usize = 100;
/// 로그인한 트럭이 이만큼 넘는데 바쁜 작업지시가 한 줄도 없으면 조회가 잘못된 것(평소 ~500줄) — 그대로 쓰면
/// 로그인한 트럭 전부가 한 틱 동안 목록에 들어간다.
const MIN_ON_FOR_BUSY_GUARD: usize = 50;

/// (CONTNO, POINT, SEQNO) = 작업지시 행 하나(기록 표 기본키와 같은 조합). ⚠ (CONTNO, POINT) 만으로는 유일하지
/// 않다 — 같은 컨테이너에 SEQNO 끝이 문자('…22I')인 진행 행과 숫자인 완료 행이 함께 있는 경우가 있고(2026-09-30
/// TT689), 두 키로 묶으면 완료 행 기록이 진행 행을 덮어 트럭이 비어 보였다. SEQNO 가 바뀌는 것은 전부 옛 행 삭제 +
/// 새 행 추가였고, 수정(U)이 SEQNO 를 바꾼 사례는 없었다(10분 기록 1,251 키 중 여러 SEQNO 49 키 전수).
pub type RowKey = (String, String, String);

#[derive(Clone, Debug, PartialEq)]
pub struct JobRow {
    pub ytno: Option<String>,
    pub ys: String, // NVL(YT_STATUS,'X')
}

#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    pub onoff: String,
    pub pool: Option<String>,
}

/// 한 순간의 상태 — getMoverList 를 계산하는 데 필요한 전부.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    /// 바쁜 작업지시(YTNO 있음 ∧ YT_STATUS ∈ BUSY). 재생 중에는 풀린 행도 잠시 담긴다.
    pub rows: HashMap<RowKey, JobRow>,
    pub machines: HashMap<String, Machine>,
    /// 열린 장비정지로 막힌 트럭.
    pub stopped: HashSet<String>,
}

impl State {
    /// getMoverList 와 같은 판정.
    pub fn free_set(&self) -> BTreeSet<String> {
        let busy: HashSet<&str> = self
            .rows
            .values()
            .filter(|r| BUSY.contains(&r.ys.as_str()))
            .filter_map(|r| r.ytno.as_deref())
            .collect();
        self.machines
            .iter()
            .filter(|(c, m)| m.onoff == "O" && !self.stopped.contains(*c) && !busy.contains(c.as_str()))
            .map(|(c, _)| c.clone())
            .collect()
    }
}

/// 직전 틱이 남긴 재생 출발점.
#[derive(Clone, Debug, PartialEq)]
pub struct Anchor {
    pub as_of: DateTime<Utc>,
    pub state: State,
    /// 이 스냅샷에 이미 반영된 커밋 번호의 상한 — 이하는 다시 적용하지 않는다. 이번 스냅샷에서 보인 기록 중 가장
    /// 큰 커밋 번호다: 그보다 작은 커밋은 먼저 끝났으니 스냅샷에 보였고, 스냅샷 뒤에 끝난 커밋은 반드시 더 크다.
    pub scn: u64,
    /// 구간 표에 쓴 가장 늦은 전이 시각 — 다음 틱의 전이는 이보다 뒤에 놓는다(같은 트럭·같은 시각 키 충돌 방지).
    pub last_ts: DateTime<Utc>,
    /// 이 스냅샷이 본 트럭별 마지막 로그인·로그아웃 시각. 다음 틱은 **값이 바뀐 것만** 새 로그온으로 적용한다.
    /// 로그인은 ONOFF 와 마지막 로그인 시각(LASTDATE/TIME)을 한 UPDATE 로 쓰므로(운영 SQL updateMchnOnoff), 앵커가 그
    /// 시각을 이미 봤다면 그 로그인은 앵커에 반영돼 있다 — 시간 창으로 고르면 이미 반영된 옛 로그인을 다시 써서, 기록에
    /// 안 남는 로그아웃(감독자 강제 로그아웃 등) 뒤의 트럭을 되살린다(2026-10-01 2초 대조 실측 TT1421, 1분 헛 등재).
    /// 반대로 앵커보다 앞 시각에 찍혔어도 앵커가 못 본 값이면 늦게 보인 로그인이다(같은 날 라이브: 로그인 16건 중 3건).
    /// None = 이 정보가 없던 옛 앵커 — 그때만 "앵커가 든 초 이후" 시각으로 고른다.
    pub logons: Option<Logons>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Logons {
    pub last_on: HashMap<String, DateTime<Utc>>,
    pub last_off: HashMap<String, DateTime<Utc>>,
}

/// JOB_ORDER_HISTORY 한 행 = 작업지시 한 행의 변경 뒤 모습(I/U) 또는 삭제(D).
#[derive(Clone, Debug)]
pub struct Change {
    pub scn: u64,          // ORA_ROWSCN = 이 행을 남긴 커밋의 번호
    pub ts: DateTime<Utc>, // 문장이 돈 순간(밀리초) — 커밋 순간이 아니다
    pub ty: char,          // 'I' | 'U' | 'D'
    pub key: RowKey,
    pub ytno: Option<String>,
    pub ys: String,
    pub jobtype: Option<String>,
    pub program: Option<String>,
}

/// 이번 스냅샷. `last_on`/`last_off` 는 트럭별 마지막 로그인/로그아웃 시각(초 단위).
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub as_of: DateTime<Utc>,
    pub state: State,
    pub last_on: HashMap<String, DateTime<Utc>>,
    pub last_off: HashMap<String, DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Transition {
    pub ytno: String,
    pub ts: DateTime<Utc>,
    pub enter: bool,
    pub cause: &'static str,
    pub program: Option<String>,
    pub contno: Option<String>,
    pub jobtype: Option<String>,
}

#[derive(Debug)]
pub struct Replay {
    pub transitions: Vec<Transition>,
    pub replay_free: BTreeSet<String>,
    pub truth_free: BTreeSet<String>,
    /// 새로 적용한 변경 기록 행 수 / 커밋 수.
    pub n_events: usize,
    pub n_txn: usize,
    /// 앵커 이전 시각에 찍혔지만 앵커 뒤에 커밋된 기록 때문에 상태가 바뀐 트럭 수.
    pub n_late: usize,
    pub n_logon: usize,
    pub scn: u64,
    pub last_ts: DateTime<Utc>,
}

/// 같은 커밋 안의 순서: 기록 시각, 같은 시각이면 U → D → I(같은 행의 "수정 → 삭제"가 한 시각에 몰려도 삭제가 이긴다).
fn ty_rank(ty: char) -> u8 {
    match ty {
        'U' => 0,
        'D' => 1,
        _ => 2,
    }
}

/// 전이 시각은 앞 걸음보다 반드시 뒤 — 같으면 1µs 민다. 커밋 순서 = 보이는 순서라 시각도 커밋 순서를 따라야 하고,
/// (트럭, 등재 시각) 키가 겹치지 않는다.
fn step_ts(want: DateTime<Utc>, last: &mut DateTime<Utc>) -> DateTime<Utc> {
    let t = if want > *last { want } else { *last + Duration::microseconds(1) };
    *last = t;
    t
}

struct Blame<'a> {
    /// 트럭을 바쁘게 만든 기록.
    took: HashMap<String, &'a Change>,
    /// 트럭을 풀어준 기록 — 그 트럭이 붙어 있던 행이 끝나거나(C), 다른 트럭으로 가거나, 지워진 것.
    freed: HashMap<String, &'a Change>,
}

fn apply_txn<'a>(cur: &mut State, evs: &[&'a Change]) -> Blame<'a> {
    let mut b = Blame { took: HashMap::new(), freed: HashMap::new() };
    for ev in evs {
        if let Some(prev) = cur.rows.get(&ev.key) {
            if let Some(y) = prev.ytno.as_deref() {
                let still_holds = ev.ty != 'D' && ev.ytno.as_deref() == Some(y) && BUSY.contains(&ev.ys.as_str());
                if !still_holds {
                    b.freed.insert(y.to_string(), ev);
                }
            }
        }
        if ev.ty != 'D' && BUSY.contains(&ev.ys.as_str()) {
            if let Some(y) = ev.ytno.as_deref() {
                b.took.insert(y.to_string(), ev);
            }
        }
        match ev.ty {
            'D' => {
                cur.rows.remove(&ev.key);
            }
            _ => {
                cur.rows.insert(ev.key.clone(), JobRow { ytno: ev.ytno.clone(), ys: ev.ys.clone() });
            }
        }
    }
    b
}

fn diff_into(
    out: &mut Vec<Transition>,
    before: &BTreeSet<String>,
    after: &BTreeSet<String>,
    ts: DateTime<Utc>,
    blame: Option<&Blame>,
    logon_changed: &HashSet<String>,
    late: bool,
) {
    for y in after.difference(before) {
        let ev = blame.and_then(|b| b.freed.get(y));
        let cause = if logon_changed.contains(y) {
            "login"
        } else if late {
            "late"
        } else {
            "release"
        };
        out.push(Transition {
            ytno: y.clone(),
            ts,
            enter: true,
            cause,
            program: if cause == "login" { None } else { ev.and_then(|e| e.program.clone()) },
            contno: None,
            jobtype: None,
        });
    }
    for y in before.difference(after) {
        let ev = blame.and_then(|b| b.took.get(y)).filter(|_| !logon_changed.contains(y));
        let cause = if logon_changed.contains(y) {
            "logout"
        } else if late {
            "late"
        } else {
            "dispatch"
        };
        out.push(Transition {
            ytno: y.clone(),
            ts,
            enter: false,
            cause,
            program: ev.and_then(|e| e.program.clone()),
            contno: ev.map(|e| e.key.0.clone()),
            jobtype: ev.and_then(|e| e.jobtype.clone()),
        });
    }
}

/// 직전 스냅샷(`anchor`)에서 변경 기록과 로그온 시각으로 재생하고, 끝을 이번 스냅샷과 대조한다.
///
/// - 커밋 번호가 `anchor.scn` 이하인 기록은 이미 앵커에 들어 있다 — 건너뛴다.
/// - 나머지는 커밋 번호 순으로, **커밋 하나를 한 걸음**으로 적용한다(커밋 안의 중간 상태는 남에게 보인 적이 없다).
///   걸음의 시각 = 그 커밋의 마지막 기록 시각(커밋 순간의 하한). 그 커밋의 기록이 전부 앵커 이전 시각이면 늦게 커밋된
///   것이라 원인을 `late` 로 두고 시각은 앵커 뒤로 민다.
/// - 로그인·로그아웃(초 단위)은 앵커가 본 값과 **달라진 것만** 새 사건으로 보고, 시각으로 끼워 넣는다: 커밋의 마지막
///   기록 시각보다 앞선 로그온을 먼저 적용한다. 앵커 이전 시각이면(늦게 보인 것) 앵커 직후에 붙는다. 실제로 바뀐 트럭만
///   로그온 원인이다. (한계: 로그인·로그아웃은 트럭별 마지막 한 번씩만 보이고, 기록에 안 남는 로그아웃은 스냅샷 대조로만 잡힌다.)
/// - 끝에서 이번 스냅샷의 실제 목록과 다른 트럭은 `reconcile` 로 스냅샷 시각에 바로잡는다.
pub fn replay(anchor: &Anchor, changes: &[Change], snap: &Snapshot) -> Replay {
    let s0 = anchor.as_of.trunc_subsecs(0);
    let is_new = |seen: Option<&HashMap<String, DateTime<Utc>>>, c: &str, t: DateTime<Utc>| match seen {
        Some(m) => m.get(c) != Some(&t),
        None => t >= s0,
    };
    let mut cur = anchor.state.clone();
    let mut free = cur.free_set();
    let mut out = Vec::new();
    let mut last = anchor.as_of.max(anchor.last_ts);

    let mut logon: Vec<(DateTime<Utc>, &str, bool)> = Vec::new();
    for (c, t) in &snap.last_on {
        if *t <= snap.as_of && is_new(anchor.logons.as_ref().map(|l| &l.last_on), c, *t) {
            logon.push((*t, c.as_str(), true));
        }
    }
    for (c, t) in &snap.last_off {
        if *t <= snap.as_of && is_new(anchor.logons.as_ref().map(|l| &l.last_off), c, *t) {
            logon.push((*t, c.as_str(), false));
        }
    }
    logon.sort();
    let n_logon = logon.len();

    let mut txns: BTreeMap<u64, Vec<&Change>> = BTreeMap::new();
    for c in changes.iter().filter(|c| c.scn > anchor.scn) {
        txns.entry(c.scn).or_default().push(c);
    }
    let scn = changes.iter().map(|c| c.scn).max().unwrap_or(0).max(anchor.scn);
    let n_events = txns.values().map(Vec::len).sum();
    let n_txn = txns.len();
    let mut n_late = 0;

    let apply_logon = |cur: &mut State, free: &mut BTreeSet<String>, out: &mut Vec<Transition>, last: &mut DateTime<Utc>, (t, code, on): (DateTime<Utc>, &str, bool)| {
        let was_on = cur.machines.get(code).is_some_and(|m| m.onoff == "O");
        let pool = snap.state.machines.get(code).and_then(|m| m.pool.clone());
        let m = cur.machines.entry(code.to_string()).or_insert(Machine { onoff: "F".into(), pool });
        m.onoff = if on { "O".into() } else { "F".into() };
        if was_on == on {
            return;
        }
        let after = cur.free_set();
        if after != *free {
            let ts = step_ts(t.max(anchor.as_of), last);
            let changed: HashSet<String> = [code.to_string()].into_iter().collect();
            diff_into(out, free, &after, ts, None, &changed, false);
            *free = after;
        }
    };

    let mut li = 0;
    for (_, mut evs) in txns {
        evs.sort_by(|a, b| (a.ts, ty_rank(a.ty)).cmp(&(b.ts, ty_rank(b.ty))));
        let g_ts = evs.iter().map(|c| c.ts).max().expect("group is non-empty");
        while li < logon.len() && logon[li].0 < g_ts {
            apply_logon(&mut cur, &mut free, &mut out, &mut last, logon[li]);
            li += 1;
        }
        let late = g_ts <= anchor.as_of;
        let blame = apply_txn(&mut cur, &evs);
        let after = cur.free_set();
        if after != free {
            if late {
                n_late += after.symmetric_difference(&free).count();
            }
            let ts = step_ts(g_ts.max(anchor.as_of), &mut last);
            diff_into(&mut out, &free, &after, ts, Some(&blame), &HashSet::new(), late);
            free = after;
        }
    }
    while li < logon.len() {
        apply_logon(&mut cur, &mut free, &mut out, &mut last, logon[li]);
        li += 1;
    }

    // 스냅샷과 대조.
    let truth = snap.state.free_set();
    if truth != free {
        let ts = step_ts(snap.as_of, &mut last);
        for y in truth.difference(&free) {
            out.push(Transition {
                ytno: y.clone(),
                ts,
                enter: true,
                cause: "reconcile",
                program: None,
                contno: None,
                jobtype: None,
            });
        }
        for y in free.difference(&truth) {
            out.push(Transition {
                ytno: y.clone(),
                ts,
                enter: false,
                cause: "reconcile",
                program: None,
                contno: None,
                jobtype: None,
            });
        }
    }

    Replay { transitions: out, replay_free: free, truth_free: truth, n_events, n_txn, n_late, n_logon, scn, last_ts: last }
}

// ───────────────────────── Oracle ─────────────────────────

/// 한 문장 = 한 SCN. 스냅샷 네 조각과 변경 기록이 같은 순간 기준이다.
/// 로그아웃 시각은 **지금 로그인 중인 트럭도** 받는다 — 두 스냅샷 사이에 로그아웃했다 다시 로그인하는 일이
/// 로그아웃의 22.9%(09-29~30 실측)라, 지금 상태만 보고 가르면 그 사이를 놓친다.
/// 비용 실측(2026-09-30): 장비정지를 TOS 식(트럭마다 상관 서브쿼리)으로 쓰면 이것만 7.8천~14천 gets 라,
/// 열린 정지를 한 번 훑고 창 함수로 거른다(1.8천 gets, 전 장비 유형에서 결과 동일 확인).
/// ⚠ 조회 도구는 SQL 어디든 주석 기호·쓰기 단어가 있으면 거부한다 — `build_sql_passes_toolbox_rules` 테스트.
fn build_sql(hist_from: &str) -> String {
    format!(
        "SELECT 'T' AS k, TO_CHAR(SYSTIMESTAMP,'YYYYMMDDHH24MISSFF3') AS a, NULL AS b, NULL AS c, NULL AS d,
                NULL AS e, NULL AS f, NULL AS g, NULL AS h, NULL AS i, NULL AS j
           FROM dual
         UNION ALL
         SELECT 'M', m.CDY_MCHN_CODE, m.CDY_MCHN_TYPE, m.CDY_MCHN_ONOFF, m.CDY_MCHN_YTPOOLNAME,
                m.CDY_MCHN_LASTDATE||m.CDY_MCHN_LASTTIME,
                (SELECT TO_CHAR(MAX(w.LGOUT_DT),'YYYYMMDDHH24MISS') FROM TOSADM.MCH_WORKTIME w
                  WHERE w.MCH_WORK_MACHNO = m.CDY_MCHN_CODE AND w.LGOUT_DT >= SYSDATE - 1/24),
                NULL, NULL, NULL, NULL
           FROM TOSADM.CDY_MACHINE m
          WHERE m.CDY_MCHN_TYPE IN ('SC','YT','AT')
         UNION ALL
         SELECT 'B', JOB_ODR_CONTNO, TO_CHAR(JOB_ODR_POINT), JOB_ODR_SEQNO, JOB_ODR_YTNO,
                NVL(JOB_ODR_YT_STATUS,'X'), NULL, NULL, NULL, NULL, NULL
           FROM TOSADM.JOB_ORDER_LIST
          WHERE JOB_ODR_YTNO IS NOT NULL AND NVL(JOB_ODR_YT_STATUS,'X') IN ('A','B','F','R','X','Q')
         UNION ALL
         SELECT 'W', o.MCH_STOP_MACHNO, o.MCH_STOP_CODE, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL
           FROM (SELECT a.MCH_STOP_MACHNO, a.MCH_STOP_CODE, a.MCH_STOP_DATE, a.MCH_STOP_SEQ,
                        MAX(a.MCH_STOP_DATE) OVER (PARTITION BY a.MCH_STOP_MACHNO) AS mxd,
                        MAX(a.MCH_STOP_SEQ)  OVER (PARTITION BY a.MCH_STOP_MACHNO) AS mxs
                   FROM TOSADM.MCH_EQ_WORKSTOP a
                  WHERE a.MCH_STOP_ENDTIME IS NULL) o,
                TOSADM.CDY_MACHINE m, TOSADM.CDY_MACHINE_JOBSTOP b
          WHERE (o.MCH_STOP_DATE || o.MCH_STOP_SEQ) = (o.mxd || o.mxs)
            AND o.MCH_STOP_MACHNO = m.CDY_MCHN_CODE
            AND m.CDY_MCHN_TYPE IN ('SC','YT','AT')
            AND o.MCH_STOP_CODE = b.CDY_MSTP_CODE AND b.MCHN_TP = m.CDY_MCHN_TYPE
         UNION ALL
         SELECT 'H', JOB_HIST_DATE||JOB_HIST_TIME, JOB_HIST_TYPE, JOB_HIST_CONTNO, TO_CHAR(JOB_HIST_POINT),
                JOB_HIST_SEQNO, JOB_HIST_YTNO, NVL(JOB_HIST_YT_STATUS,'X'), JOB_HIST_JOBTYPE,
                SUBSTR(JOB_HIST_PROGRAM,1,60), TO_CHAR(ORA_ROWSCN)
           FROM TOSADM.JOB_ORDER_HISTORY
          WHERE JOB_HIST_DATE||JOB_HIST_TIME >= '{hist_from}'"
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
struct RawRow {
    k: String,
    a: Option<String>,
    b: Option<String>,
    c: Option<String>,
    d: Option<String>,
    e: Option<String>,
    f: Option<String>,
    g: Option<String>,
    h: Option<String>,
    i: Option<String>,
    j: Option<String>,
}

fn clean(s: &Option<String>) -> Option<String> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// MYT "YYYYMMDDHH24MISS[mmm]" → UTC. 밀리초 꼬리는 있으면 살린다.
fn parse_myt(raw: &str) -> Option<DateTime<Utc>> {
    let s = raw.trim();
    if s.len() < 14 || !s.as_bytes()[..14].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(&s[..14], "%Y%m%d%H%M%S").ok()?;
    let mut t = tt_core::shift::terminal_to_utc(naive);
    if s.len() >= 17 && s.as_bytes()[14..17].iter().all(u8::is_ascii_digit) {
        t += Duration::milliseconds(s[14..17].parse::<i64>().ok()?);
    }
    Some(t)
}

/// DATE||TIME 과 같은 17자리(밀리초) — 기록 표 키와 문자열 비교가 그대로 시각 비교가 된다.
fn to_myt_ms(t: DateTime<Utc>) -> String {
    t.with_timezone(&tt_core::shift::terminal_offset()).format("%Y%m%d%H%M%S%3f").to_string()
}

/// 이번 틱이 변경 기록을 읽기 시작할 시각. 이미 반영된 기록은 커밋 번호로 거르므로 이 창은 넉넉하기만 하면 된다 —
/// 조건은 "직전 스냅샷 뒤에 커밋될 수 있는 기록을 다 덮을 것". 첫 수집·공백이면 서버 시계 기준으로 잡는다.
fn hist_from(anchor: Option<DateTime<Utc>>, now: DateTime<Utc>) -> DateTime<Utc> {
    match anchor {
        Some(a) => a - Duration::seconds(OVERLAP_S),
        None => now - Duration::seconds(OVERLAP_S + CLOCK_MARGIN_S),
    }
}

fn parse_fetch(rows: Vec<RawRow>) -> Result<(Snapshot, Vec<Change>)> {
    let mut as_of = None;
    let mut state = State::default();
    let mut last_on = HashMap::new();
    let mut last_off = HashMap::new();
    let mut changes = Vec::new();
    let mut bad_scn = 0usize;
    let mut bad_hist = 0usize;
    for r in rows {
        match r.k.as_str() {
            "T" => as_of = r.a.as_deref().and_then(parse_myt),
            "M" => {
                let Some(code) = clean(&r.a) else { continue };
                if let Some(t) = r.e.as_deref().and_then(parse_myt) {
                    last_on.insert(code.clone(), t);
                }
                if let Some(t) = r.f.as_deref().and_then(parse_myt) {
                    last_off.insert(code.clone(), t);
                }
                state.machines.insert(
                    code,
                    Machine { onoff: clean(&r.c).unwrap_or_default(), pool: clean(&r.d) },
                );
            }
            "B" => {
                let (Some(contno), Some(point)) = (clean(&r.a), clean(&r.b)) else { continue };
                state.rows.insert(
                    (contno, point, clean(&r.c).unwrap_or_default()),
                    JobRow { ytno: clean(&r.d), ys: clean(&r.e).unwrap_or_else(|| "X".into()) },
                );
            }
            "W" => {
                if let Some(code) = clean(&r.a) {
                    state.stopped.insert(code);
                }
            }
            "H" => {
                let (Some(ts), Some(ty), Some(contno), Some(point)) = (
                    r.a.as_deref().and_then(parse_myt),
                    r.b.as_deref().and_then(|s| s.trim().chars().next()),
                    clean(&r.c),
                    clean(&r.d),
                ) else {
                    bad_hist += 1;
                    continue;
                };
                let Some(scn) = r.j.as_deref().and_then(|s| s.trim().parse::<u64>().ok()) else {
                    bad_scn += 1;
                    continue;
                };
                changes.push(Change {
                    scn,
                    ts,
                    ty,
                    key: (contno, point, clean(&r.e).unwrap_or_default()),
                    ytno: clean(&r.f),
                    ys: clean(&r.g).unwrap_or_else(|| "X".into()),
                    jobtype: clean(&r.h),
                    program: clean(&r.i),
                });
            }
            _ => {}
        }
    }
    let Some(as_of) = as_of else { bail!("tos-avail: 스냅샷 시각(T 행)이 없다") };
    if state.machines.len() < MIN_MACHINES {
        bail!("tos-avail: 트럭 마스터가 {}대뿐 — 조회 이상으로 보고 기록하지 않는다", state.machines.len());
    }
    let n_on = state.machines.values().filter(|m| m.onoff == "O").count();
    if state.rows.is_empty() && n_on >= MIN_ON_FOR_BUSY_GUARD {
        bail!("tos-avail: 로그인 {n_on}대인데 바쁜 작업지시가 0줄 — 조회 이상으로 보고 기록하지 않는다");
    }
    if bad_scn + bad_hist > 0 {
        // 커밋 번호·시각·종류·키를 읽지 못한 기록은 적용할 수 없다 — 버리되 알린다(평소 0). 형식이 바뀌면 재생이 조용히
        // "스냅샷 대조로만 바로잡기"로 떨어지므로 여기서 드러나야 한다.
        tracing::warn!(bad_scn, bad_hist, "tos-avail: 읽지 못한 변경 기록을 버렸다");
    }
    Ok((Snapshot { as_of, state, last_on, last_off }, changes))
}

// ───────────────────────── 앵커 저장 ─────────────────────────

#[derive(Serialize, Deserialize)]
struct AnchorJson {
    rows: Vec<(String, String, String, Option<String>, String)>,
    machines: BTreeMap<String, (String, Option<String>)>,
    stopped: Vec<String>,
    scn: u64,
    last_ts: DateTime<Utc>,
    #[serde(default)]
    last_on: Option<BTreeMap<String, DateTime<Utc>>>,
    #[serde(default)]
    last_off: Option<BTreeMap<String, DateTime<Utc>>>,
}

fn anchor_to_json(a: &Anchor) -> Result<String> {
    let s = &a.state;
    let mut rows: Vec<_> = s
        .rows
        .iter()
        .map(|((c, p, q), r)| (c.clone(), p.clone(), q.clone(), r.ytno.clone(), r.ys.clone()))
        .collect();
    rows.sort();
    let mut stopped: Vec<_> = s.stopped.iter().cloned().collect();
    stopped.sort();
    let j = AnchorJson {
        rows,
        machines: s.machines.iter().map(|(c, m)| (c.clone(), (m.onoff.clone(), m.pool.clone()))).collect(),
        stopped,
        scn: a.scn,
        last_ts: a.last_ts,
        last_on: a.logons.as_ref().map(|l| l.last_on.iter().map(|(k, v)| (k.clone(), *v)).collect()),
        last_off: a.logons.as_ref().map(|l| l.last_off.iter().map(|(k, v)| (k.clone(), *v)).collect()),
    };
    Ok(serde_json::to_string(&j)?)
}

fn anchor_from_json(as_of: DateTime<Utc>, raw: &str) -> Result<Anchor> {
    let j: AnchorJson = serde_json::from_str(raw)?;
    Ok(Anchor {
        as_of,
        state: State {
            rows: j.rows.into_iter().map(|(c, p, q, ytno, ys)| ((c, p, q), JobRow { ytno, ys })).collect(),
            machines: j.machines.into_iter().map(|(c, (onoff, pool))| (c, Machine { onoff, pool })).collect(),
            stopped: j.stopped.into_iter().collect(),
        },
        scn: j.scn,
        last_ts: j.last_ts,
        logons: match (j.last_on, j.last_off) {
            (Some(on), Some(off)) => Some(Logons { last_on: on.into_iter().collect(), last_off: off.into_iter().collect() }),
            _ => None,
        },
    })
}

// ───────────────────────── 한 틱 ─────────────────────────

/// 동시 실행(타이머 + 손으로 돌린 것) 직렬화. 앵커를 이 잠금 안에서 읽으므로 뒤에 온 실행은 앞 실행이 커밋한
/// 앵커부터 재생한다 — 잠금이 없으면 늦게 커밋한 쪽이 더 새 앵커를 옛것으로 덮는다.
const LOCK_KEY: i64 = 0x7454_4156_4149_4c; // "tTAVAIL"

pub async fn tick_tos_avail(pool: &PgPool, target: &str) -> Result<()> {
    let date = tt_core::shift::terminal_now().date_naive();
    run_logged(pool, "TOS_AVAIL", date, |_| async move {
        let mut tx = pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(LOCK_KEY).execute(&mut *tx).await?;
        let prev: Option<(DateTime<Utc>, String)> =
            sqlx::query_as("SELECT as_of_ts, snapshot::text FROM tos_avail_anchor WHERE id = 1")
                .fetch_optional(&mut *tx)
                .await?;
        let now = Utc::now();
        // 앵커가 너무 오래됐으면 재생하지 않는다 — 그 사이 변경은 한 번에 읽기엔 많고, 초 단위로 말할 근거도 없다.
        let usable = prev.as_ref().filter(|(t, _)| (now - *t).num_seconds() <= MAX_REPLAY_S);
        let from = hist_from(usable.map(|(t, _)| *t), now);

        let raw = Toolbox::from_env(target)?.run_sql(&build_sql(&to_myt_ms(from))).await?;
        let rows: Vec<RawRow> = parse_rows(&raw).context("parsing tos-avail rows")?;
        let (snap, changes) = parse_fetch(rows)?;
        let fetched_scn = changes.iter().map(|c| c.scn).max().unwrap_or(0);

        // 스냅샷이 앵커보다 앞선다 = Oracle 시계가 뒤로 갔다(잠금이 있어 동시 실행은 아니다). 실패를 반복하며
        // 시계가 따라올 때까지 멈추는 대신: 이번 스냅샷 시각 뒤로 뻗은 구간(열린 것, 닫혔지만 끝이 더 늦은 것)을 그 시각에
        // 공백으로 자르고(그보다 늦게 연 구간은 길이 0이 되어 어떤 시각 조회에도 안 걸린다) 앵커를 지워, 다음 틱이
        // 처음부터(init) 시작하게 한다.
        if let Some((a, _)) = &prev {
            if snap.as_of <= *a {
                tracing::warn!(snapshot = %snap.as_of, anchor = %a, "tos-avail: 스냅샷이 앵커보다 앞선다 — 새로 시작");
                // 이미 닫혔지만 끝이 새 스냅샷보다 늦은 구간도 자른다 — 그대로 두면 다음 init 이 연 구간과 같은 트럭으로 겹친다.
                sqlx::query(
                    "UPDATE tos_avail_interval SET exit_ts = GREATEST(enter_ts, $1), exit_cause = 'gap'
                      WHERE exit_ts IS NULL OR exit_ts > $1",
                )
                .bind(snap.as_of)
                .execute(&mut *tx)
                .await?;
                sqlx::query("DELETE FROM tos_avail_anchor WHERE id = 1").execute(&mut *tx).await?;
                sqlx::query(
                    "INSERT INTO tos_avail_check (as_of_ts, prev_as_of_ts, mode, n_truth)
                     VALUES ($1, $2, 'gap', $3) ON CONFLICT (as_of_ts) DO NOTHING",
                )
                .bind(snap.as_of)
                .bind(a)
                .bind(snap.state.free_set().len() as i32)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                return Ok(0);
            }
        }

        let (mode, rep) = match usable {
            Some((anchor_ts, anchor_raw)) => {
                let anchor = anchor_from_json(*anchor_ts, anchor_raw).context("reading tos_avail_anchor")?;
                ("replay", Some((*anchor_ts, replay(&anchor, &changes, &snap))))
            }
            None => (if prev.is_some() { "gap" } else { "init" }, None),
        };

        let mut n_trans = 0u64;
        let mut n_desync = 0i32;
        let (next_scn, next_last) = match &rep {
            Some((_, r)) => {
                for t in &r.transitions {
                    if !write_transition(&mut tx, t, &snap).await? {
                        n_desync += 1;
                    }
                }
                n_trans = r.transitions.len() as u64;
                (r.scn, r.last_ts)
            }
            None => {
                // 공백(또는 첫 수집): 열린 구간은 마지막으로 안 시각에 닫고, 지금 목록으로 새로 연다. 앵커가 없는데
                // 열린 구간이 남아 있으면(앵커를 지워 재시작한 경우) 마지막 점검 시각에 닫는다.
                sqlx::query(
                    "UPDATE tos_avail_interval
                        SET exit_ts = GREATEST(enter_ts, COALESCE($1, (SELECT max(as_of_ts) FROM tos_avail_check), enter_ts)),
                            exit_cause = 'gap'
                      WHERE exit_ts IS NULL",
                )
                .bind(prev.as_ref().map(|(t, _)| *t))
                .execute(&mut *tx)
                .await?;
                for y in snap.state.free_set() {
                    let t = Transition {
                        ytno: y,
                        ts: snap.as_of,
                        enter: true,
                        cause: "init",
                        program: None,
                        contno: None,
                        jobtype: None,
                    };
                    if !write_transition(&mut tx, &t, &snap).await? {
                        n_desync += 1;
                    }
                    n_trans += 1;
                }
                (fetched_scn, snap.as_of)
            }
        };
        if n_desync > 0 {
            tracing::warn!(n_desync, "tos-avail: 구간 표와 앵커가 어긋났다(건너뛴 등재·닫을 게 없던 이탈)");
        }

        let truth = snap.state.free_set();
        let (prev_ts, n_replay, n_match, missing, extra, n_events, n_late, n_logon, n_txn) = match &rep {
            Some((ts, r)) => (
                Some(*ts),
                Some(r.replay_free.len() as i32),
                Some(r.replay_free.intersection(&truth).count() as i32),
                Some(truth.difference(&r.replay_free).cloned().collect::<Vec<_>>()),
                Some(r.replay_free.difference(&truth).cloned().collect::<Vec<_>>()),
                Some(r.n_events as i32),
                Some(r.n_late as i32),
                Some(r.n_logon as i32),
                Some(r.n_txn as i32),
            ),
            None => (prev.as_ref().map(|(t, _)| *t), None, None, None, None, None, None, None, None),
        };
        sqlx::query(
            "INSERT INTO tos_avail_check
               (as_of_ts, prev_as_of_ts, mode, n_truth, n_replay, n_match, missing, extra, n_events, n_late, n_logon,
                n_txn, n_desync)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
             ON CONFLICT (as_of_ts) DO NOTHING",
        )
        .bind(snap.as_of)
        .bind(prev_ts)
        .bind(mode)
        .bind(truth.len() as i32)
        .bind(n_replay)
        .bind(n_match)
        .bind(missing)
        .bind(extra)
        .bind(n_events)
        .bind(n_late)
        .bind(n_logon)
        .bind(n_txn)
        .bind(n_desync)
        .execute(&mut *tx)
        .await
        .context("insert tos_avail_check")?;

        let anchor = Anchor {
            as_of: snap.as_of,
            state: snap.state.clone(),
            scn: next_scn,
            last_ts: next_last,
            logons: Some(Logons { last_on: snap.last_on.clone(), last_off: snap.last_off.clone() }),
        };
        sqlx::query(
            "INSERT INTO tos_avail_anchor (id, as_of_ts, snapshot) VALUES (1, $1, $2::jsonb)
             ON CONFLICT (id) DO UPDATE SET as_of_ts = EXCLUDED.as_of_ts, snapshot = EXCLUDED.snapshot",
        )
        .bind(snap.as_of)
        .bind(anchor_to_json(&anchor)?)
        .execute(&mut *tx)
        .await
        .context("upsert tos_avail_anchor")?;
        tx.commit().await?;

        tracing::info!(
            mode,
            truth = truth.len(),
            replay = ?n_replay,
            matched = ?n_match,
            events = changes.len(),
            transitions = n_trans,
            "tos-avail"
        );
        Ok(n_trans)
    })
    .await
    .map(|_| ())
}

/// 전이 하나를 구간 표에 쓴다. 표가 앵커와 맞으면 등재는 새 행 하나, 이탈은 열린 행 하나를 닫는다 —
/// 그렇지 않으면(건너뛴 등재, 닫을 게 없는 이탈) false 를 돌려 어긋남으로 센다.
async fn write_transition(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    t: &Transition,
    snap: &Snapshot,
) -> Result<bool> {
    let n = if t.enter {
        // 트럭당 열린 구간은 하나(부분 유일 인덱스) — 이미 열려 있으면 건너뛴다.
        let pool = snap.state.machines.get(&t.ytno).and_then(|m| m.pool.clone());
        sqlx::query(
            "INSERT INTO tos_avail_interval (ytno, enter_ts, enter_cause, enter_program, pool)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT DO NOTHING",
        )
        .bind(&t.ytno)
        .bind(t.ts)
        .bind(t.cause)
        .bind(&t.program)
        .bind(pool)
        .execute(&mut **tx)
        .await
        .context("insert tos_avail_interval")?
        .rows_affected()
    } else {
        sqlx::query(
            "UPDATE tos_avail_interval
                SET exit_ts = GREATEST(enter_ts, $2), exit_cause = $3, exit_program = $4,
                    exit_contno = $5, exit_jobtype = $6
              WHERE ytno = $1 AND exit_ts IS NULL",
        )
        .bind(&t.ytno)
        .bind(t.ts)
        .bind(t.cause)
        .bind(&t.program)
        .bind(&t.contno)
        .bind(&t.jobtype)
        .execute(&mut **tx)
        .await
        .context("close tos_avail_interval")?
        .rows_affected()
    };
    Ok(n == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(s: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 30, 6, 0, 0).unwrap() + Duration::seconds(s)
    }
    fn tm(ms: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 30, 6, 0, 0).unwrap() + Duration::milliseconds(ms)
    }
    fn mach(codes: &[(&str, &str)]) -> HashMap<String, Machine> {
        codes.iter().map(|(c, o)| (c.to_string(), Machine { onoff: o.to_string(), pool: Some("AAA".into()) })).collect()
    }
    fn row(y: &str, ys: &str) -> JobRow {
        JobRow { ytno: Some(y.into()), ys: ys.into() }
    }
    fn key(c: &str, seq: &str) -> RowKey {
        (c.into(), "1".into(), seq.into())
    }
    #[allow(clippy::too_many_arguments)]
    fn ch(scn: u64, ts: DateTime<Utc>, ty: char, c: &str, seq: &str, y: Option<&str>, ys: &str, prog: &str) -> Change {
        Change {
            scn,
            ts,
            ty,
            key: key(c, seq),
            ytno: y.map(str::to_string),
            ys: ys.into(),
            jobtype: Some("DS".into()),
            program: Some(prog.into()),
        }
    }
    /// 앵커: 시각 tm(ms), 커밋 번호 상한 100.
    /// 앵커: 시각 tm(ms), 커밋 번호 상한 100, 앵커가 본 로그온 없음(모든 로그온 값이 새것).
    fn anc(ms: i64, state: State) -> Anchor {
        Anchor { as_of: tm(ms), state, scn: 100, last_ts: tm(ms), logons: Some(Logons::default()) }
    }
    fn snap(as_of: DateTime<Utc>, state: State) -> Snapshot {
        Snapshot { as_of, state, last_on: HashMap::new(), last_off: HashMap::new() }
    }
    fn found<'a>(r: &'a Replay, y: &str, enter: bool) -> Vec<&'a Transition> {
        r.transitions.iter().filter(|t| t.ytno == y && t.enter == enter).collect()
    }
    fn st(m: &[(&str, &str)]) -> State {
        State { machines: mach(m), ..Default::default() }
    }

    #[test]
    fn free_set_matches_getmoverlist_rule() {
        let mut s = st(&[("T1", "O"), ("T2", "O"), ("T3", "F"), ("T4", "O"), ("T5", "O")]);
        s.rows.insert(key("C1", "1"), row("T1", "X")); // NULL YT_STATUS 는 바쁨
        s.rows.insert(key("C2", "1"), row("T2", "C")); // 'C' 는 풀림
        s.stopped.insert("T4".into());
        s.rows.insert(key("C5", "1"), row("T5", "F")); // 싣고 가는 중(F)도 바쁨
        assert_eq!(s.free_set(), ["T2".to_string()].into_iter().collect());
    }

    #[test]
    fn release_then_dispatch_in_separate_commits_are_both_kept() {
        let mut a = st(&[("T1", "O")]);
        a.rows.insert(key("C1", "1"), row("T1", "A"));
        let changes = vec![
            ch(101, t(10), 'U', "C1", "1", Some("T1"), "C", "completeQuayOrderForLoading"),
            ch(102, t(17), 'U', "C2", "1", Some("T1"), "X", "NEW_ITV_DISPATCHING"),
        ];
        let mut end = st(&[("T1", "O")]);
        end.rows.insert(key("C2", "1"), row("T1", "X"));
        let r = replay(&anc(500, a), &changes, &snap(t(60), end));
        let en = found(&r, "T1", true);
        let ex = found(&r, "T1", false);
        assert_eq!((en.len(), en[0].ts, en[0].cause), (1, t(10), "release"));
        assert_eq!(en[0].program.as_deref(), Some("completeQuayOrderForLoading"));
        assert_eq!((ex[0].ts, ex[0].cause, ex[0].program.as_deref()), (t(17), "dispatch", Some("NEW_ITV_DISPATCHING")));
        assert_eq!(ex[0].contno.as_deref(), Some("C2"));
        assert_eq!((r.n_txn, r.n_events, r.scn), (2, 2, 102));
    }

    #[test]
    fn a_short_stay_across_commits_is_real() {
        // 커밋이 다르면 0.3초라도 진짜 머무름이다(한때 1초 문턱이 이런 것을 지웠다).
        let mut a = st(&[("T1", "O")]);
        a.rows.insert(key("C1", "1"), row("T1", "A"));
        let changes = vec![
            ch(101, t(10), 'U', "C1", "1", Some("T1"), "C", "x"),
            ch(102, t(10) + Duration::milliseconds(300), 'U', "C2", "1", Some("T1"), "X", "NEW_ITV_DISPATCHING"),
        ];
        let mut end = st(&[("T1", "O")]);
        end.rows.insert(key("C2", "1"), row("T1", "X"));
        let r = replay(&anc(0, a), &changes, &snap(t(60), end));
        assert_eq!(found(&r, "T1", true).len(), 1);
        assert_eq!(found(&r, "T1", false).len(), 1);
    }

    #[test]
    fn an_exchange_inside_one_commit_is_not_a_visit() {
        // 교환: T1 의 작업이 T2 로 가고(+0ms) T1 이 새 작업을 받는다(+17ms) — 같은 커밋이라 중간 상태는 보인 적이 없다.
        let mut a = st(&[("T1", "O"), ("T2", "O")]);
        a.rows.insert(key("C1", "1"), row("T1", "X"));
        a.rows.insert(key("C2", "1"), row("T2", "X"));
        let changes = vec![
            ch(101, t(10), 'U', "C1", "1", Some("T2"), "X", "Swap4ManualService.executeItvExchange"),
            ch(101, t(10) + Duration::milliseconds(17), 'U', "C2", "1", Some("T1"), "X", "Swap4ManualService.executeItvExchange"),
        ];
        let mut end = a.clone();
        end.rows.insert(key("C1", "1"), row("T2", "X"));
        end.rows.insert(key("C2", "1"), row("T1", "X"));
        let r = replay(&anc(0, a), &changes, &snap(t(60), end));
        assert!(r.transitions.is_empty(), "{:?}", r.transitions);
        assert_eq!(r.n_txn, 1);
    }

    #[test]
    fn already_committed_before_the_anchor_is_skipped() {
        // 커밋 번호가 앵커 상한 이하 = 이미 앵커에 반영. 뒤따른 변경이 기록에 없더라도 옛 기록이 앵커를 되돌리면
        // 안 된다(2026-09-30 TT1363·TT689 헛 등재).
        let mut a = st(&[("T1", "O")]);
        a.rows.insert(key("C1", "1"), row("T1", "A"));
        let old = ch(99, t(-20), 'U', "C1", "1", Some("T1"), "C", "x");
        let edge = ch(100, t(-5), 'U', "C1", "1", Some("T1"), "C", "x");
        let r = replay(&anc(0, a.clone()), &[old, edge], &snap(t(60), a));
        assert!(r.transitions.is_empty(), "{:?}", r.transitions);
        assert_eq!((r.n_events, r.n_txn), (0, 0));
    }

    #[test]
    fn late_commit_is_stamped_just_after_the_anchor() {
        // 앵커 이전 시각에 찍혔지만 앵커 뒤에 커밋(번호 > 상한) — 앵커엔 없고 이번에 처음 보인다.
        let mut a = st(&[("T1", "O")]);
        a.rows.insert(key("C1", "1"), row("T1", "A"));
        let changes = vec![ch(101, t(-30), 'U', "C1", "1", Some("T1"), "C", "doneJobOrderForDischarge")];
        let r = replay(&anc(0, a), &changes, &snap(t(60), st(&[("T1", "O")])));
        let en = found(&r, "T1", true);
        assert_eq!(en[0].cause, "late");
        assert!(en[0].ts > tm(0) && en[0].ts < tm(1), "{}", en[0].ts);
        assert_eq!(en[0].program.as_deref(), Some("doneJobOrderForDischarge"));
        assert_eq!(r.n_late, 1);
    }

    #[test]
    fn commits_apply_in_commit_order_not_statement_time() {
        // 긴 트랜잭션: 커밋 102 의 기록은 10:00:02 에 찍혔지만 커밋 101(10:00:05)보다 늦게 끝났다.
        // 101: T1 풀림 / 102: T1 배차. 문장 시각 순이면 배차 → 풀림이 되어 T1 이 목록에 남는다.
        let mut a = st(&[("T1", "O")]);
        a.rows.insert(key("C1", "1"), row("T1", "A"));
        let changes = vec![
            ch(102, t(2), 'U', "C2", "1", Some("T1"), "X", "NEW_ITV_DISPATCHING"),
            ch(101, t(5), 'U', "C1", "1", Some("T1"), "C", "x"),
        ];
        let mut end = st(&[("T1", "O")]);
        end.rows.insert(key("C2", "1"), row("T1", "X"));
        let r = replay(&anc(0, a), &changes, &snap(t(60), end));
        let en = found(&r, "T1", true);
        let ex = found(&r, "T1", false);
        assert_eq!((en.len(), ex.len()), (1, 1));
        assert!(ex[0].ts > en[0].ts, "시각도 커밋 순서를 따라야 한다: {:?}", r.transitions);
        assert!(r.replay_free.is_empty());
        assert!(r.transitions.iter().all(|t| t.cause != "reconcile"));
    }

    #[test]
    fn second_busy_row_keeps_truck_off_the_list() {
        // 적재 운행 중에 다음 작업이 선배정(Q, YT_STATUS NULL)된 트럭 — 현재 작업이 끝나도 목록에 안 들어간다.
        let mut a = st(&[("T1", "O")]);
        a.rows.insert(key("C1", "1"), row("T1", "F"));
        a.rows.insert(key("C2", "1"), row("T1", "X"));
        let changes = vec![ch(101, t(10), 'U', "C1", "1", Some("T1"), "C", "doneJobOrderForDischarge")];
        let mut end = a.clone();
        end.rows.remove(&key("C1", "1"));
        let r = replay(&anc(0, a), &changes, &snap(t(60), end));
        assert!(r.transitions.is_empty(), "{:?}", r.transitions);
    }

    #[test]
    fn logout_and_login_are_their_own_causes() {
        let a = st(&[("T1", "O"), ("T2", "F"), ("T3", "F")]);
        let mut s = snap(t(60), st(&[("T1", "F"), ("T2", "O"), ("T3", "F")]));
        s.last_off.insert("T1".into(), t(20));
        s.last_on.insert("T2".into(), t(30));
        // 앵커가 이미 본 로그인 시각은 다시 적용되지 않는다 — 로그아웃한 T3 이 살아나면 안 된다.
        s.last_on.insert("T1".into(), t(-3600));
        s.last_on.insert("T3".into(), t(-7200));
        let mut an = anc(0, a);
        an.logons.as_mut().unwrap().last_on.insert("T1".into(), t(-3600));
        an.logons.as_mut().unwrap().last_on.insert("T3".into(), t(-7200));
        let r = replay(&an, &[], &s);
        let ex = found(&r, "T1", false);
        let en = found(&r, "T2", true);
        assert_eq!((ex[0].ts, ex[0].cause), (t(20), "logout"));
        assert_eq!((en[0].ts, en[0].cause), (t(30), "login"));
        assert_eq!(r.transitions.len(), 2);
    }

    #[test]
    fn logout_and_relogin_inside_one_tick_leaves_a_gap() {
        // 두 스냅샷 사이 로그아웃 → 재로그인(로그아웃의 22.9%). 두 스냅샷 모두 'O' 라 지금 상태만 보면 안 보인다.
        let a = st(&[("T1", "O")]);
        let mut s = snap(t(60), a.clone());
        s.last_off.insert("T1".into(), t(20));
        s.last_on.insert("T1".into(), t(40));
        let r = replay(&anc(0, a), &[], &s);
        let ex = found(&r, "T1", false);
        let en = found(&r, "T1", true);
        assert_eq!((ex.len(), ex[0].ts, ex[0].cause), (1, t(20), "logout"));
        assert_eq!((en.len(), en[0].ts, en[0].cause), (1, t(40), "login"));
    }

    #[test]
    fn logon_is_ordered_against_commits_by_time() {
        // 로그아웃(20초) 때 TOS 가 작업을 떼어낸다(커밋, 20.6초) → 재로그인(40초). 트럭은 40초에 목록에 들어간다.
        let mut a = st(&[("T1", "O")]);
        a.rows.insert(key("C1", "1"), row("T1", "A"));
        let changes = vec![ch(101, t(20) + Duration::milliseconds(660), 'U', "C1", "1", None, "X", "Event : USR_LOGIN_INFO")];
        let mut s = snap(t(60), st(&[("T1", "O")]));
        s.last_off.insert("T1".into(), t(20));
        s.last_on.insert("T1".into(), t(40));
        let r = replay(&anc(0, a), &changes, &s);
        let en = found(&r, "T1", true);
        assert_eq!((en.len(), en[0].ts, en[0].cause), (1, t(40), "login"));
        assert!(found(&r, "T1", false).is_empty(), "{:?}", r.transitions);
    }

    #[test]
    fn a_login_stamped_before_the_anchor_but_visible_after_is_applied() {
        // 로그인 시각(…−2초)이 앵커보다 앞이지만 앵커엔 아직 로그아웃 — 늦게 보인 로그인이다. 앵커 직후에 login 으로 붙는다.
        let mut s = snap(t(60), st(&[("T1", "O")]));
        s.last_on.insert("T1".into(), t(-2));
        let r = replay(&anc(500, st(&[("T1", "F")])), &[], &s);
        let en = found(&r, "T1", true);
        assert_eq!((en.len(), en[0].cause), (1, "login"));
        assert!(en[0].ts > tm(500) && en[0].ts < tm(501), "{}", en[0].ts);
        assert!(r.transitions.iter().all(|t| t.cause != "reconcile"));
    }

    #[test]
    fn a_login_already_seen_by_the_anchor_is_not_reapplied() {
        // 2026-10-01 TT1421: 로그인(−30초)은 앵커가 이미 봤고, 그 뒤 기록에 안 남는 로그아웃으로 앵커엔 F. 시간 창으로
        // 고르면 이 로그인을 다시 써서 1분 동안 헛 등재했다. 값이 같으면 새 사건이 아니다.
        let mut s = snap(t(60), st(&[("T1", "F")]));
        s.last_on.insert("T1".into(), t(-30));
        let mut an = anc(0, st(&[("T1", "F")]));
        an.logons.as_mut().unwrap().last_on.insert("T1".into(), t(-30));
        let r = replay(&an, &[], &s);
        assert!(r.transitions.is_empty(), "{:?}", r.transitions);
        assert_eq!(r.n_logon, 0);
    }

    #[test]
    fn a_legacy_anchor_without_logons_falls_back_to_the_anchor_second() {
        // 로그온 정보가 없던 옛 앵커: 앵커가 든 초 이후 시각만 새 로그온으로 본다(전부 새것으로 보면 몇 시간 전 로그인까지 되살린다).
        let mut s = snap(t(60), st(&[("T1", "F"), ("T2", "O")]));
        s.last_on.insert("T1".into(), t(-3600));
        s.last_on.insert("T2".into(), t(30));
        let mut an = anc(0, st(&[("T1", "F"), ("T2", "F")]));
        an.logons = None;
        let r = replay(&an, &[], &s);
        assert!(found(&r, "T1", true).is_empty());
        assert_eq!(found(&r, "T2", true)[0].cause, "login");
    }

    #[test]
    fn login_in_the_anchor_second_is_a_login_not_late() {
        // 로그인 시각은 초 단위라 앵커가 든 초의 로그인도 적용한다 — 실제로 바뀌었다면 원인은 login.
        let mut s = snap(t(60), st(&[("T1", "O")]));
        s.last_on.insert("T1".into(), t(0));
        let r = replay(&anc(500, st(&[("T1", "F")])), &[], &s);
        let en = found(&r, "T1", true);
        assert_eq!((en.len(), en[0].cause), (1, "login"));
        assert!(en[0].ts > tm(500));
    }

    #[test]
    fn a_relogin_that_changes_nothing_does_not_mask_a_late_release() {
        // 앵커 초 안의 로그인은 이미 앵커에 반영(로그인 상태 그대로). 같은 트럭이 늦게 커밋된 기록으로 풀렸다면 원인은 late.
        let mut a = st(&[("T1", "O")]);
        a.rows.insert(key("C1", "1"), row("T1", "A"));
        let c = ch(101, t(-10), 'U', "C1", "1", Some("T1"), "C", "doneJobOrderForDischarge");
        let mut s = snap(t(60), st(&[("T1", "O")]));
        s.last_on.insert("T1".into(), t(0));
        let r = replay(&anc(500, a), &[c], &s);
        let en = found(&r, "T1", true);
        assert_eq!((en.len(), en[0].cause, en[0].program.as_deref()), (1, "late", Some("doneJobOrderForDischarge")));
    }

    #[test]
    fn delete_and_reinsert_in_one_commit_keeps_the_new_row() {
        // 한 커밋에서 옛 행 수정·삭제 + 새 행 추가 — U → D → I 로 적용해야 새 행(바쁨)이 남는다.
        let mut a = st(&[("T1", "O")]);
        a.rows.insert(key("C1", "old"), row("T1", "A"));
        let changes = vec![
            ch(101, t(10), 'I', "C1", "new", Some("T1"), "X", "B2B"),
            ch(101, t(10), 'D', "C1", "old", Some("T1"), "A", "B2B"),
            ch(101, t(10), 'U', "C1", "old", Some("T1"), "C", "B2B"),
        ];
        let mut end = st(&[("T1", "O")]);
        end.rows.insert(key("C1", "new"), row("T1", "X"));
        let r = replay(&anc(0, a), &changes, &snap(t(60), end));
        assert!(r.transitions.is_empty(), "{:?}", r.transitions);
    }

    #[test]
    fn rows_sharing_contno_and_point_stay_separate() {
        // 같은 컨테이너·포인트에 진행 행(…22I, A)과 완료 행(…229, C) — 완료 행 기록이 진행 행을 지우면 안 된다.
        let mut a = st(&[("T1", "O")]);
        a.rows.insert(key("C1", "22I"), row("T1", "A"));
        let changes = vec![ch(101, t(5), 'U', "C1", "229", Some("T1"), "C", "DuplicatedLocation-ChangeLocationWhenYCDoneInYard")];
        let r = replay(&anc(0, a.clone()), &changes, &snap(t(60), a));
        assert!(r.transitions.is_empty(), "{:?}", r.transitions);
    }

    #[test]
    fn deleted_row_frees_the_truck_and_names_the_program() {
        let mut a = st(&[("T1", "O")]);
        a.rows.insert(key("C1", "1"), row("T1", "A"));
        let changes = vec![ch(101, t(12), 'D', "C1", "1", Some("T1"), "A", "cancelJob")];
        let r = replay(&anc(0, a), &changes, &snap(t(60), st(&[("T1", "O")])));
        let en = found(&r, "T1", true);
        assert_eq!((en[0].ts, en[0].program.as_deref()), (t(12), Some("cancelJob")));
    }

    #[test]
    fn swap_moves_busy_from_one_truck_to_another() {
        // 한 방향 재지향: 행의 트럭이 T1 → T2 로 바뀐다. T1 은 풀리고(원인 = 그 스왑) T2 는 바빠진다.
        let mut a = st(&[("T1", "O"), ("T2", "O")]);
        a.rows.insert(key("C1", "1"), row("T1", "X"));
        let changes = vec![ch(101, t(5), 'U', "C1", "1", Some("T2"), "X", "Swap4DischargingImpl.dischargeJobSwap")];
        let mut end = st(&[("T1", "O"), ("T2", "O")]);
        end.rows.insert(key("C1", "1"), row("T2", "X"));
        let r = replay(&anc(0, a), &changes, &snap(t(60), end));
        let en = found(&r, "T1", true);
        assert_eq!((en[0].ts, en[0].program.as_deref()), (t(5), Some("Swap4DischargingImpl.dischargeJobSwap")));
        assert_eq!(found(&r, "T2", false)[0].program.as_deref(), Some("Swap4DischargingImpl.dischargeJobSwap"));
    }

    #[test]
    fn reconcile_fixes_what_replay_missed() {
        // 기록에 안 남은 변경 — 스냅샷과 대조해 바로잡는다.
        let mut a = st(&[("T1", "O"), ("T2", "O")]);
        a.rows.insert(key("C1", "1"), row("T1", "A"));
        let mut end = st(&[("T1", "O"), ("T2", "O")]);
        end.rows.insert(key("C2", "1"), row("T2", "A"));
        let r = replay(&anc(0, a), &[], &snap(t(60), end));
        assert_eq!(found(&r, "T1", true)[0].cause, "reconcile");
        assert_eq!(found(&r, "T2", false)[0].cause, "reconcile");
        assert_eq!(r.replay_free, ["T2".to_string()].into_iter().collect());
        assert_eq!(r.truth_free, ["T1".to_string()].into_iter().collect());
    }

    #[test]
    fn transition_times_strictly_increase_across_steps_and_ticks() {
        // 늦은 커밋 둘이 모두 앵커 뒤로 밀려도 시각이 겹치지 않고, 직전 틱이 쓴 마지막 시각보다도 뒤다.
        let mut a = st(&[("T1", "O")]);
        a.rows.insert(key("C1", "1"), row("T1", "A"));
        let changes = vec![
            ch(101, t(-30), 'U', "C1", "1", Some("T1"), "C", "x"),
            ch(102, t(-20), 'U', "C2", "1", Some("T1"), "X", "NEW_ITV_DISPATCHING"),
        ];
        let mut end = st(&[("T1", "O")]);
        end.rows.insert(key("C2", "1"), row("T1", "X"));
        let mut an = anc(0, a);
        an.last_ts = tm(0) + Duration::microseconds(5); // 직전 틱 대조 시각이 스냅샷보다 약간 뒤였던 경우
        let r = replay(&an, &changes, &snap(t(60), end));
        let ts: Vec<_> = r.transitions.iter().map(|t| t.ts).collect();
        assert_eq!(ts.len(), 2);
        assert!(ts[0] > an.last_ts && ts[1] > ts[0], "{ts:?}");
        assert_eq!(r.last_ts, ts[1]);
    }

    #[test]
    fn hist_window_starts_before_the_anchor_by_the_overlap() {
        let a = tm(60_740);
        assert_eq!(hist_from(Some(a), a + Duration::seconds(60)), a - Duration::seconds(OVERLAP_S));
        let now = tm(1_000_000);
        assert_eq!(hist_from(None, now), now - Duration::seconds(OVERLAP_S + CLOCK_MARGIN_S));
        assert_eq!(to_myt_ms(a).len(), 17);
        assert_eq!(parse_myt(&to_myt_ms(a)), Some(a));
    }

    #[test]
    fn build_sql_passes_toolbox_rules() {
        // 조회 도구는 SQL 어디든(문자열 안 포함) 주석 기호나 쓰기 단어가 있으면 거부한다.
        let sql = build_sql("20260930141301000").to_uppercase();
        assert!(!sql.contains("--") && !sql.contains("/*"), "주석 기호");
        let words: HashSet<&str> = sql.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).collect();
        for w in ["INSERT", "UPDATE", "DELETE", "MERGE", "DROP", "ALTER", "CREATE", "TRUNCATE", "GRANT", "REVOKE",
                  "COMMIT", "ROLLBACK", "EXECUTE", "EXEC", "CALL", "LOCK", "RENAME", "BEGIN", "DECLARE"] {
            assert!(!words.contains(w), "쓰기 단어 {w}");
        }
    }

    #[test]
    fn anchor_json_round_trips() {
        let mut s = st(&[("T1", "O"), ("T2", "F")]);
        s.rows.insert(key("C1", "1"), row("T1", "X"));
        s.rows.insert(("C2".into(), "3".into(), "9".into()), JobRow { ytno: None, ys: "X".into() });
        s.stopped.insert("T2".into());
        let mut lg = Logons::default();
        lg.last_on.insert("T1".into(), tm(1000));
        lg.last_off.insert("T2".into(), tm(900));
        let a = Anchor { as_of: tm(1234), state: s, scn: 16_866_812_346_123, last_ts: tm(1235), logons: Some(lg) };
        assert_eq!(anchor_from_json(a.as_of, &anchor_to_json(&a).unwrap()).unwrap(), a);
        // 로그온 정보가 없던 옛 앵커도 읽힌다.
        let legacy = r#"{"rows":[],"machines":{},"stopped":[],"scn":5,"last_ts":"2026-10-01T00:00:00Z"}"#;
        assert_eq!(anchor_from_json(a.as_of, legacy).unwrap().logons, None);
    }

    fn filler() -> Vec<RawRow> {
        (0..MIN_MACHINES)
            .map(|n| RawRow {
                k: "M".into(),
                a: Some(format!("X{n}")),
                b: Some("YT".into()),
                c: Some("F".into()),
                d: None,
                e: None,
                f: None,
                g: None,
                h: None,
                i: None,
                j: None,
            })
            .collect()
    }

    #[test]
    fn parse_fetch_reads_every_part() {
        let raw = concat!(
            r#"{"result":"[{\"K\":\"T\",\"A\":\"20260930141330971\"},"#,
            r#"{\"K\":\"M\",\"A\":\"TT1 \",\"B\":\"YT\",\"C\":\"O\",\"D\":\"AAA\",\"E\":\"20260930141000\",\"F\":null},"#,
            r#"{\"K\":\"M\",\"A\":\"TT2\",\"B\":\"YT\",\"C\":\"F\",\"D\":\"AAA\",\"E\":\"20260930120000\",\"F\":\"20260930141200\"},"#,
            r#"{\"K\":\"B\",\"A\":\"CONT1\",\"B\":\"5\",\"C\":\"S1\",\"D\":\"TT1\",\"E\":\"X\"},"#,
            r#"{\"K\":\"W\",\"A\":\"TT3\",\"B\":\"BRK\"},"#,
            r#"{\"K\":\"H\",\"A\":\"20260930141301123\",\"B\":\"U\",\"C\":\"CONT1\",\"D\":\"5\",\"E\":\"S1\",\"F\":\"TT1\",\"G\":\"X\",\"H\":\"DS\",\"I\":\"NEW_ITV_DISPATCHING\",\"J\":\"16866812346123\"},"#,
            r#"{\"K\":\"H\",\"A\":\"20260930141302000\",\"B\":\"U\",\"C\":\"CONT9\",\"D\":\"1\",\"E\":\"S9\",\"F\":\"TT9\",\"G\":\"X\",\"J\":null},"#,
            r#"{\"K\":\"H\",\"A\":\"2026-09-30 14:13\",\"B\":\"U\",\"C\":\"CONT8\",\"D\":\"1\",\"E\":\"S8\",\"J\":\"5\"}]"}"#
        );
        let mut rows: Vec<RawRow> = parse_rows(raw).unwrap();
        rows.extend(filler());
        let (s, ch) = parse_fetch(rows).unwrap();
        assert_eq!(s.as_of, Utc.with_ymd_and_hms(2026, 9, 30, 6, 13, 30).unwrap() + Duration::milliseconds(971));
        assert!(s.state.machines.contains_key("TT1"), "코드는 공백을 걷어낸다");
        assert_eq!(s.last_off.get("TT2"), parse_myt("20260930141200").as_ref());
        assert_eq!(s.state.rows[&("CONT1".to_string(), "5".to_string(), "S1".to_string())].ytno.as_deref(), Some("TT1"));
        assert!(s.state.stopped.contains("TT3"));
        assert_eq!(ch.len(), 1, "커밋 번호·시각을 읽지 못한 기록은 버린다");
        assert_eq!((ch[0].ty, ch[0].program.as_deref(), ch[0].scn), ('U', Some("NEW_ITV_DISPATCHING"), 16_866_812_346_123));
        assert_eq!(ch[0].key, ("CONT1".to_string(), "5".to_string(), "S1".to_string()));
        assert_eq!(ch[0].ts, Utc.with_ymd_and_hms(2026, 9, 30, 6, 13, 1).unwrap() + Duration::milliseconds(123));
    }

    #[test]
    fn parse_fetch_refuses_a_thin_machine_list() {
        let raw = r#"{"result":"[{\"K\":\"T\",\"A\":\"20260930141330971\"},{\"K\":\"M\",\"A\":\"TT1\",\"C\":\"O\"}]"}"#;
        let rows: Vec<RawRow> = parse_rows(raw).unwrap();
        assert!(parse_fetch(rows).is_err());
    }

    #[test]
    fn parse_fetch_refuses_an_empty_busy_part_with_many_trucks_logged_in() {
        let raw = r#"{"result":"[{\"K\":\"T\",\"A\":\"20260930141330971\"}]"}"#;
        let mut rows: Vec<RawRow> = parse_rows(raw).unwrap();
        let mut on = filler();
        for r in on.iter_mut() {
            r.c = Some("O".into());
        }
        rows.extend(on);
        assert!(parse_fetch(rows).is_err());
        // 로그인한 트럭이 적으면(한산) 바쁜 작업지시 0줄도 받아들인다.
        let rows2: Vec<RawRow> = parse_rows(raw).unwrap().into_iter().chain(filler()).collect();
        assert!(parse_fetch(rows2).is_ok());
    }
}
