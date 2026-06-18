// TT operations page. The work pool (per-QC sequence + urgent job front) and the
// vehicle pool are LIVE: the work pool comes from /api/workpool (TOS JOB_QUEUE_SCHEDULE
// + JOB_ORDER_LIST, refreshed ~90s into Postgres) fused with /api/livemap/positions
// (websocket PLC = crane physically cycling, GPS = where the assigned TT actually is).
// Status distribution is live from the dispatch counts. Last Decision + Utilization
// remain visual mocks (future AI-dispatch panels).
import { useEffect, useState } from "react";
import { type Lang } from "./i18n";
import { api, type WorkpoolResponse, type WpQc, type WpMove } from "./api";

const ko = (lang: Lang) => lang === "ko";

// ── shared live sources ──
type Dev = {
  id: string; cls: string; speed?: number; age_s?: number;
  dispatch?: string; dispatch_reason?: string; arrival?: string; topos1?: string;
  plc?: { is_loaded: boolean; age_s: number; mph?: number; last_move_age_s?: number };
};
type Snap = {
  connected: boolean; as_of: string | null; dispatch_counts?: Record<string, number>;
  crane_mph_live?: number | null; crane_moves_60m?: number; cranes_working?: number;
  devices: Dev[];
};

function usePositions(ms = 3000) {
  const [snap, setSnap] = useState<Snap | null>(null);
  const [err, setErr] = useState(false);
  useEffect(() => {
    let alive = true;
    const poll = async () => {
      try {
        const r = await fetch("/api/livemap/positions");
        if (!r.ok) throw new Error();
        const j: Snap = await r.json();
        if (alive) { setSnap(j); setErr(false); }
      } catch { if (alive) setErr(true); }
    };
    poll();
    const iv = setInterval(poll, ms);
    return () => { alive = false; clearInterval(iv); };
  }, [ms]);
  return { snap, err };
}

function useWorkpool(ms = 15000) {
  const [data, setData] = useState<WorkpoolResponse | null>(null);
  const [err, setErr] = useState(false);
  useEffect(() => {
    let alive = true;
    const poll = () => api.workpool().then((d) => { if (alive) { setData(d); setErr(false); } }).catch(() => { if (alive) setErr(true); });
    poll();
    const iv = setInterval(poll, ms);
    return () => { alive = false; clearInterval(iv); };
  }, [ms]);
  return { data, err };
}

// dispatch-state colors (shared with the live map / vehicle pool)
const DSP_META: Record<string, { ko: string; en: string; color: string }> = {
  idle: { ko: "유휴 (배차 가능)", en: "Idle (available)", color: "#22c55e" },
  staging: { ko: "배차·대기", en: "Assigned·staging", color: "#0ea5e9" },
  soon_idle: { ko: "곧유휴·임박", en: "Imminent", color: "#f59e0b" },
  approaching: { ko: "접근·적재됨", en: "Approaching", color: "#fcd34d" },
  delivering: { ko: "적재 이동", en: "Delivering", color: "#64748b" },
  wait_rtg: { ko: "도착·RTG 대기", en: "Arrived·wait RTG", color: "#ef4444" },
  empty_travel: { ko: "공차 주행 중", en: "Empty traveling", color: "#94a3b8" },
};

// ETW countdown from the accurate TOS ETW RPC (qc_etw_utc via the tos_etw_gateway). The
// snapshot has a TTL (expires); past it, the value is stale and shown dimmed.
// Relative duration in ETW's format: "5h20" (≥1h), "23:45" (≥1m, mm:ss), "8s" (<1m).
function fmtRel(sec: number): string {
  const abs = Math.abs(sec);
  const hh = Math.floor(abs / 3600), mm = Math.floor((abs % 3600) / 60);
  return hh > 0 ? `${hh}h${String(mm).padStart(2, "0")}` : (mm > 0 ? `${mm}:${String(abs % 60).padStart(2, "0")}` : `${abs}s`);
}

function etwLabel(etw: string | null | undefined, expires: string | null | undefined, lang: Lang): { text: string; cls: string } | null {
  if (!etw) return null;
  const sec = Math.round((Date.parse(etw) - Date.now()) / 1000);
  const stale = expires != null && Date.parse(expires) < Date.now();
  const t = fmtRel(sec);
  if (stale) return { text: ko(lang) ? `${t} (만료)` : `${t} (stale)`, cls: "lo" };
  if (sec < -30) return { text: ko(lang) ? `지연 ${t}` : `overdue ${t}`, cls: "bad" };
  if (sec < 90) return { text: ko(lang) ? `곧 ${t}` : t, cls: "bad" };
  if (sec < 600) return { text: t, cls: "warn" };
  return { text: t, cls: "ok" };
}

const kindChip = (jt: string | null) => (jt === "DS" ? "dsc" : jt === "LD" ? "lod" : "shf");
const kindLabel = (jt: string | null) => (jt === "DS" ? "DSC" : jt === "LD" ? "LOD" : "SHF");

// ───────────────────────── live vehicle pool ─────────────────────────
type LiveTT = { id: string; cls: string; dispatch?: string; jobtype?: string; topos1?: string; dispatch_reason?: string; swappable?: boolean; dest_remaining_m?: number; nearest_rtg_m?: number; free_in_s?: number; free_in_hi_s?: number };

// "곧 빔 ~N분 (최대 M분)" — shadow time-to-free from the backend (free_in_s), display-only.
// Grounded in tt_cycle_v2 (도착→자유 중앙 8분, 운반중 17분); the p90 width conveys the RTG-wait tail.
function freeInLabel(d: LiveTT, lang: Lang): string | null {
  if (d.free_in_s == null) return null;
  const m = Math.round(d.free_in_s / 60);
  const hi = d.free_in_hi_s != null ? Math.round(d.free_in_hi_s / 60) : null;
  return ko(lang) ? `~${m}분${hi ? ` (최대 ${hi})` : ""}` : `~${m}m${hi ? ` (max ${hi})` : ""}`;
}

// localized "why" for a soon-idle TT (built from structured fields, not the
// backend's Korean dispatch_reason — so EN mode shows no Korean).
function soonWhy(d: LiveTT, lang: Lang): string {
  if (d.dispatch === "approaching") {
    return ko(lang) ? "QC 양하 완료 · RTG 대기 (~12분 후 유휴)" : "QC discharged · waiting RTG (~12m to free)";
  }
  if (d.nearest_rtg_m != null) {
    const m = Math.round(d.nearest_rtg_m);
    return ko(lang) ? `블록 RTG 근접 ${m}m` : `block RTG ${m}m`;
  }
  return ko(lang) ? "안벽 핸드오버 · PLC" : "quay handover · PLC";
}
// localized dispatch-state label for tooltips
function dspTitle(dispatch: string | undefined, lang: Lang): string | undefined {
  if (!dispatch || !DSP_META[dispatch]) return undefined;
  return ko(lang) ? DSP_META[dispatch].ko : DSP_META[dispatch].en;
}

// where the assigned truck physically is / what it's doing now — phrased per job direction
// (LD picks up at a block and drops at the QC; DS receives at the QC and drops at the yard).
function ttWhere(tt: Dev | undefined, jobtype: string | null, lang: Lang): string | null {
  if (!tt?.dispatch) return null;
  const k = ko(lang), d = tt.dispatch, arrived = tt.arrival === "ARRIVED";
  if (d === "soon_idle" || d === "approaching" || (arrived && (d === "delivering" || d === "staging"))) {
    return jobtype === "LD" ? (k ? "QC 밑·적재 중" : "at QC · loading") : (k ? "야드 도착·RTG 인계" : "at yard · RTG handover");
  }
  if (d === "wait_rtg") return k ? "야드 도착·RTG 대기" : "at yard · waiting RTG";
  if (d === "delivering") return jobtype === "LD" ? (k ? "블록→QC 운반 중" : "block→QC carrying") : (k ? "QC→야드 운반 중" : "QC→yard carrying");
  if (d === "empty_travel") return jobtype === "LD" ? (k ? "블록으로 공차 이동" : "→ block (empty)") : (k ? "QC로 공차 이동" : "→ QC (empty)");
  if (d === "staging") return k ? "배차·대기" : "staging";
  if (d === "idle") return k ? "유휴" : "idle";
  return ko(lang) ? DSP_META[d]?.ko ?? null : DSP_META[d]?.en ?? null;
}

function LiveDispatchPool({ lang, snap, err }: { lang: Lang; snap: Snap | null; err: boolean }) {
  const tts = ((snap?.devices ?? []) as LiveTT[]).filter((d) => d.cls === "TT");
  const soon = tts.filter((d) => d.dispatch === "soon_idle").sort((a, b) => a.id.localeCompare(b.id)); // imminent only
  const idle = tts.filter((d) => d.dispatch === "idle").sort((a, b) => a.id.localeCompare(b.id));
  const empties = tts.filter((d) => d.dispatch === "empty_travel");
  // swap pool: empty trucks still far enough from their pickup, EXCLUDING yard moves (MI/MO)
  // — only vessel work (DS/LD) is swappable. Distance threshold is operator-adjustable.
  const [swapMinM, setSwapMinM] = useState(500);
  const isYardMove = (d: LiveTT) => ["MI", "MO"].includes((d.jobtype ?? "").toUpperCase());
  const swap = empties
    .filter((d) => !isYardMove(d) && (d.dest_remaining_m ?? 0) >= swapMinM)
    .sort((a, b) => (b.dest_remaining_m ?? 1e9) - (a.dest_remaining_m ?? 1e9));
  const swapExcluded = empties.filter((d) => !isYardMove(d)).length - swap.length;
  const ageS = snap?.as_of ? Math.max(0, Math.round((Date.now() - Date.parse(snap.as_of)) / 1000)) : null;

  return (
    <section className="tcard lvp">
      <div className="tcard-head">
        <h3>{ko(lang) ? "TT 배차 풀" : "Dispatch TT Pool"}
          <span className="h3-sub">{ko(lang) ? "websocket GPS/PLC · 차량(공급)" : "websocket GPS/PLC · vehicles (supply)"}</span></h3>
        <div className="head-sub">
          <span className={`pill ${snap?.connected ? "good" : "bad"}`}><span className="dot" />{snap?.connected ? "LIVE" : (err ? "OFF" : "…")}</span>
          <span className="muted">{ageS != null ? `⟳ ${ageS}s` : ""}</span>
        </div>
      </div>
      <div className="tcard-body">
        <div className="lvp-cols lvp-cols3">
          <div className="lvp-col">
            <div className="lvp-col-h"><span className="sw" style={{ background: DSP_META.idle.color }} />{ko(lang) ? "현재 유휴" : "Idle now"}<span className="lvp-cn">{idle.length}</span></div>
            <div className="lvp-sub">{ko(lang) ? "즉시 배차 가능" : "dispatchable now"}</div>
            <div className="lvp-chips">
              {idle.length === 0 && <div className="lvp-empty">{ko(lang) ? "없음" : "none"}</div>}
              {idle.slice(0, 48).map((d) => <span className="lvp-chip idle mono" key={d.id}>{d.id}</span>)}
              {idle.length > 48 && <span className="lvp-more">+{idle.length - 48}</span>}
            </div>
          </div>
          <div className="lvp-col">
            <div className="lvp-col-h"><span className="sw" style={{ background: DSP_META.soon_idle.color }} />{ko(lang) ? "곧 유휴 (임박)" : "Soon-idle"}<span className="lvp-cn">{soon.length}</span></div>
            <div className="lvp-sub">{ko(lang) ? "임박만 — RTG 물리 관여·~2분 (측정값)" : "imminent only — RTG engaged ~2m (measured)"}</div>
            <div className="lvp-list">
              {soon.length === 0 && <div className="lvp-empty">{ko(lang) ? "없음" : "none"}</div>}
              {soon.map((d) => (
                <div className="lvp-row" key={d.id}>
                  <span className="lvp-id mono">{d.id}</span>
                  {d.jobtype && <span className={`lvp-job type-${d.jobtype.toLowerCase()}`}>{d.jobtype}</span>}
                  {d.topos1 && <span className="lvp-dest mono">→{d.topos1}</span>}
                  {freeInLabel(d, lang) && <span className="lvp-freein" title={ko(lang) ? "곧 빔까지 추정(측정 중앙값·표시 전용, 배차 미연결)" : "estimated time-to-free (measured median · display-only, not yet wired to dispatch)"}>{freeInLabel(d, lang)}</span>}
                  <span className="lvp-why">{soonWhy(d, lang)}</span>
                </div>
              ))}
            </div>
          </div>
          <div className="lvp-col">
            <div className="lvp-col-h"><span className="sw" style={{ background: DSP_META.empty_travel.color }} />{ko(lang) ? "스왑 가능한 공차" : "Swappable empty"}<span className="lvp-cn">{swap.length}</span></div>
            <div className="lvp-sub">{ko(lang) ? `픽업까지 잔여 ≥${swapMinM}m · MI/MO 제외 · 기준미달 ${swapExcluded} 제외` : `≥${swapMinM}m left to pickup · MI/MO excluded · ${swapExcluded} below threshold`}</div>
            <div className="lvp-swapctl">
              <span className="lvp-swapctl-l">{ko(lang) ? "기준 거리" : "min dist"}</span>
              <input type="range" min={100} max={1500} step={50} value={swapMinM} onChange={(e) => setSwapMinM(Number(e.target.value))} />
              <span className="lvp-swapctl-v mono">{swapMinM}m</span>
            </div>
            <div className="lvp-list">
              {swap.length === 0 && <div className="lvp-empty">{ko(lang) ? "없음" : "none"}</div>}
              {swap.map((d) => (
                <div className="lvp-row" key={d.id}>
                  <span className="lvp-id mono">{d.id}</span>
                  {d.jobtype && <span className={`lvp-job type-${d.jobtype.toLowerCase()}`}>{d.jobtype}</span>}
                  {d.topos1 && <span className="lvp-dest mono">→{d.topos1}</span>}
                  <span className="lvp-why">{d.dest_remaining_m != null ? (ko(lang) ? `잔여 ${Math.round(d.dest_remaining_m)}m` : `${Math.round(d.dest_remaining_m)}m left`) : (ko(lang) ? "목적지 학습 중" : "dest learning")}</span>
                </div>
              ))}
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}

// ───────────────────────── live QC work sequence ─────────────────────────
// group QCs by vessel (a QC serves one vessel at a time); QCs sorted by number within each.
function groupByVessel<T>(items: T[], vesselOf: (t: T) => string, qcOf: (t: T) => string): { vessel: string; items: T[] }[] {
  const map = new Map<string, T[]>();
  for (const it of items) {
    const v = vesselOf(it) || "—";
    const arr = map.get(v);
    if (arr) arr.push(it); else map.set(v, [it]);
  }
  return [...map.entries()]
    .map(([vessel, list]) => ({ vessel, items: list.slice().sort((a, b) => qcOf(a).localeCompare(qcOf(b), undefined, { numeric: true })) }))
    .sort((a, b) => a.vessel.localeCompare(b.vessel));
}

function LiveQcSequence({ lang, wp, snap }: { lang: Lang; wp: WorkpoolResponse | null; snap: Snap | null }) {
  // "auto" (default) = all assigned work + the next 3 unassigned; a number = max containers per QC.
  const [maxN, setMaxN] = useState<number | "auto">("auto");
  const [showDl, setShowDl] = useState(true); // show the computed deadline overlay (shadow)
  // 1s ticker so relative times (ETW, deadline, slack) keep counting down between 15s data polls.
  const [, setTick] = useState(0);
  useEffect(() => { const iv = setInterval(() => setTick((t) => t + 1), 1000); return () => clearInterval(iv); }, []);
  // fuse: live crane PLC (cycling now + live move/hr) + per-TT dispatch state
  const ttState = new Map<string, Dev>();
  const craneFresh = new Map<string, boolean>();
  const craneMph = new Map<string, number>(); // websocket live move/hr per crane (PLC cycle count)
  for (const d of snap?.devices ?? []) {
    if (d.cls === "TT") ttState.set(d.id, d);
    else if (d.plc) {
      craneFresh.set(d.id, (d.plc.age_s ?? 999) <= 120);
      if (d.plc.mph != null && d.plc.mph > 0) craneMph.set(d.id, d.plc.mph);
    }
  }
  // QCs with any work (assigned or unassigned), grouped by vessel.
  const working = (wp?.qcs ?? []).filter((q) => q.moves.length > 0);
  const groups = groupByVessel(working, (q) => q.vessels[0] ?? "—", (q) => q.qc);
  const ageS = wp?.as_of ? Math.max(0, Math.round((Date.now() - Date.parse(wp.as_of)) / 1000)) : null;
  const fleetMph = snap?.crane_mph_live ?? null;

  return (
    <section className="tcard">
      <div className="tcard-head">
        <h3>{ko(lang) ? "QC 작업 현황" : "QC Work Status"}
          <span className="h3-sub">{ko(lang) ? "작업 순서 · 배차/미배차(후보) 통합 (TOS+PLC/GPS)" : "work sequence · assigned + unassigned (candidates), merged (TOS+PLC/GPS)"}</span></h3>
        <div className="head-sub">
          <span className="pill good">{ko(lang) ? "가동 QC" : "Working QC"} {working.length}</span>
          {fleetMph != null && (
            <span className="pill" style={{ borderColor: "#f59e0b", color: "#fbbf24", background: "rgba(245,158,11,0.10)" }}
              title={ko(lang) ? "websocket PLC 사이클로 계산한 실시간 QC 평균 처리량 (TOS K_MPH 교차검증)" : "live avg QC throughput from PLC cycles (cross-check for TOS K_MPH)"}>
              ⚡ {fleetMph.toFixed(0)} {ko(lang) ? "move/h (실시간)" : "mv/h live"}
            </span>
          )}
          <span className="muted">{ko(lang) ? `잔여 ${(wp?.total_remaining ?? 0).toLocaleString()} move` : `${(wp?.total_remaining ?? 0).toLocaleString()} moves left`}</span>
          <label className="qc-pastsel" title={ko(lang) ? "QC당 작업 표시 — 자동=배차된 작업 전부+미배차 다음 3개 / 숫자=최대 컨테이너 수" : "work shown per QC — auto = all assigned + next 3 unassigned / number = max containers"}>
            {ko(lang) ? "QC당" : "per QC"}
            <select value={maxN} onChange={(e) => setMaxN(e.target.value === "auto" ? "auto" : Number(e.target.value))}>
              <option value="auto">{ko(lang) ? "자동" : "auto"}</option>
              {[5, 10, 20, 30].map((n) => <option key={n} value={n}>{n}</option>)}
            </select>
          </label>
          <label className="qc-pastsel" title={ko(lang) ? "예측(작업 시점·출항·여유, 그림자) 표시 켜고 끄기" : "toggle the forecast overlay (work ETA·departure·slack, shadow)"}>
            <input type="checkbox" checked={showDl} onChange={(e) => setShowDl(e.target.checked)} /> {ko(lang) ? "예측" : "forecast"}
          </label>
          <span className="muted">{ageS != null ? `⟳ ${ageS}s` : ""}</span>
        </div>
      </div>
      <div className="tcard-body">
        {working.length === 0 && <div className="lvp-empty">{ko(lang) ? "가동 중인 QC 없음" : "no working QC"}</div>}
        {groups.map((g) => (
          <div className="qc-vgroup" key={g.vessel}>
            <div className="qc-vgroup-h"><span className="vsl">{g.vessel}</span><span className="qc-vgroup-n">{g.items.length} QC</span></div>
            <div className="qc-panel">
              {g.items.map((q) => <QcCol key={q.qc} q={q} lang={lang} ttState={ttState} working={craneFresh.get(q.qc) ?? false} mph={craneMph.get(q.qc)} maxN={maxN} showDl={showDl} asOf={wp?.as_of} />)}
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}

const agoMin = (ts?: string | null): number | null =>
  ts ? Math.max(0, Math.round((Date.now() - new Date(ts).getTime()) / 60000)) : null;

// Container-level QC work sequence. Order = bay sequence (JOB_QUE_SEQ → live_workqueue.seq), then
// within a bay: already-discharged → trucked (by ETW) → not-yet-dispatched. Shows [past N
// containers] → NOW → [future N containers]; N counts CONTAINERS (not bays). "future" is mostly the
// not-yet-dispatched (candidate) work. TOS has no per-container order within a bay, so within-bay
// unassigned order is by bay only. Label = Vessel(QC) ↔ Block(RTG).
function QcCol({ q, lang, ttState, working, mph, maxN, showDl, asOf }: { q: WpQc; lang: Lang; ttState: Map<string, Dev>; working: boolean; mph?: number; maxN: number | "auto"; showDl: boolean; asOf?: string | null }) {
  const k = ko(lang);
  const tot = q.queues.reduce((a, x) => a + x.total, 0);
  const done = q.queues.reduce((a, x) => a + x.done, 0);
  const pct = tot > 0 ? Math.round((done / tot) * 100) : 0;
  const discharged = (m: WpMove) => m.jobtype === "DS" && !!m.actv_ts; // QC already discharged it
  const assigned = (m: WpMove) => !!(m.ytno && m.ytno.trim());

  // order all of this QC's containers by bay sequence, then within-bay (done → trucked → unassigned)
  const seqByQ = new Map<string, number>();
  for (const b of q.queues) seqByQ.set(b.queuename, b.seq ?? 9999);
  const within = (m: WpMove) => (discharged(m) ? 0 : assigned(m) ? 1 : 2);
  const etwKey = (m: WpMove) => m.etw_accurate ?? m.etw_ts ?? "~";
  const all = q.moves.slice().sort((a, b) =>
    (seqByQ.get(a.queuename) ?? 9999) - (seqByQ.get(b.queuename) ?? 9999)
    || within(a) - within(b)
    || etwKey(a).localeCompare(etwKey(b))
    || (a.contno ?? "").localeCompare(b.contno ?? ""));
  // not-yet-done work (assigned-but-not-discharged + unassigned). "auto" = all assigned + next 3
  // unassigned; a number = first maxN containers.
  const notDone = all.filter((m) => !discharged(m));
  let shown: WpMove[];
  let shownMore: number;
  if (maxN === "auto") {
    const asg = notDone.filter(assigned);
    const un = notDone.filter((m) => !assigned(m));
    shown = [...asg, ...un.slice(0, 3)];
    shownMore = Math.max(0, un.length - 3);
  } else {
    shown = notDone.slice(0, Math.max(1, maxN));
    shownMore = notDone.length - shown.length;
  }
  // trucks assigned to this QC and not finished — DISTINCT trucks (ytno), excluding DS trucks
  // already discharged (received + leaving = done with the QC). Distinct ⇒ twin-lift (1 truck, 2
  // container moves) counts once. Same definition as the top "Trucks Assigned per QC" card → match.
  const truckedSet = new Set<string>();
  for (const m of q.moves) if (assigned(m) && !discharged(m)) truckedSet.add((m.ytno as string).trim());
  const trucked = truckedSet.size;

  // SHADOW deadline distribution: per-QC slack (will it finish by departure?) + per-bay deadline.
  const fmtSlack = (sec: number) => { const a = Math.abs(Math.round(sec / 60)); const t = a >= 60 ? `${Math.floor(a / 60)}시간 ${a % 60}분` : `${a}분`; return (sec < 0 ? "−" : "+") + t; };
  const clock = (ts?: string | null) => ts ? new Date(ts).toLocaleTimeString(k ? "ko-KR" : "en-US", { hour: "2-digit", minute: "2-digit", hour12: false }) : null;
  // remaining time as an unambiguous DURATION (not a clock): "5시간 42분", "23분", "45초", "지연 4분".
  const relDur = (sec: number) => {
    const neg = sec < 0; const a = Math.abs(Math.round(sec));
    const h = Math.floor(a / 3600), mn = Math.floor((a % 3600) / 60);
    const t = h > 0 ? `${h}${k ? "시간 " : "h "}${mn}${k ? "분" : "m"}` : a >= 60 ? `${mn}${k ? "분" : "m"}` : `${a}${k ? "초" : "s"}`;
    return (neg ? (k ? "지연 " : "late ") : "") + t;
  };
  const dlByQueue = new Map<string, string>();
  for (const b of q.queues) if (b.deadline_ts) dlByQueue.set(b.queuename, b.deadline_ts);
  // per-MOVE deadline: within a bay the moves are worked back-to-back, so each one's deadline is the
  // bay deadline minus the work still after it in that bay (move-level distribution, not one shared
  // bay deadline). Walk in work order (notDone) so the first move of a bay gets the earliest.
  const remByQueue = new Map<string, number>();
  for (const b of q.queues) remByQueue.set(b.queuename, b.remaining);
  const mkey = (m: WpMove) => `${m.queuename}-${m.contno ?? ""}-${m.ytno ?? "u"}`;
  const dlByMove = new Map<string, string>();
  {
    const bayIdx = new Map<string, number>();
    for (const m of notDone) {
      const i = bayIdx.get(m.queuename) ?? 0;
      bayIdx.set(m.queuename, i + 1);
      const dlBay = dlByQueue.get(m.queuename);
      if (!dlBay) continue;
      const rem = remByQueue.get(m.queuename) ?? 1;
      const mt = (m.jobtype === "LD" ? 110 : 90) * 1000; // per-move time (ms)
      const after = Math.max(0, rem - i - 1); // moves still after this one in the bay
      dlByMove.set(mkey(m), new Date(new Date(dlBay).getTime() - after * mt).toISOString());
    }
  }

  const row = (m: WpMove, role: "past" | "now" | "future") => {
    const tt = assigned(m) ? ttState.get((m.ytno as string).trim()) : undefined;
    const dot = tt?.dispatch ? DSP_META[tt.dispatch]?.color : undefined;
    const ago = agoMin(m.actv_ts);
    const seqTxt = role === "now" ? "NOW" : role === "past" ? (ago != null ? (k ? `${ago}분전` : `${ago}m`) : "✓") : "▸";
    return (
      <div className={`qc-task ${role === "past" ? "past" : role === "now" ? "now" : "queued"}`} key={`${m.queuename}-${m.contno ?? ""}-${m.ytno ?? "u"}`}>
        <span className="seq">{seqTxt}</span>
        <div className="body">
          <div className="top">
            <span className={`type-${kindChip(m.jobtype)}`}>{kindLabel(m.jobtype)}</span> {m.contno ?? "—"}{m.twintandem ? ` · ${m.twintandem}` : ""}
          </div>
          <div className="bot">
            {(() => { const e = etwLabel(m.etw_accurate, m.etw_expires, lang); return e && role !== "past" && <span className={`jetw ${e.cls}`} title={k ? "TOS 작업예정(ETW) — 크레인이 이 컨테이너를 작업할 예정 시각" : "TOS ETW — when the crane is scheduled to work this"}>⏱ {e.text}</span>; })()}
            {showDl && (() => {
              const dl = dlByMove.get(mkey(m));
              if (!dl || q.work_left_s == null || !q.estdep_ts || !asOf) return null;
              // when the QC will actually work this container = deadline − slack. As an absolute time:
              //   work-ETA = as_of + (work_left − work_after) = as_of + work_left − estdep + deadline.
              const etaMs = new Date(asOf).getTime() + q.work_left_s * 1000 - new Date(q.estdep_ts).getTime() + new Date(dl).getTime();
              const sec = Math.round((etaMs - Date.now()) / 1000);
              const lead = (m.jobtype === "LD" ? 20 : 5) * 60; // truck must arrive by work time − lead
              const cls = sec < lead ? "late" : sec < lead + 1800 ? "soon" : "";
              return <span className={`qc-dl mono ${cls}`} title={k ? "이 컨테이너가 실제로 작업될 때까지 남은 시간 (내 처리속도 추정·그림자). 빨강=지금 트럭 보내야 함" : "time until the QC actually works this (my estimate, shadow); red = dispatch now"}>⏳ {k ? "작업까지 " : "work in "}{relDur(sec)}</span>;
            })()}
            {role === "past" && m.actv_ts && <span className="jetw rtg-actv" title={k ? "TOS ACTV — QC 양하 완료(트럭 적재). 검증 ACTV==QC move 완료 0초(n=3464)." : "TOS ACTV — QC discharged onto the truck (verified, n=3464)."}>{k ? "양하완료" : "discharged"}</span>}
          </div>
        </div>
        <div className="assign">
          {assigned(m) ? <span className="tt" title={dspTitle(tt?.dispatch, lang)}>{dot && <span className="dot" style={{ background: dot, marginRight: 4 }} />}{m.ytno}</span> : <span className="tt-none">{k ? "미배차" : "Unassigned"}</span>}
          {(() => { const w = ttWhere(tt, m.jobtype, lang); return w && <span className="tt-status" style={{ color: tt?.dispatch ? DSP_META[tt.dispatch]?.color : undefined }}>{w}</span>; })()}
        </div>
      </div>
    );
  };

  return (
    <div className="qc-col">
      <div className="qc-head">
        <span className={`id ${working ? "busy" : "idle"}`}><span className="dot" />{q.qc}
          <span className="qc-vessel">{q.vessels.join(" · ") || "—"}</span></span>
        {mph != null
          ? <span className="mph" title={k ? "PLC 실시간 처리량 (최근 1시간 move)" : "live throughput from PLC (moves in last hour)"}>⚡<span className="v">{mph}</span>/h</span>
          : <span className="mph">{k ? "잔여" : "rem"} <span className="v">{q.remaining}</span></span>}
      </div>
      <div className="qc-progress"><span>{trucked} {k ? "배차중" : "trucked"}{working ? (k ? " · PLC 가동" : " · PLC live") : ""}</span><span className="mono">{done.toLocaleString()} / {tot.toLocaleString()}</span></div>
      <div className="qc-progress-bar"><div className="fill" style={{ width: `${pct}%` }} /></div>
      {showDl && q.estdep_ts && (() => {
        const depSec = (new Date(q.estdep_ts).getTime() - Date.now()) / 1000; // time until departure
        const slackSec = q.work_left_s != null ? depSec - q.work_left_s : null; // recomputed each tick
        return (
          <div className="qc-deadline" title={k ? "출항 예정시각 (괄호=지금부터 남은 시간) / 여유 = 출항까지 − 남은 작업 (그림자·추정)" : "departure time (paren = time left) / slack = to-departure − remaining work (shadow)"}>
            🏁 {k ? "출항" : "dep"} <span className="mono">{clock(q.estdep_ts)}</span> <span className="qc-dep-left">({relDur(depSec)} {k ? "남음" : "left"})</span>
            {slackSec != null && <span className={`qc-slack ${slackSec < 0 ? "late" : slackSec < 1800 ? "tight" : "ok"}`}>{k ? "여유" : "slack"} {fmtSlack(slackSec)}</span>}
          </div>
        );
      })()}

      <div className="qc-seqlabel">{k ? "작업 (컨테이너)" : "work (containers)"}</div>
      {shown.length === 0 && <div className="lvp-empty" style={{ padding: "8px 0" }}>{k ? "대기 작업 없음" : "no pending work"}</div>}
      {(() => {
        // group contiguous containers by queue: show the queue number once as a divider, not per row
        const out: JSX.Element[] = [];
        let prevQ: string | null = null;
        shown.forEach((m, i) => {
          if (m.queuename !== prevQ) {
            prevQ = m.queuename;
            const dl = m.jobtype === "DS" ? (k ? "양하" : "DSC") : m.jobtype === "LD" ? (k ? "적하" : "LOD") : "";
            out.push(
              <div className="qc-qdiv" key={`d-${m.queuename}-${i}`}>
                <span className="mono">{m.queuename || "—"}</span>{dl && <span className="qc-qdiv-t">{dl}</span>}
              </div>
            );
          }
          out.push(row(m, i === 0 ? "now" : "future"));
        });
        return out;
      })()}
      {shownMore > 0 && <div className="qc-bay-more">+{shownMore} {k ? "컨테이너 더 (대부분 미배차)" : "more containers (mostly unassigned)"}</div>}
    </div>
  );
}


// Per-QC live assignment: how many distinct trucks are currently assigned to each quay
// crane (from live_workpool — the DS/LD dispatch pool). Starvation (0–2 trucks) is colour-cued.
function qcAssignColor(n: number): string {
  if (n === 0) return "#ef4444";   // starved
  if (n <= 2) return "#f59e0b";    // thin
  return "#22c55e";                // healthy
}
function QcAssignedCard({ lang, wp }: { lang: Lang; wp: WorkpoolResponse | null }) {
  // Trucks assigned to each QC and NOT finished — distinct trucks (ytno), excluding DS trucks
  // already discharged (received the box and left = done with the QC). Distinct ⇒ twin-lift (1
  // truck = 2 container moves) counts once. SAME definition as the QC Work Status header → match.
  const qcs = (wp?.qcs ?? [])
    .map((q) => {
      const t = new Set<string>();
      for (const m of q.moves) {
        if (!m.ytno || !m.ytno.trim()) continue;
        if (m.jobtype === "DS" && m.actv_ts) continue; // discharged → truck left, done with QC
        t.add(m.ytno.trim());
      }
      return { qc: q.qc, count: t.size, moves: q.active_moves, vessel: q.vessels[0] ?? "" };
    })
    .filter((x) => x.moves > 0 || x.count > 0) // only working QCs (a 0 here = real starvation)
    .sort((a, b) => a.qc.localeCompare(b.qc, undefined, { numeric: true }));
  const totalTrucks = qcs.reduce((a, x) => a + x.count, 0);
  const starved = qcs.filter((x) => x.count === 0).length;
  const groups = groupByVessel(qcs, (x) => x.vessel || "—", (x) => x.qc);
  return (
    <section className="tcard">
      <div className="tcard-head">
        <h3>{ko(lang) ? "QC별 배차 현황" : "Trucks Assigned per QC"}
          <span className="h3-sub">{ko(lang) ? "각 QC에 배차된 트럭 수 (양하완료·이탈 트럭 제외 · 트윈 중복 제거)" : "trucks assigned to each quay crane — finished/twin de-duped"}</span></h3>
        <div className="head-sub">
          <span className="muted">{ko(lang) ? `가동 QC ${qcs.length} · 배차 ${totalTrucks}대` : `${qcs.length} QCs · ${totalTrucks} trucks`}</span>
          {starved > 0 && <span style={{ color: "#ef4444", marginLeft: 8 }}>{ko(lang) ? `· 굶주림 ${starved}` : `· ${starved} starved`}</span>}
        </div>
      </div>
      <div className="tcard-body">
        {qcs.length === 0 && <div className="lvp-empty">{ko(lang) ? "가동 중인 QC 없음" : "no active QC"}</div>}
        <div className="qca-cols">
          {groups.map((g) => {
            const vtrucks = g.items.reduce((a, x) => a + x.count, 0);
            return (
              <div className="qca-vgroup" key={g.vessel}>
                <div className="qc-vgroup-h"><span className="vsl">{g.vessel}</span><span className="qc-vgroup-n">{g.items.length} QC · {vtrucks}{ko(lang) ? "대" : ""}</span></div>
                <div className="qca-grid">
                  {g.items.map((x) => (
                    <div className="qca-cell" key={x.qc} title={`${x.qc} · ${x.vessel} · ${ko(lang) ? `작업 ${x.moves}건` : `${x.moves} moves`}`}>
                      <div className="qca-qc">{x.qc}</div>
                      <div className="qca-n" style={{ color: qcAssignColor(x.count) }}>{x.count}<small>{ko(lang) ? "대" : ""}</small></div>
                      <div className="qca-vsl">{ko(lang) ? `${x.moves}작업` : `${x.moves} mv`}</div>
                    </div>
                  ))}
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </section>
  );
}

export default function TtPage({ lang }: { lang: Lang }) {
  const { snap, err } = usePositions();
  const { data: wp } = useWorkpool();
  return (
    <div className="content tt-page tt-2col">
      <div className="tt-col tt-col-qc">
        <QcAssignedCard lang={lang} wp={wp} />
        <LiveQcSequence lang={lang} wp={wp} snap={snap} />
      </div>
      <div className="tt-col tt-col-tt">
        <LiveDispatchPool lang={lang} snap={snap} err={err} />
      </div>
    </div>
  );
}
