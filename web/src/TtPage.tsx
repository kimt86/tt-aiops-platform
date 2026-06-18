// TT operations page. The work pool (per-QC sequence + urgent job front) and the
// vehicle pool are LIVE: the work pool comes from /api/workpool (TOS JOB_QUEUE_SCHEDULE
// + JOB_ORDER_LIST, refreshed ~90s into Postgres) fused with /api/livemap/positions
// (websocket PLC = crane physically cycling, GPS = where the assigned TT actually is).
// Status distribution is live from the dispatch counts. Last Decision + Utilization
// remain visual mocks (future AI-dispatch panels).
import { useEffect, useState } from "react";
import { type Lang } from "./i18n";
import { api, type WorkpoolResponse, type WpQc, type WpCandidate, type WpMove } from "./api";

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
function etwLabel(etw: string | null | undefined, expires: string | null | undefined, lang: Lang): { text: string; cls: string } | null {
  if (!etw) return null;
  const sec = Math.round((Date.parse(etw) - Date.now()) / 1000);
  const stale = expires != null && Date.parse(expires) < Date.now();
  const abs = Math.abs(sec);
  const hh = Math.floor(abs / 3600), mm = Math.floor((abs % 3600) / 60);
  const t = hh > 0 ? `${hh}h${String(mm).padStart(2, "0")}` : (mm > 0 ? `${mm}:${String(abs % 60).padStart(2, "0")}` : `${abs}s`);
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
  const [pastN, setPastN] = useState(0); // how many COMPLETED bays to show before NOW (default 0 = none)
  const [futureN, setFutureN] = useState(5); // how many UPCOMING (not-yet-dispatched) bays to show after NOW
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
  // working QCs (active moves), grouped by vessel — same set/definition as the per-QC card.
  const working = (wp?.qcs ?? []).filter((q) => q.active_moves > 0);
  const groups = groupByVessel(working, (q) => q.vessels[0] ?? "—", (q) => q.qc);
  const ageS = wp?.as_of ? Math.max(0, Math.round((Date.now() - Date.parse(wp.as_of)) / 1000)) : null;
  const fleetMph = snap?.crane_mph_live ?? null;
  // unassigned demand (no truck yet) grouped per QC — shown as block/queue counts in each column
  const candByQc = new Map<string, WpCandidate[]>();
  for (const c of wp?.candidates ?? []) {
    if (!c.qc) continue;
    const arr = candByQc.get(c.qc);
    if (arr) arr.push(c); else candByQc.set(c.qc, [c]);
  }

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
          <label className="qc-pastsel" title={ko(lang) ? "NOW 앞에 보여줄 '완료된 베이' 수 (0=숨김)" : "how many COMPLETED bays to show before NOW (0 = none)"}>
            {ko(lang) ? "과거" : "past"}
            <select value={pastN} onChange={(e) => setPastN(Number(e.target.value))}>
              {[0, 3, 5, 10].map((n) => <option key={n} value={n}>{n}</option>)}
            </select>
          </label>
          <label className="qc-pastsel" title={ko(lang) ? "NOW 뒤에 보여줄 '앞으로 할(미배차 포함) 베이' 수" : "how many UPCOMING (incl. not-yet-dispatched) bays to show after NOW"}>
            {ko(lang) ? "미래" : "next"}
            <select value={futureN} onChange={(e) => setFutureN(Number(e.target.value))}>
              {[3, 5, 10, 20].map((n) => <option key={n} value={n}>{n}</option>)}
            </select>
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
              {g.items.map((q) => <QcCol key={q.qc} q={q} lang={lang} ttState={ttState} working={craneFresh.get(q.qc) ?? false} mph={craneMph.get(q.qc)} cands={candByQc.get(q.qc) ?? []} pastN={pastN} futureN={futureN} />)}
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}

// Location label, QC-centric: vessel side = `Vessel(QC)`, yard side = `Block(RTG)`.
// DS (discharge) physically goes vessel→block; LD (load) goes block→vessel.
function wpLoc(jobtype: string | null, vessel: string, qc: string, block: string, rtg: string): string {
  const b = `${block}(${rtg})`, v = `${vessel}(${qc})`;
  return jobtype === "DS" ? `${v} → ${b}` : `${b} → ${v}`;
}
const agoMin = (ts?: string | null): number | null =>
  ts ? Math.max(0, Math.round((Date.now() - new Date(ts).getTime()) / 60000)) : null;

// Bay-sequenced QC work card. The discharge/work order is the BAY sequence
// (JOB_QUEUE_SCHEDULE.JOB_QUE_SEQ → live_workqueue.seq), NOT a per-container order (TOS has none).
// Per QC, for its active vessel(s): [recently discharged N] → NOW (active bay) → upcoming bays in
// seq order. Inside a bay, assigned containers (with a truck) show individually; the rest = an
// unassigned count (per source block for LD). Label = Vessel(QC) ↔ Block(RTG).
function QcCol({ q, lang, ttState, working, mph, cands, pastN, futureN }: { q: WpQc; lang: Lang; ttState: Map<string, Dev>; working: boolean; mph?: number; cands: WpCandidate[]; pastN: number; futureN: number }) {
  const k = ko(lang);
  const tot = q.queues.reduce((a, x) => a + x.total, 0);
  const done = q.queues.reduce((a, x) => a + x.done, 0);
  const pct = tot > 0 ? Math.round((done / tot) * 100) : 0;
  const discharged = (m: WpMove) => m.jobtype === "DS" && !!m.actv_ts; // QC already discharged it

  // bays for the QC's active vessel(s), in work sequence (JOB_QUE_SEQ)
  const vset = new Set(q.vessels);
  const bays = q.queues
    .filter((b) => vset.size === 0 || vset.has(b.vessel))
    .slice().sort((a, b) => (a.seq ?? 9999) - (b.seq ?? 9999) || a.queuename.localeCompare(b.queuename));
  const movesByQ = new Map<string, WpMove[]>();
  for (const m of q.moves) { const a = movesByQ.get(m.queuename); if (a) a.push(m); else movesByQ.set(m.queuename, [m]); }
  const candByQ = new Map<string, WpCandidate[]>();
  for (const c of cands) { const a = candByQ.get(c.queuename); if (a) a.push(c); else candByQ.set(c.queuename, [c]); }
  const phaseOf = (b: { done: number; total: number }) => (b.done >= b.total && b.total > 0) ? "done" : b.done > 0 ? "active" : "upcoming";
  // completed bays before NOW (last `pastN`, most recent), then the active + upcoming bays
  const doneAll = bays.filter((b) => phaseOf(b) === "done");
  const doneBays = pastN > 0 ? doneAll.slice(-pastN) : [];
  const activeBays = bays.filter((b) => phaseOf(b) === "active");
  // upcoming = not-yet-started bays (mostly NOT-yet-dispatched work = the "candidate" jobs);
  // capped to futureN so the card shows the next N work chunks the QC still has to do.
  const upcomingBays = bays.filter((b) => phaseOf(b) === "upcoming").slice(0, Math.max(0, futureN));
  const upcomingTotal = bays.filter((b) => phaseOf(b) === "upcoming").length;
  const shownBays = [...doneBays, ...activeBays, ...upcomingBays];
  // The current work FRONT per disload = the lowest-seq active queue (one for discharge, one for
  // load — a QC can dual-cycle). "active = partially done" alone over-marks: twin/dual queues for
  // the same bay and paused-partial queues all look active. So only the front(s) get NOW; the rest
  // of the partials show as 진행/WIP.
  const frontByDisload = new Map<string, typeof bays[number]>();
  for (const b of activeBays) {
    const dl = b.disload ?? "?";
    const cur = frontByDisload.get(dl);
    if (!cur || (b.seq ?? 9999) < (cur.seq ?? 9999)) frontByDisload.set(dl, b);
  }
  const nowKeys = new Set([...frontByDisload.values()].map((b) => `${b.queuename}|${b.seq}`));
  const isNow = (b: typeof bays[number]) => nowKeys.has(`${b.queuename}|${b.seq}`);

  const moveRow = (m: WpMove) => {
    const tt = m.ytno ? ttState.get(m.ytno) : undefined;
    const dot = tt?.dispatch ? DSP_META[tt.dispatch]?.color : undefined;
    const dim = discharged(m); // already discharged by the QC (truck carrying away)
    const ago = agoMin(m.actv_ts);
    return (
      <div className={`qc-task bay${dim ? " past" : ""}`} key={`m-${m.contno}-${m.ytno ?? ""}`}>
        <span className="seq">{dim ? (ago != null ? (k ? `${ago}분전` : `${ago}m`) : "✓") : "▸"}</span>
        <div className="body">
          <div className="top"><span className={`type-${kindChip(m.jobtype)}`}>{kindLabel(m.jobtype)}</span> {m.contno ?? "—"}{m.twintandem ? ` · ${m.twintandem}` : ""}</div>
          <div className="bot">{wpLoc(m.jobtype, m.vessel ?? "?", q.qc, m.yt_topos ?? "?", m.armgc ?? "RTG")}
            {(() => { const e = etwLabel(m.etw_accurate, m.etw_expires, lang); return e && !dim && <span className={`jetw ${e.cls}`} style={{ marginLeft: 6 }} title={k ? "TOS ETW RPC 기반 정확 ETW" : "accurate ETW from the TOS ETW RPC"}>ETW {e.text}</span>; })()}
            {dim && m.actv_ts && <span className="jetw rtg-actv" style={{ marginLeft: 6 }} title={k ? "TOS ACTV — QC 양하 완료(트럭 적재). 검증 ACTV==QC move 완료 0초(n=3464)." : "TOS ACTV — QC discharged onto the truck (verified, n=3464)."}>{k ? "양하완료" : "discharged"}</span>}
          </div>
        </div>
        <div className="assign">
          {m.ytno ? <span className="tt" title={dspTitle(tt?.dispatch, lang)}>{dot && <span className="dot" style={{ background: dot, marginRight: 4 }} />}{m.ytno}</span> : <span className="tt-none">{k ? "미배차" : "Unassigned"}</span>}
          {(() => { const w = ttWhere(tt, m.jobtype, lang); return w && <span className="tt-status" style={{ color: tt?.dispatch ? DSP_META[tt.dispatch]?.color : undefined }}>{w}</span>; })()}
        </div>
      </div>
    );
  };

  const bayBlock = (b: typeof bays[number]) => {
    const phase = phaseOf(b);
    const bmoves = (movesByQ.get(b.queuename) ?? []).slice()
      .sort((x, y) => (y.actv_ts ?? y.etw_accurate ?? y.etw_ts ?? "").localeCompare(x.actv_ts ?? x.etw_accurate ?? x.etw_ts ?? "")); // newest first (recent discharges on top)
    const bcands = candByQ.get(b.queuename) ?? [];
    const unassigned = bcands.reduce((a, c) => a + c.n, 0);
    const pctB = b.total > 0 ? Math.round((b.done / b.total) * 100) : 0;
    const isDS = b.disload === "D";
    const now = phase === "active" && isNow(b);
    const cls = phase === "done" ? "done" : now ? "active" : phase === "active" ? "wip" : "upcoming";
    const seqBadge = now ? "NOW" : phase === "done" ? "✓" : phase === "active" ? (k ? "진행" : "WIP") : `${b.seq ?? "·"}`;
    return (
      <div className={`qc-bay ${cls}`} key={`${b.vessel}-${b.queuename}-${b.seq}`}>
        <div className="qc-bay-h">
          <span className="bay-seq">{seqBadge}</span>
          <span className="bay-name">{b.queuename}</span>
          <span className={`type-${kindChip(isDS ? "DS" : "LD")}`}>{kindLabel(isDS ? "DS" : "LD")}</span>
          <span className="bay-prog mono">{b.done}/{b.total}</span>
        </div>
        <div className="qc-bay-bar"><div className="fill" style={{ width: `${pctB}%` }} /></div>
        {bmoves.slice(0, 4).map((m) => moveRow(m))}
        {bmoves.length > 4 && <div className="qc-bay-more">+{bmoves.length - 4} {k ? "더" : "more"}</div>}
        {unassigned > 0 && (
          <div className="qc-bay-cand">
            <span className="cand-n">×{unassigned}</span> {k ? "미배차" : "unassigned"}
            {!isDS && bcands.length > 0 && <span className="cand-blocks"> · {bcands.slice(0, 3).map((c) => `${c.src_block ?? "?"}(${c.rtg ?? "RTG"})`).join("  ")}{bcands.length > 3 ? " …" : ""}</span>}
          </div>
        )}
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
      <div className="qc-progress"><span>{q.active_moves} {k ? "작업중" : "active"}{working ? (k ? " · PLC 가동" : " · PLC live") : ""}</span><span className="mono">{done.toLocaleString()} / {tot.toLocaleString()}</span></div>
      <div className="qc-progress-bar"><div className="fill" style={{ width: `${pct}%` }} /></div>

      <div className="qc-seqlabel">{k ? "작업 순서 (베이)" : "work sequence (bays)"}</div>
      {shownBays.length === 0 && <div className="lvp-empty" style={{ padding: "8px 0" }}>{k ? "예정 베이 없음" : "no queued bay"}</div>}
      {shownBays.map(bayBlock)}
      {upcomingTotal > upcomingBays.length && <div className="qc-bay-more">+{upcomingTotal - upcomingBays.length} {k ? "베이 더 (미배차)" : "more bays (unassigned)"}</div>}
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
function QcAssignedCard({ lang, wp, snap }: { lang: Lang; wp: WorkpoolResponse | null; snap: Snap | null }) {
  // Trucks currently SERVING each QC, from live GPS: a truck whose destination (topos1) is this
  // crane and that is not idle — i.e. heading to or at the QC. Trucks that already finished their
  // QC bit (DS: discharged, now carrying to the yard → topos1 = a block; LD: loaded → gone/idle)
  // have moved their destination off the QC, so they drop out automatically (no double-count).
  const inbound = new Map<string, number>();
  for (const d of (snap?.devices ?? []) as LiveTT[]) {
    if (d.cls !== "TT" || !d.topos1 || !d.dispatch || d.dispatch === "idle") continue;
    inbound.set(d.topos1, (inbound.get(d.topos1) ?? 0) + 1);
  }
  const qcs = (wp?.qcs ?? [])
    .map((q) => ({ qc: q.qc, count: inbound.get(q.qc) ?? 0, moves: q.active_moves, vessel: q.vessels[0] ?? "" }))
    .filter((x) => x.moves > 0 || x.count > 0) // only working QCs (a 0 here = real starvation)
    .sort((a, b) => a.qc.localeCompare(b.qc, undefined, { numeric: true }));
  const totalTrucks = qcs.reduce((a, x) => a + x.count, 0);
  const starved = qcs.filter((x) => x.count === 0).length;
  const groups = groupByVessel(qcs, (x) => x.vessel || "—", (x) => x.qc);
  return (
    <section className="tcard">
      <div className="tcard-head">
        <h3>{ko(lang) ? "QC별 배차 현황" : "Trucks Assigned per QC"}
          <span className="h3-sub">{ko(lang) ? "각 QC로 향하는·있는 트럭 (작업 끝나 이탈한 트럭 제외 · 실시간 GPS)" : "trucks heading to / at each quay crane — finished trucks excluded (live GPS)"}</span></h3>
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
        <QcAssignedCard lang={lang} wp={wp} snap={snap} />
        <LiveQcSequence lang={lang} wp={wp} snap={snap} />
      </div>
      <div className="tt-col tt-col-tt">
        <LiveDispatchPool lang={lang} snap={snap} err={err} />
      </div>
    </div>
  );
}
