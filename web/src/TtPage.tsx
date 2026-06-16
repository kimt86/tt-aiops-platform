// TT operations page. The work pool (per-QC sequence + urgent job front) and the
// vehicle pool are LIVE: the work pool comes from /api/workpool (TOS JOB_QUEUE_SCHEDULE
// + JOB_ORDER_LIST, refreshed ~90s into Postgres) fused with /api/livemap/positions
// (websocket PLC = crane physically cycling, GPS = where the assigned TT actually is).
// Status distribution is live from the dispatch counts. Last Decision + Utilization
// remain visual mocks (future AI-dispatch panels).
import { useEffect, useState } from "react";
import { type Lang } from "./i18n";
import { api, type WorkpoolResponse, type WpQc, type WpCandidate } from "./api";

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
const DSP_ORDER = ["idle", "staging", "soon_idle", "approaching", "delivering", "wait_rtg", "empty_travel"];

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

function LiveDispatchPool({ lang, snap, err }: { lang: Lang; snap: Snap | null; err: boolean }) {
  const tts = ((snap?.devices ?? []) as LiveTT[]).filter((d) => d.cls === "TT");
  const counts = snap?.dispatch_counts ?? {};
  const tierRank = (s?: string) => (s === "soon_idle" ? 0 : 1); // 임박 먼저, 접근 다음
  const soon = tts.filter((d) => d.dispatch === "soon_idle" || d.dispatch === "approaching")
    .sort((a, b) => tierRank(a.dispatch) - tierRank(b.dispatch) || a.id.localeCompare(b.id));
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
        <div className="lvp-stats">
          {DSP_ORDER.map((k) => (
            <div className="lvp-stat" key={k} style={{ borderTopColor: DSP_META[k].color }}>
              <div className="lvp-n">{counts[k] ?? 0}</div>
              <div className="lvp-l">{ko(lang) ? DSP_META[k].ko : DSP_META[k].en}</div>
            </div>
          ))}
        </div>
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
            <div className="lvp-col-h"><span className="sw" style={{ background: DSP_META.soon_idle.color }} />{ko(lang) ? "곧 유휴" : "Soon-idle"}<span className="lvp-cn">{soon.length}</span></div>
            <div className="lvp-sub">{ko(lang) ? "임박(RTG 물리 관여·~2분) + 접근(블록 도착·~8분, 측정값)" : "imminent (RTG engaged ~2m) + approaching (at block ~8m, measured)"}</div>
            <div className="lvp-list">
              {soon.length === 0 && <div className="lvp-empty">{ko(lang) ? "없음" : "none"}</div>}
              {soon.map((d) => (
                <div className="lvp-row" key={d.id}>
                  {d.dispatch && DSP_META[d.dispatch] && <span className="lvp-tier" style={{ color: DSP_META[d.dispatch].color, borderColor: DSP_META[d.dispatch].color }}>{d.dispatch === "soon_idle" ? (ko(lang) ? "임박" : "now") : (ko(lang) ? "접근" : "~")}</span>}
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

  return (
    <section className="tcard">
      <div className="tcard-head">
        <h3>{ko(lang) ? "QC 작업 시퀀스 & 배차 (라이브)" : "QC Work Sequence & Dispatch (live)"}
          <span className="h3-sub">{ko(lang) ? "TOS 작업지시 + PLC/GPS 융합" : "TOS job orders fused with PLC/GPS"}</span></h3>
        <div className="head-sub">
          <span className="pill good">{ko(lang) ? "가동 QC" : "Working QC"} {working.length}</span>
          {fleetMph != null && (
            <span className="pill" style={{ borderColor: "#f59e0b", color: "#fbbf24", background: "rgba(245,158,11,0.10)" }}
              title={ko(lang) ? "websocket PLC 사이클로 계산한 실시간 QC 평균 처리량 (TOS K_MPH 교차검증)" : "live avg QC throughput from PLC cycles (cross-check for TOS K_MPH)"}>
              ⚡ {fleetMph.toFixed(0)} {ko(lang) ? "move/h (실시간)" : "mv/h live"}
            </span>
          )}
          <span className="muted">{ko(lang) ? `잔여 ${(wp?.total_remaining ?? 0).toLocaleString()} move` : `${(wp?.total_remaining ?? 0).toLocaleString()} moves left`}</span>
          <span className="muted">{ageS != null ? `⟳ ${ageS}s` : ""}</span>
        </div>
      </div>
      <div className="tcard-body">
        {working.length === 0 && <div className="lvp-empty">{ko(lang) ? "가동 중인 QC 없음" : "no working QC"}</div>}
        {groups.map((g) => (
          <div className="qc-vgroup" key={g.vessel}>
            <div className="qc-vgroup-h"><span className="vsl">{g.vessel}</span><span className="qc-vgroup-n">{g.items.length} QC</span></div>
            <div className="qc-panel">
              {g.items.map((q) => <QcCol key={q.qc} q={q} lang={lang} ttState={ttState} working={craneFresh.get(q.qc) ?? false} mph={craneMph.get(q.qc)} />)}
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}

function QcCol({ q, lang, ttState, working, mph }: { q: WpQc; lang: Lang; ttState: Map<string, Dev>; working: boolean; mph?: number }) {
  const tot = q.queues.reduce((a, x) => a + x.total, 0);
  const done = q.queues.reduce((a, x) => a + x.done, 0);
  const pct = tot > 0 ? Math.round((done / tot) * 100) : 0;
  const moves = q.moves; // always show every move (card is at the bottom)
  return (
    <div className="qc-col">
      <div className="qc-head">
        <span className={`id ${working ? "busy" : "idle"}`}><span className="dot" />{q.qc}
          <span className="qc-vessel">{q.vessels.join(" · ") || "—"}</span></span>
        {mph != null
          ? <span className="mph" title={ko(lang) ? "PLC 실시간 처리량 (최근 1시간 move)" : "live throughput from PLC (moves in last hour)"}>⚡<span className="v">{mph}</span>/h</span>
          : <span className="mph">{ko(lang) ? "잔여" : "rem"} <span className="v">{q.remaining}</span></span>}
      </div>
      <div className="qc-progress"><span>{q.active_moves} {ko(lang) ? "작업중" : "active"}{working ? (ko(lang) ? " · PLC 가동" : " · PLC live") : ""}</span><span className="mono">{done.toLocaleString()} / {tot.toLocaleString()}</span></div>
      <div className="qc-progress-bar"><div className="fill" style={{ width: `${pct}%` }} /></div>
      {moves.length === 0 && <div className="lvp-empty" style={{ padding: "10px 0" }}>{ko(lang) ? "활성 작업 없음" : "no active move"}</div>}
      {moves.map((m, i) => {
        const tt = m.ytno ? ttState.get(m.ytno) : undefined;
        const dot = tt?.dispatch ? DSP_META[tt.dispatch]?.color : undefined;
        return (
          <div className={`qc-task ${i === 0 ? "now" : "queued"}`} key={`${m.contno}-${i}`}>
            <span className="seq">{i === 0 ? "NOW" : `+${i}`}</span>
            <div className="body">
              <div className="top"><span className={`type-${kindChip(m.jobtype)}`}>{kindLabel(m.jobtype)}</span> {m.contno ?? "—"}{m.twintandem ? ` · ${m.twintandem}` : ""}</div>
              <div className="bot">
                {m.jobtype === "DS" ? `${m.yt_topos ?? m.from_pos ?? "?"} → ${m.armgc ?? "RTG"}` : `${m.armgc ?? m.yt_topos ?? "?"} → ${q.qc}`}
                {(() => { const e = etwLabel(m.etw_accurate, m.etw_expires, lang); return e && <span className={`jetw ${e.cls}`} style={{ marginLeft: 6 }} title={ko(lang) ? "TOS ETW RPC 기반 정확 ETW" : "accurate ETW from the TOS ETW RPC"}>ETW {e.text}</span>; })()}
                {/* DS만 노출: ACTV는 QC 양하 물리 move 완료와 0초 일치(검증 n=3464, 위반0) → "트럭 적재" 확정,
                    자유까지 중앙 ~12분의 카운트다운 시작점. LD의 ACTV는 RTG/블록측 활성으로 실제 픽업과
                    느슨(±90초 내 34%뿐, n=435) + 적재 *시작*단(곧유휴의 반대 끝) + 어떤 dispatch 상태도
                    안 먹임 → 표시하면 오해만 줌(검증 2026-06-16). LD 곧유휴는 QC PLC 쪽이 담당. */}
                {m.actv_ts && m.jobtype === "DS" && (() => {
                  const mins = Math.max(0, Math.round((Date.now() - new Date(m.actv_ts).getTime()) / 60000));
                  const title = ko(lang)
                    ? "TOS ACTV — QC 양하 완료(트럭 적재) 후 경과. 검증: ACTV==QC move 완료 0초 일치(n=3464). 자유까지 중앙 ~12분. 물리 집기 순간 아님."
                    : "TOS ACTV — since QC discharged the box onto the truck. Verified ACTV==QC move complete (0s, n=3464). ~12m median to free.";
                  return <span className="jetw rtg-actv" style={{ marginLeft: 6 }} title={title}>{ko(lang) ? `적재 ${mins}분` : `loaded ${mins}m`}</span>;
                })()}
              </div>
            </div>
            <div className="assign">
              {m.ytno ? <span className="tt" title={dspTitle(tt?.dispatch, lang)}>{dot && <span className="dot" style={{ background: dot, marginRight: 4 }} />}{m.ytno}</span> : <span className="tt-none">{ko(lang) ? "미배차" : "Unassigned"}</span>}
              {tt?.dispatch && <span className="tt-status">{ko(lang) ? DSP_META[tt.dispatch]?.ko : DSP_META[tt.dispatch]?.en}</span>}
            </div>
          </div>
        );
      })}
    </div>
  );
}

// ───────────────────────── candidate job pool (unassigned demand, grouped by QC) ─────
// The work that actually needs dispatching: jobs with NO truck yet. Grouped per QC; the
// QC's urgency = how soon it reaches this work (지금=working now / 곧=soon / 대기=later).
// Each QC shows its demand split by pickup: discharge picks up AT the QC, load picks up
// at source yard blocks (distance varies → shown per block).
type CandGroup = {
  qc: string; vessel: string; total: number; urg: "now" | "soon" | "later";
  ds: number; loads: WpCandidate[];
};
const URG_META: Record<string, { ko: string; en: string; color: string }> = {
  now: { ko: "지금", en: "Now", color: "#ef4444" },
  soon: { ko: "곧", en: "Soon", color: "#f59e0b" },
  later: { ko: "대기", en: "Later", color: "#64748b" },
};

function LiveCandidatePool({ lang, wp }: { lang: Lang; wp: WorkpoolResponse | null }) {
  const cands = wp?.candidates ?? [];
  const total = wp?.candidate_total ?? 0;

  // group candidates by QC (load candidates use their destination QC)
  const byQc = new Map<string, WpCandidate[]>();
  for (const c of cands) {
    const k = c.qc ?? "—";
    (byQc.get(k) ?? byQc.set(k, []).get(k)!).push(c);
  }
  const groups: CandGroup[] = [...byQc.entries()].map(([qc, list]) => {
    const ds = list.filter((c) => c.jobtype === "DS").reduce((a, c) => a + c.n, 0);
    const loads = list.filter((c) => c.jobtype === "LD" && c.src_block).sort((a, b) => b.n - a.n);
    const sum = list.reduce((a, c) => a + c.n, 0);
    const minMoves = Math.min(...list.map((c) => (c.active ? 0 : c.moves_until)));
    const urg: CandGroup["urg"] = list.some((c) => c.active) ? "now" : minMoves < 25 ? "soon" : "later";
    const vessel = (list.find((c) => c.jobtype === "DS") ?? list[0]).vessel;
    return { qc, vessel, total: sum, urg, ds, loads };
  });
  const rank = { now: 0, soon: 1, later: 2 };
  groups.sort((a, b) => rank[a.urg] - rank[b.urg] || b.total - a.total);
  const shown = groups.slice(0, 16);

  return (
    <section className="tcard">
      <div className="tcard-head">
        <h3>{ko(lang) ? "후보 작업 풀" : "Candidate Job Pool"}
          <span className="h3-sub">{ko(lang) ? "미배정 수요 · QC별" : "unassigned demand · by QC"}</span></h3>
        <div className="head-sub"><span className="muted">{ko(lang) ? `트럭 ${total.toLocaleString()} 필요` : `${total.toLocaleString()} trucks needed`}</span></div>
      </div>
      <div className="tcard-body">
        <div className="cand-note">{ko(lang)
          ? "아직 트럭이 안 붙은 작업을 QC별로. 시급도 — 🔴지금(작업 중) · 🟠곧 · ⚪대기. 양하는 QC에서, 적하는 출발 블록에서 픽업."
          : "unassigned work, per QC. Urgency — 🔴Now (working) · 🟠Soon · ⚪Later. Discharge picks up at the QC, load at the source block."}</div>
        <div className="cg-list">
          {shown.length === 0 && <div className="lvp-empty">{ko(lang) ? "미배정 작업 없음" : "none unassigned"}</div>}
          {shown.map((g) => {
            const u = URG_META[g.urg];
            return (
              <div className="cg-card" key={g.qc}>
                <div className="cg-head">
                  <span className="cg-dot" style={{ background: u.color }} />
                  <span className="cg-qc">{g.qc}</span>
                  <span className="cg-vsl">{g.vessel}</span>
                  <span className="cg-urg" style={{ color: u.color, borderColor: u.color }}>{ko(lang) ? u.ko : u.en}</span>
                  <span className="cg-total">{g.total}<small>{ko(lang) ? "대" : ""}</small></span>
                </div>
                <div className="cg-chips">
                  {g.ds > 0 && (
                    <span className="cg-chip ds" title={ko(lang) ? "양하 — QC에서 픽업" : "discharge — pick up at QC"}>
                      <span className="type-dsc">DSC</span> {g.ds} · {ko(lang) ? "QC" : "@QC"}
                    </span>
                  )}
                  {g.loads.slice(0, 5).map((l) => (
                    <span className="cg-chip ld" key={l.src_block} title={ko(lang) ? `적하 — 블록 ${l.src_block}에서 픽업${l.rtg ? ` (${l.rtg})` : ""}` : `load — pick up at block ${l.src_block}${l.rtg ? ` (${l.rtg})` : ""}`}>
                      <span className="type-lod">LOD</span> {l.src_block} {l.n}
                    </span>
                  ))}
                  {g.loads.length > 5 && <span className="cg-more">+{g.loads.length - 5}</span>}
                </div>
              </div>
            );
          })}
        </div>
        {groups.length > shown.length && (
          <div className="cand-note" style={{ marginTop: 8 }}>{ko(lang) ? `+${groups.length - shown.length} QC 더` : `+${groups.length - shown.length} more QC`}</div>
        )}
      </div>
    </section>
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
  const qcs = (wp?.qcs ?? [])
    .map((q) => {
      const trucks = new Set<string>();
      for (const m of q.moves) if (m.ytno && m.ytno.trim()) trucks.add(m.ytno.trim());
      return { qc: q.qc, count: trucks.size, moves: q.active_moves, vessel: q.vessels[0] ?? "" };
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
          <span className="h3-sub">{ko(lang) ? "각 안벽크레인에 현재 배차된 트럭 수 (실시간)" : "trucks currently assigned to each quay crane (live)"}</span></h3>
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
    <div className="content tt-page">
      <QcAssignedCard lang={lang} wp={wp} />
      <LiveDispatchPool lang={lang} snap={snap} err={err} />
      <LiveCandidatePool lang={lang} wp={wp} />
      <LiveQcSequence lang={lang} wp={wp} snap={snap} />
    </div>
  );
}
