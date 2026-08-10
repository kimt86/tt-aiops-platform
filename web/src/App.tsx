import { createContext, useContext, useEffect, useMemo, useState, type ReactElement } from "react";
import { api, type BreakdownResponse, type HealthResponse, type KpiCard, type KpisResponse, type LiveKpi, type LiveResponse, type QcRow, type TrendResponse, type VesselRow, type VesselsResponse } from "./api";
import { LineChart } from "./charts";
import { deltaLabel, fmtValue, isImprovement } from "./format";
import { t, type Lang } from "./i18n";
import BoardPage from "./BoardPage";
import TtPage from "./TtPage";
import Stage2Page from "./Stage2Page";
import CyclesPage from "./CyclesPage";
import LiveMapPage from "./LiveMapPage";
import HealthPage from "./HealthPage";
import FeedHealthPage from "./FeedHealthPage";
import LearnPage from "./LearnPage";
import HistoryMatrix from "./HistoryMatrix";

function useClock(): string {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const id = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(id);
  }, []);
  return now.toISOString().slice(0, 19).replace("T", " ");
}

interface Data {
  kpis: KpisResponse | null;
  breakdown: BreakdownResponse | null;
  trends: Record<string, TrendResponse>;
}

function useData(period: string): Data {
  const [data, setData] = useState<Data>({ kpis: null, breakdown: null, trends: {} });
  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const [kpis, breakdown] = await Promise.all([api.kpis(period), api.breakdown(period)]);
        // sparkline follows the selected period so it actually changes per period
        const trendList = await Promise.all(
          kpis.kpis.map((k) => api.trend(k.key, { from: kpis.range_from, to: kpis.range_to }))
        );
        if (!alive) return;
        const trends: Record<string, TrendResponse> = {};
        trendList.forEach((tr) => (trends[tr.key] = tr));
        setData({ kpis, breakdown, trends });
      } catch (e) {
        console.error(e);
      }
    };
    load();
    const id = setInterval(load, 30000); // poll
    return () => { alive = false; clearInterval(id); };
  }, [period]);
  return data;
}

const PERIOD_GROUPS: string[][] = [
  ["today", "yesterday"],
  ["this_week", "last_week"],
  ["this_month", "last_month"],
  ["last7", "last30"],
];

// ── live-feed health (real-time GPS pipeline) — the authoritative "is the terminal feed up?" bit.
// Drives the global outage banner + per-page treatments. /api/livemap/health already computes a
// green/amber/red verdict from `connected` + `last_msg_age_s` (red = !connected || age>60s). This is
// the ONLY per-second signal; /api/health (data_freshness) is daily-cadence ETL and must not be used.
export type FeedHealth = {
  down: boolean; warn: boolean; connected: boolean; ageS: number | null; cause: string;
} | null;
const FeedHealthContext = createContext<FeedHealth>(null);
export function useFeedHealthCtx(): FeedHealth { return useContext(FeedHealthContext); }

function useFeedHealth(): FeedHealth {
  const [f, setF] = useState<FeedHealth>(null);
  useEffect(() => {
    let alive = true;
    const poll = () => fetch("/api/livemap/health").then((r) => (r.ok ? r.json() : null))
      .then((h) => {
        if (!alive) return;
        if (!h) { setF({ down: true, warn: false, connected: false, ageS: null, cause: "api" }); return; }
        setF({ down: h.color === "red", warn: h.color === "amber", connected: !!h.connected,
               ageS: h.last_msg_age_s ?? null, cause: h.cause ?? "" });
      })
      .catch(() => { if (alive) setF({ down: true, warn: false, connected: false, ageS: null, cause: "api" }); });
    poll();
    const id = setInterval(poll, 5000);
    return () => { alive = false; clearInterval(id); };
  }, []);
  return f;
}

function fmtAge(s: number, ko: boolean): string {
  if (s < 60) return `${Math.round(s)}${ko ? "초" : "s"}`;
  if (s < 3600) return `${Math.floor(s / 60)}${ko ? "분" : "m"}`;
  if (s < 86400) return `${Math.floor(s / 3600)}${ko ? "시간" : "h"}`;
  return `${Math.floor(s / 86400)}${ko ? "일" : "d"}`;
}

// Full-width outage bar shown on EVERY page when the live feed is down/lagging.
function OutageBanner({ feed, lang }: { feed: FeedHealth; lang: Lang }) {
  if (!feed || (!feed.down && !feed.warn)) return null;
  const ko = lang === "ko";
  const age = feed.ageS != null ? fmtAge(feed.ageS, ko) : null;
  let msg: string;
  if (feed.down) {
    let tail: string;
    if (feed.cause === "api") tail = ko ? " · 대시보드 API 응답 없음" : " · dashboard API not responding";
    else if (age) tail = ko ? ` · 마지막 수신 ${age} 전 · 화면의 데이터는 정지 상태입니다` : ` · last packet ${age} ago · data on screen is frozen`;
    else tail = ko ? " · 화면의 데이터는 정지 상태입니다" : " · data on screen is frozen";
    msg = (ko ? "터미널 라이브 피드 끊김" : "Terminal live feed down") + tail;
  } else {
    msg = age ? (ko ? `라이브 피드 지연 — 마지막 수신 ${age} 전` : `Live feed lagging — last packet ${age} ago`)
              : (ko ? "라이브 피드 지연" : "Live feed lagging");
  }
  return (
    <div className={`outage-banner ${feed.down ? "down" : "warn"}`} role="alert">
      <span className="ob-ico" aria-hidden="true">⚠</span>
      <span className="ob-msg">{msg}</span>
    </div>
  );
}

// ── operational alerts (mig 0107) — the channel that makes db::prune failures, size ceilings,
// dead-man checks and unit failures reach a person. Before this they were tracing::warn! lines in a
// journal nobody read, which is how the road graph ran degraded for five days unnoticed.
//
// Deliberately a SECOND bar rather than an extension of OutageBanner above: that banner's verdict is
// computed server-side in livemap.rs and LiveMapPage consumes the same FeedHealthContext for its
// live/stale/replay modes, so folding another source into it would change dispatch-page behaviour.
//
// Self-healing, no ack: the API returns a 24h window with age_s and only recent rows count as
// active, so a condition that clears stops being refreshed and drops off on its own. A manual ack
// would get neglected, and a neglected crit becomes a permanent bar — which is how alert plumbing
// stops being read at all.
type OpsAlert = {
  source: string; subject: string; severity: string; message: string; occurrences: number; age_s: number;
};
const OPS_ACTIVE_S = 3600; // watchdog refreshes every 120s; the slowest prune cadence is well inside an hour

function OpsAlertBanner({ lang }: { lang: Lang }) {
  const [rows, setRows] = useState<OpsAlert[]>([]);
  useEffect(() => {
    let alive = true;
    const poll = () => fetch("/api/ops/alerts").then((r) => (r.ok ? r.json() : []))
      .then((a: OpsAlert[]) => { if (alive) setRows(Array.isArray(a) ? a : []); })
      // a dead API is already the feed banner's job; staying quiet here avoids a duplicate bar
      .catch(() => {});
    poll();
    const id = setInterval(poll, 30000);
    return () => { alive = false; clearInterval(id); };
  }, []);
  const active = rows.filter((r) => r.age_s < OPS_ACTIVE_S);
  if (active.length === 0) return null;
  const ko = lang === "ko";
  const crit = active.filter((r) => r.severity === "crit");
  const head = crit[0] ?? active[0];
  const more = active.length - 1;
  const msg = (ko ? "운영 경고" : "Ops alert") + ": " + head.message
    + (more > 0 ? (ko ? ` · 외 ${more}건` : ` · +${more} more`) : "");
  return (
    <div className={`outage-banner ${crit.length > 0 ? "down" : "warn"}`} role="alert" title={active.map((r) => `${r.source}/${r.subject}: ${r.message}`).join("\n")}>
      <span className="ob-ico" aria-hidden="true">⚑</span>
      <span className="ob-msg">{msg}</span>
    </div>
  );
}

function HealthPill({ health, feed, lang }: { health: HealthResponse | null; feed: FeedHealth; lang: Lang }) {
  const s = t(lang);
  // real-time GPS feed down dominates — the pill must not read green during a live outage; otherwise
  // fall back to the TOS/ETL rollup from /api/health.
  if (feed?.down) return <span className="status-pill bad"><span className="dot" />{lang === "ko" ? "피드 다운" : "Feed down"}</span>;
  if (!health) return null;
  const cls = health.overall === "OK" ? "ok" : health.overall === "STALE" ? "warn" : "bad";
  const label = health.overall === "OK" ? s.healthOk : health.overall === "STALE" ? s.healthStale : s.healthDegraded;
  return <span className={`status-pill ${cls}`}><span className="dot" />{label}</span>;
}

function name(c: KpiCard, lang: Lang) { return lang === "ko" ? c.name_ko : c.name_en; }

// Extra sub-values shown inside the distance/ratio cards (no new cards, no backend
// change). Loaded + total travel and loaded-ratio are exact functions of the two
// existing KPIs — empty distance E (km/Job) and empty ratio R (%):
//   total = E·100/R · loaded = total − E · loaded-ratio = 100 − R
type KExtra = { label: string; value: string };
function distanceExtras(items: { key: string; value: number | null }[], key: string, lang: Lang): KExtra[] | undefined {
  if (key !== "K_EMPTY" && key !== "K_EMPTY_R") return undefined;
  const E = items.find((x) => x.key === "K_EMPTY")?.value ?? null;     // empty km/Job
  const R = items.find((x) => x.key === "K_EMPTY_R")?.value ?? null;   // empty ratio %
  if (E == null || R == null || R <= 0) return undefined;
  const ko = lang === "ko";
  if (key === "K_EMPTY") {
    const total = (E * 100) / R;
    const loaded = total - E;
    return [
      { label: ko ? "적재 거리" : "Loaded", value: `${loaded.toFixed(2)} km/Job` },
      { label: ko ? "전체 거리" : "Total", value: `${total.toFixed(2)} km/Job` },
    ];
  }
  return [{ label: ko ? "적재 비율" : "Loaded", value: `${(100 - R).toFixed(1)} %` }];
}
// per-jobtype TT cycle breakdown shown on the cycle card (discharge / load).
function CycleSplit({ ds, ld, ko }: { ds?: number | null; ld?: number | null; ko: boolean }) {
  if (ds == null && ld == null) return null;
  return (
    <div className="cyc-split mono">
      <span><span className="cs-l">{ko ? "양하" : "DS"}</span> {fmtCycle(ds)}</span>
      <span><span className="cs-l">{ko ? "적하" : "LD"}</span> {fmtCycle(ld)}</span>
    </div>
  );
}
function KpiExtras({ extras }: { extras?: KExtra[] }) {
  if (!extras || extras.length === 0) return null;
  return (
    <div className="kpi-extras">
      {extras.map((e) => (
        <div className="kpi-extra" key={e.label}><span className="kx-l">{e.label}</span><span className="kx-v mono">{e.value}</span></div>
      ))}
    </div>
  );
}

function SmallCard({ c, trend, lang, extras }: { c: KpiCard; trend?: TrendResponse; lang: Lang; extras?: KExtra[] }) {
  const s = t(lang);
  const ko = lang === "ko";
  const imp = isImprovement(c);
  const dl = deltaLabel(c);
  // NOTE: K_QC_TT_WAIT_GPS demoted to SHADOW (2026-06-19) — it keeps accumulating in kpi_daily/shift
  // for parallel comparison with the TOS K_QC_TT_WAIT, but is no longer surfaced on this card.
  return (
    <div className={`kpi${c.tier === "PRIMARY" ? " primary" : ""}`}>
      <div className="label">{name(c, lang)}<SourceBadge src="tos" ko={ko} /></div>
      <div className="vrow">
        <span className="val">{mainValue(c.key, c.value, c.unit).val}</span>
        <span className="unit">{mainValue(c.key, c.value, c.unit).unit}</span>
      </div>
      <KpiExtras extras={extras} />
      {c.key === "K_CYCLE" && <CycleSplit ds={c.ds_cycle_s} ld={c.ld_cycle_s} ko={ko} />}
      {dl ? (
        <div className={`delta ${imp ? "good" : "bad"}`}>{dl}<span className="vs">{s.vsBaseline}</span></div>
      ) : (
        <div className="delta" style={{ color: "var(--text-mute)" }}>{s.baselinePending}</div>
      )}
      <div className="spark">
        {trend && trend.points.length > 1 && (
          <LineChart values={trend.points.map((p) => p.value)} color={imp === false ? "#f59e0b" : "#60a5fa"} />
        )}
      </div>
    </div>
  );
}

// ---- LIVE tab ----

function useLive() {
  const [live, setLive] = useState<LiveResponse | null>(null);
  const [vessels, setVessels] = useState<VesselsResponse | null>(null);
  const [today, setToday] = useState<KpisResponse | null>(null);
  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const [l, v, td] = await Promise.all([api.live(), api.liveVessels(), api.kpis("today")]);
        if (alive) { setLive(l); setVessels(v); setToday(td); }
      } catch (e) { console.error(e); }
    };
    load();
    const id = setInterval(load, 20000); // fast poll for LIVE
    return () => { alive = false; clearInterval(id); };
  }, []);
  return { live, vessels, today };
}

// websocket-derived LIVE cross-check strip: per-second signals that refine the TOS
// shift KPIs (which are pulled every few minutes and hour-bucketed). See the KC doc
// "websocket로 KPI 정확도 향상". The live TT cycle here is the REAL truck transport
// cycle, distinct from (and far shorter than) the renamed TOS "작업 처리 시간".
type WsLive = {
  tt_cycle_littles_s?: number | null; tt_cycle_median_s?: number | null; tt_cycle_samples?: number;
  tt_cycle_min_samples?: number; tt_cycle_p25_s?: number | null; tt_cycle_p75_s?: number | null;
  tt_artifacts_60m?: number; tt_artifacts_near_60m?: number; window_fill_min?: number;
  tt_util_live?: number | null; tt_engaged_live?: number | null; tt_util_shift_avg?: number | null; crane_mph_live?: number | null;
  qc_starving?: number; qc_wait_live_s?: number | null; active_trucks?: number; connected?: boolean;
};
function fmtCycle(s: number | null | undefined): string {
  if (s == null) return "—";
  s = Math.round(s); // whole seconds — the source value can carry a .5 decimal
  return s >= 60 ? `${Math.floor(s / 60)}m ${s % 60}s` : `${s}s`;
}
// poll the per-second websocket feed (shared by the KPI cards so each can show its live
// counterpart inline next to the TOS value).
function useWsLive(): WsLive | null {
  const [w, setW] = useState<WsLive | null>(null);
  useEffect(() => {
    let alive = true;
    const poll = () => fetch("/api/livemap/positions").then((r) => r.ok ? r.json() : null)
      .then((j) => { if (alive && j) setW(j); }).catch(() => {});
    poll();
    const id = setInterval(poll, 5000);
    return () => { alive = false; clearInterval(id); };
  }, []);
  return w;
}

// What data source(s) a KPI card shows, and which leads.
//  • "dual"    — TOS leads (headline = c.value), websocket shown as an auxiliary "live"
//                line. The lower-variation value is the headline; TOS shift/daily aggregates
//                are smoother than the per-second feed, so TOS leads on
//                K_UTIL/K_MPH/K_QC_NOMOVE/K_CYCLE (the TOS truck-cycle approximation).
//  • "tos"     — TOS only (no websocket counterpart, or the ws feed is down).
//  • "wsOnly"  — only the websocket value is meaningful; the TOS value is not shown
//                (K_UTIL: the TOS session number counts idle as utilized, so it is dropped).
type CardSrc =
  | { kind: "tos" }
  | { kind: "dual"; auxVal: string; auxTitle: string }
  | { kind: "wsOnly"; val: string; title: string; sub?: string };

function cardSrc(key: string, w: WsLive | null, ko: boolean): CardSrc {
  if (key === "K_UTIL") {
    // TRUE utilization: of manned trucks, the fraction with an active job assignment
    // (allocated→completed) — a truck queued at a crane with a job is utilized, NOT idle.
    // Idle = manned but unassigned (awaiting dispatch). TOS session value is not shown.
    const now = w && w.connected ? (w.tt_util_live ?? null) : null;          // instantaneous
    const shift = w && w.connected ? (w.tt_util_shift_avg ?? null) : null;    // time-based shift avg
    const headline = shift ?? now; // prefer the time-based shift average (history-bearing)
    return {
      kind: "wsOnly",
      val: headline != null ? `${headline}%` : "—",
      sub: shift != null
        ? (ko ? `교대 평균 · 현재 ${now ?? "—"}%` : `shift avg · now ${now ?? "—"}%`)
        : (now != null ? (ko ? "현재 · 교대평균 수집중" : "now · shift avg collecting") : ""),
      title: ko
        ? `진짜 가동률(시간기반, 100% TOS) — 활성 작업 중 트럭(A) / 배치된 트럭(A·블록·큐, 모든 작업유형 DS/LD+MI/MO/LC)을 60초마다 표본화해 교대 평균. 할당~완료(크레인 전달) 사이는 멈춰 있어도(큐잉 포함) 가동. 분모도 TOS 작업풀(GPS 미사용). 헤드라인=교대 평균, 보조=현재 ${now ?? "—"}%.`
        : `true utilization (time-based, 100% TOS) — actively-dispatched trucks (status A) / tasked fleet (A+blocked+queued, all job types DS/LD+MI/MO/LC), sampled every 60s and averaged over the shift. Allocation→completion counts even while stopped (queuing incl.). Denominator is also the TOS work pool (no GPS). Headline = shift average, secondary = now ${now ?? "—"}%.`,
    };
  }
  if (key === "K_CYCLE") {
    // Truck cycle. Headline = TOS approximation (raw_k_tt_cycle: per-truck consecutive
    // QC-move interval, ~14m), lower-variation and always available. Live GPS cycle (~13m,
    // the same quantity, movement-validated) rides along as the ⚡ aux. Both agree; "—"
    // when the TOS value has no data. The ~40m container handling span is kept internal.
    const cyc = w?.tt_cycle_median_s ?? null;
    if (w?.connected && cyc != null) {
      const n = w.tt_cycle_samples ?? 0;
      return {
        kind: "dual",
        auxVal: fmtCycle(cyc),
        auxTitle: ko
          ? `실시간 GPS 트럭 사이클 — container1 변경을 이동 ≥150m로 검증(n=${n}). TOS(MCH_OPERATION) 근사 ~14분과 일치하는 독립 측정.`
          : `live GPS truck cycle — container1 changes movement-validated ≥150m (n=${n}); an independent measure agreeing with the TOS (MCH_OPERATION) ~14m approximation.`,
      };
    }
    return { kind: "tos" };
  }
  if (!w || !w.connected) return { kind: "tos" };
  if (key === "K_MPH" && w.crane_mph_live != null)
    return { kind: "dual", auxVal: `${w.crane_mph_live}/h`, auxTitle: ko ? "실시간 QC 평균 처리량 (PLC 사이클)" : "live avg QC throughput (PLC cycles)" };
  // K_QC_TT_WAIT_GPS demoted to shadow (2026-06-19): no longer shown as a card secondary; it still
  // accumulates in kpi_daily/shift for parallel comparison with the TOS K_QC_TT_WAIT.
  return { kind: "tos" };
}

// small source chip on each KPI card: TOS DB vs websocket vs both.
function SourceBadge({ src, ko }: { src: "tos" | "ws" | "dual"; ko: boolean }) {
  if (src === "ws") return <span className="src-badge ws" title={ko ? "websocket 실시간 GPS/PLC로 산출" : "computed from the live websocket GPS/PLC feed"}>⚡ WS</span>;
  if (src === "dual") return <span className="src-badge dual" title={ko ? "TOS DB(메인) + websocket 실시간(보조)" : "TOS DB (main) + websocket live (auxiliary)"}>TOS<span className="sb-plus">+⚡</span></span>;
  return <span className="src-badge tos" title={ko ? "TOS DB 기반" : "from TOS DB"}>TOS</span>;
}

// auxiliary live row shown under the TOS headline on a dual-source card.
function WsAux({ val, title, ko }: { val: string; title: string; ko: boolean }) {
  return (
    <div className="ws-aux" title={title}>
      <span className="ws-aux-b">⚡</span>
      <span className="ws-aux-l">{ko ? "실시간" : "live"}</span>
      <span className="ws-aux-v mono">{val}</span>
    </div>
  );
}

// the cycle KPI is stored in seconds but reads as a duration; everything else uses fmtValue.
// (K_UTIL kpi_daily is now the TIME-BASED utilization aggregated from work-pool samples, so
// historical/period cells show it normally; the LIVE cards use the websocket shift average.)
function mainValue(key: string, value: number | null, unit: string): { val: string; unit: string } {
  if (key === "K_CYCLE") return { val: value != null ? fmtCycle(value) : "—", unit: "" };
  return { val: fmtValue(value, unit), unit };
}

function LiveCard({ c, lang, ws, extras }: { c: LiveKpi; lang: Lang; ws: WsLive | null; extras?: KExtra[] }) {
  const s = t(lang);
  const ko = lang === "ko";
  const src = cardSrc(c.key, ws, ko);
  const mv = mainValue(c.key, c.value, c.unit);
  const nm = ko ? c.name_ko : c.name_en;
  const imp = c.delta_abs == null || !c.direction ? null : (c.direction === "LOWER_BETTER" ? c.delta_abs < 0 : c.delta_abs > 0);
  const deltaTxt = c.delta_abs == null ? null : (() => {
    const arrow = c.delta_abs > 0 ? "▲" : "▼";
    if (c.unit === "%") return `${arrow} ${Math.abs(c.delta_abs).toFixed(1)}pp`;
    if (c.delta_pct != null) return `${arrow} ${Math.abs(c.delta_pct).toFixed(1)}%`;
    return `${arrow} ${Math.abs(c.delta_abs).toFixed(2)}`;
  })();
  // K_UTIL: websocket-only (idle-excluded true utilization); TOS session value not shown.
  if (src.kind === "wsOnly") {
    return (
      <div className={`kpi${c.tier === "PRIMARY" ? " primary" : ""}`} title={src.title}>
        <div className="label">{nm}<SourceBadge src="tos" ko={ko} /></div>
        <div className="vrow"><span className="val">{src.val}</span></div>
        <div className="ws-sub mono">{src.sub || (ko ? "할당 기준" : "by assignment")}</div>
        <div className="n" style={{ marginTop: "auto" }}>{ko ? "TOS 작업풀" : "TOS work pool"}</div>
      </div>
    );
  }
  return (
    <div className={`kpi${c.tier === "PRIMARY" ? " primary" : ""}`}>
      <div className="label">{nm}<SourceBadge src={src.kind === "dual" ? "dual" : "tos"} ko={ko} /></div>
      <div className="vrow"><span className="val">{mv.val}</span><span className="unit">{mv.unit}</span></div>
      <KpiExtras extras={extras} />
      {src.kind === "dual" && <WsAux val={src.auxVal} title={src.auxTitle} ko={ko} />}
      {c.key === "K_CYCLE" && <CycleSplit ds={c.ds_cycle_s} ld={c.ld_cycle_s} ko={ko} />}
      {deltaTxt
        ? <div className={`delta ${imp ? "good" : "bad"}`}>{deltaTxt}<span className="vs">{s.vsPrevShift}</span></div>
        : <div className="delta" style={{ color: "var(--text-mute)" }}>{s.noPrevShift}</div>}
      <div className="n" style={{ marginTop: "auto" }}>N {c.sample_n != null ? c.sample_n.toLocaleString() : "—"}</div>
    </div>
  );
}

const vkey = (v: { vessel: string; voyage: string }) => `${v.vessel}/${v.voyage}`;

function VesselPanel({ vessels, lang, onSelect }: { vessels: VesselRow[]; lang: Lang; onSelect: (v: VesselRow) => void }) {
  const s = t(lang);
  const hhmm = (ts: string | null) => (ts && ts.length >= 12 ? `${ts.slice(8, 10)}:${ts.slice(10, 12)}` : "—");
  return (
    <div className="grid vessel-grid">
      {vessels.map((v) => (
        <div className="vessel-card clickable" key={vkey(v)} onClick={() => onSelect(v)} title={s.qcThroughput}>
          <div className="vtop"><span className="vname">{v.vessel}</span><span className="vvoy mono">{v.voyage}</span><span className="spacer" /><span className="vmore">QC ›</span></div>
          <div className="vqcs">{v.qcs.map((q) => <span className="qc-chip" key={q}>{q}</span>)}</div>
          <div className="vprog-row">
            <span className="mono">{v.moves?.toLocaleString() ?? "—"} {s.moves}{v.planned_moves ? ` / ${v.planned_moves.toLocaleString()}` : ""}</span>
            <span className="mono" style={{ color: "var(--brand)" }}>{v.progress_pct != null ? `${v.progress_pct}%` : ""}</span>
          </div>
          {v.progress_pct != null && <div className="vbar"><div className="vbar-fill" style={{ width: `${Math.min(100, v.progress_pct)}%` }} /></div>}
          <div className="vmeta mono">
            <span>{s.mph} {v.mph?.toFixed(1) ?? "—"}</span>
            <span>{s.ldDs} {v.load_moves ?? 0}/{v.discharge_moves ?? 0}</span>
            <span>{hhmm(v.first_move)}→{hhmm(v.last_move)}</span>
          </div>
        </div>
      ))}
    </div>
  );
}

// Today-cumulative KPI card (full-day-so-far, vs previous period). Mirrors LiveCard.
function TodayCard({ c, lang, ws, extras }: { c: KpiCard; lang: Lang; ws: WsLive | null; extras?: KExtra[] }) {
  const s = t(lang);
  const ko = lang === "ko";
  const src = cardSrc(c.key, ws, ko);
  const mv = mainValue(c.key, c.value, c.unit);
  const imp = isImprovement(c);
  const dl = deltaLabel(c);
  if (src.kind === "wsOnly") {
    return (
      <div className={`kpi${c.tier === "PRIMARY" ? " primary" : ""}`} title={src.title}>
        <div className="label">{name(c, lang)}<SourceBadge src="tos" ko={ko} /></div>
        <div className="vrow"><span className="val">{src.val}</span></div>
        <div className="ws-sub mono">{src.sub || (ko ? "할당 기준" : "by assignment")}</div>
        <div className="n" style={{ marginTop: "auto" }}>{ko ? "TOS 작업풀" : "TOS work pool"}</div>
      </div>
    );
  }
  return (
    <div className={`kpi${c.tier === "PRIMARY" ? " primary" : ""}`}>
      <div className="label">{name(c, lang)}<SourceBadge src={src.kind === "dual" ? "dual" : "tos"} ko={ko} /></div>
      <div className="vrow"><span className="val">{mv.val}</span><span className="unit">{mv.unit}</span></div>
      <KpiExtras extras={extras} />
      {src.kind === "dual" && <WsAux val={src.auxVal} title={src.auxTitle} ko={ko} />}
      {c.key === "K_CYCLE" && <CycleSplit ds={c.ds_cycle_s} ld={c.ld_cycle_s} ko={ko} />}
      {dl
        ? <div className={`delta ${imp ? "good" : "bad"}`}>{dl}<span className="vs">{s.vsBaseline}</span></div>
        : <div className="delta" style={{ color: "var(--text-mute)" }}>{s.baselinePending}</div>}
      <div className="n" style={{ marginTop: "auto" }}>N {c.sample_n != null ? c.sample_n.toLocaleString() : "—"}</div>
    </div>
  );
}

// Modern popup: per-QC throughput for one vessel, opened by clicking its card.
function VesselQcModal({ vessel, lang, onClose }: { vessel: VesselRow; lang: Lang; onClose: () => void }) {
  const s = t(lang);
  const hhmm = (ts: string | null) => (ts && ts.length >= 12 ? `${ts.slice(8, 10)}:${ts.slice(10, 12)}` : "—");
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  const qcs = vessel.qc_rows ?? [];
  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <div><span className="vname">{vessel.vessel}</span> <span className="vvoy mono">{vessel.voyage}</span></div>
          <button className="modal-x" onClick={onClose} aria-label="close">×</button>
        </div>
        <div className="modal-sub mono">
          {(vessel.moves ?? 0).toLocaleString()} {s.moves}
          {vessel.planned_moves ? ` / ${vessel.planned_moves.toLocaleString()}` : ""}
          {vessel.progress_pct != null ? `  ·  ${vessel.progress_pct}%` : ""}
          {"  ·  "}{s.mph} {vessel.mph?.toFixed(1) ?? "—"}
          {"  ·  "}{s.ldDs} {vessel.load_moves ?? 0}/{vessel.discharge_moves ?? 0}
          {"  ·  "}{hhmm(vessel.first_move)}→{hhmm(vessel.last_move)}
        </div>
        <div className="modal-qc-title">{s.qcThroughput}<span className="section-sub">{qcs.length}</span></div>
        {qcs.length === 0 ? (
          <div className="loading">{s.noData}</div>
        ) : (
          <div className="qc-vcards">
            {qcs.map((q) => (
              <div className="qc-mini" key={q.qc}>
                <div className="qc-mini-id mono">{q.qc}</div>
                <div className="qc-mini-mph">{q.mph?.toFixed(1) ?? "—"}<span className="qc-mini-unit"> {s.mph}</span></div>
                <div className="qc-mini-mv mono">{(q.moves ?? 0).toLocaleString()} {s.moves} · {q.load_moves ?? 0}/{q.discharge_moves ?? 0}</div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function LiveTab({ lang }: { lang: Lang }) {
  const s = t(lang);
  const { live, vessels, today } = useLive();
  const ws = useWsLive();
  const [selKey, setSelKey] = useState<string | null>(null);
  if (!live) return <div className="loading">{s.loading}</div>;
  const shiftName = (s as Record<string, string>)["shift_" + live.shift];
  const vrows = vessels?.vessels ?? [];
  // 목록 기준은 "이번 교대 QC 실적"(vessel_shift) 그대로 두고, 이미 출항한 배만 접힌
  // 하위 섹션으로 분리한다 (2026-08-11 사용자 결정). departed = live_vessel_schedule.actdep_ts.
  const active = vrows.filter((v) => !v.departed);
  const gone = vrows.filter((v) => v.departed);
  const sel = vrows.find((v) => vkey(v) === selKey) ?? null; // re-resolve each poll so the modal stays live
  return (
    <>
      {/* 현재 쉬프트 정보 */}
      <div className="shift-header">
        <span className="shift-badge">{shiftName} <span className="mono">({live.shift})</span></span>
        <span className="mono shift-win">{live.window_start} → {live.as_of}</span>
        <span className="shift-elapsed">{s.elapsed} {live.elapsed_min}{s.min} · {s.remaining} {live.remaining_min}{s.min}</span>
        <span className="spacer" />
        <span style={{ color: "var(--text-dim)", fontSize: 11 }} className="mono">{live.business_date}</span>
      </div>

      {/* 현재 쉬프트 KPI 7개 */}
      <div className="section-title">{s.shiftKpis}<span className="section-sub">{s.vsPrevShift}</span></div>
      <div className="grid kpi-strip">
        {live.kpis.map((c) => <LiveCard key={c.key} c={c} lang={lang} ws={ws} extras={distanceExtras(live.kpis, c.key, lang)} />)}
      </div>

      {/* 오늘 누적 KPI 7개 */}
      <div className="section-title" style={{ marginTop: 18 }}>{s.todayKpis}<span className="section-sub">{s.vsBaseline}</span></div>
      <div className="grid kpi-strip">
        {(today?.kpis ?? []).map((c) => <TodayCard key={c.key} c={c} lang={lang} ws={ws} extras={distanceExtras(today?.kpis ?? [], c.key, lang)} />)}
      </div>

      {/* 현재 작업 중인 선박 (카드 클릭 → QC별 처리량 팝업) */}
      <div className="section-title" style={{ marginTop: 18 }}>{s.activeVessels}<span className="section-sub">{active.length}</span></div>
      <VesselPanel vessels={active} lang={lang} onSelect={(v) => setSelKey(vkey(v))} />

      {/* 이번 교대에 작업 후 출항 완료한 선박 — 기본 접힘 */}
      {gone.length > 0 && (
        <details className="departed-fold">
          <summary className="section-title departed-title">{s.departedVessels}<span className="section-sub">{gone.length}</span></summary>
          <VesselPanel vessels={gone} lang={lang} onSelect={(v) => setSelKey(vkey(v))} />
        </details>
      )}

      {sel && <VesselQcModal vessel={sel} lang={lang} onClose={() => setSelKey(null)} />}
    </>
  );
}

// Period QC throughput as a modern card grid (replaces the sparse table): each crane
// is a card with its moves/hr (bar = relative to the period's fastest crane) and wait.
function QcGrid({ rows, lang }: { rows: QcRow[]; lang: Lang }) {
  const s = t(lang);
  if (rows.length === 0) return <div className="loading">{s.noData}</div>;
  const maxMph = Math.max(1, ...rows.map((r) => r.mph ?? 0));
  return (
    <div className="qc-grid">
      {rows.map((r) => {
        const pct = r.mph != null ? Math.round((r.mph / maxMph) * 100) : 0;
        return (
          <div className="qcg-card" key={r.qc}>
            <div className="qcg-top"><span className="qcg-id mono">{r.qc}</span></div>
            <div className="qcg-mph">{r.mph != null ? r.mph.toFixed(1) : "—"}<span className="qcg-unit"> {s.mph}</span></div>
            <div className="qcg-bar"><div className="qcg-bar-fill" style={{ width: `${pct}%` }} /></div>
            <div className="qcg-wait mono">{s.qcWait} {r.qc_wait_sec != null ? Math.round(r.qc_wait_sec) : "—"}</div>
          </div>
        );
      })}
    </div>
  );
}

// ---- HISTORY section (period browser; lives at the bottom of the page) ----

function HistoryTab({ lang }: { lang: Lang }) {
  const [period, setPeriod] = useState<string>("last7");
  const data = useData(period);
  const s = t(lang);
  const kpis = data.kpis?.kpis ?? [];
  return (
      <>
        <div className="period-bar">
          <span className="period-label">{s.period}</span>
          {PERIOD_GROUPS.map((g, i) => (
            <div className="period-group" key={i}>
              {g.map((p) => (
                <button key={p} className={`period-btn${period === p ? " active" : ""}`} onClick={() => setPeriod(p)}>
                  {(s as Record<string, string>)["p_" + p]}
                </button>
              ))}
            </div>
          ))}
          {data.kpis && (
            <span className="period-range mono">
              {data.kpis.range_from} ~ {data.kpis.range_to}
              <span style={{ color: "var(--text-faint)" }}> ({s.vs} {data.kpis.prev_from}~{data.kpis.prev_to})</span>
            </span>
          )}
        </div>
        {kpis.length === 0 ? (
          <div className="loading">{s.loading}</div>
        ) : (
          <>
            <div className="section-title">{s.headline}</div>
            <div className="grid kpi-strip">
              {kpis.map((c) => <SmallCard key={c.key} c={c} trend={data.trends[c.key]} lang={lang} extras={distanceExtras(kpis, c.key, lang)} />)}
            </div>

            <div className="section-title" style={{ marginTop: 18 }}>{s.qcBreakdown}<span className="section-sub">{data.breakdown?.as_of}</span></div>
            <QcGrid rows={data.breakdown?.rows ?? []} lang={lang} />
          </>
        )}
      </>
  );
}

// ---- App shell ----

function useHealth(): HealthResponse | null {
  const [h, setH] = useState<HealthResponse | null>(null);
  useEffect(() => {
    let alive = true;
    const load = () => api.health().then((x) => alive && setH(x)).catch(() => {});
    load();
    const id = setInterval(load, 30000);
    return () => { alive = false; clearInterval(id); };
  }, []);
  return h;
}

function KpiPage({ lang }: { lang: Lang }) {
  const s = t(lang);
  return (
    <div className="content">
      <LiveTab lang={lang} />
      <div className="area-divider"><span>{s.areaHistory}</span></div>
      <HistoryTab lang={lang} />
      <HistoryMatrix lang={lang} />
    </div>
  );
}

const IconKpi = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <line x1="6" y1="20" x2="6" y2="13" /><line x1="12" y1="20" x2="12" y2="7" /><line x1="18" y1="20" x2="18" y2="11" />
  </svg>
);
const IconTt = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <rect x="1" y="6" width="13" height="10" rx="1.5" /><path d="M14 9h3.5L21 12.5V16h-7z" /><circle cx="5.5" cy="18.5" r="1.6" /><circle cx="17.5" cy="18.5" r="1.6" />
  </svg>
);
const IconMap = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M12 21s7-6.3 7-11a7 7 0 1 0-14 0c0 4.7 7 11 7 11z" /><circle cx="12" cy="10" r="2.5" />
  </svg>
);
const IconStage2 = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M16 3h5v5" /><path d="M21 3L13 11" /><path d="M8 21H3v-5" /><path d="M3 21l8-8" /><circle cx="6" cy="6" r="2.2" /><circle cx="18" cy="18" r="2.2" />
  </svg>
);
const IconCycles = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M21 12a9 9 0 1 1-2.64-6.36" /><polyline points="21 3 21 9 15 9" />
  </svg>
);
const IconHealth = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M3 12h4l2 5 4-12 2 7h6" />
  </svg>
);
const IconFeed = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M4 11a9 9 0 0 1 9 9" /><path d="M4 4a16 16 0 0 1 16 16" /><circle cx="5" cy="19" r="1.5" fill="currentColor" stroke="none" />
  </svg>
);
const IconLearn = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M3 3v18h18" /><path d="M7 14l4-4 3 3 5-6" />
  </svg>
);

const IconBoard = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <rect x="3" y="4" width="18" height="16" rx="2" /><path d="M7 9h6M7 13h10M7 17h4" />
  </svg>
);

type PageKey = "kpi" | "board" | "tt" | "stage2" | "cycles" | "learn" | "map" | "health" | "feed";
// internal: 고객에게 보여주지 않는 내부 진단·분석 페이지 (2026-08-11 사용자 결정).
// 기본은 고객 화면(탭 숨김 + 해시 직접 접근도 KPI 로 우회)이고, 주소 뒤 #internal 로
// 내부 모드를 켜면 localStorage 에 기억된다. 표시 구분일 뿐 인증이 아니다 — API 는
// 그대로 열려 있으므로, 진짜 접근 통제가 필요해지면 서버측 게이트를 따로 만든다.
const PAGES: { key: PageKey; label: string; Icon: () => ReactElement; ko: string; en: string; internal?: boolean }[] = [
  { key: "kpi", label: "KPI", Icon: IconKpi, ko: "KPI 운영 지표", en: "KPI Metrics" },
  { key: "tt", label: "TT", Icon: IconTt, ko: "TT 배차 현황", en: "TT Dispatch" },
  { key: "board", label: "BOARD", Icon: IconBoard, ko: "배차 보드", en: "Dispatch Board" },
  { key: "cycles", label: "CYCLES", Icon: IconCycles, ko: "사이클 이력", en: "Cycle History" },
  { key: "map", label: "MAP", Icon: IconMap, ko: "라이브 맵", en: "Live Map" },
  { key: "learn", label: "LEARN", Icon: IconLearn, ko: "학습 센터", en: "Learning Center", internal: true },
  { key: "stage2", label: "VS TOS", Icon: IconStage2, ko: "배차 비교", en: "Dispatch vs TOS", internal: true },
  { key: "health", label: "HEALTH", Icon: IconHealth, ko: "AI 배차 헬스", en: "Dispatch Health", internal: true },
  { key: "feed", label: "FEED", Icon: IconFeed, ko: "WS 데이터 헬스", en: "WS Data Health", internal: true },
];

const INTERNAL_LS_KEY = "tt_internal_mode";
const internalFromStorage = () => localStorage.getItem(INTERNAL_LS_KEY) === "1";

// URL 해시 딥링크(P3 후속): 주소 뒤 #board 처럼 탭을 지정해 공유할 수 있다 —
// 관제에 배차 보드 URL 을 그대로 건네는 용도(라우터 없이 최소 구현).
const pageFromHash = (internalOn: boolean): PageKey => {
  const h = window.location.hash.replace("#", "");
  const p = PAGES.find((p) => p.key === h);
  return p && (internalOn || !p.internal) ? p.key : "kpi";
};

export default function App() {
  const [lang, setLang] = useState<Lang>("en");
  // #internal 진입은 여기(state 초기화)서 잡아야 한다 — 마운트 효과 순서상 아래
  // replaceState 가 해시를 #kpi 로 덮어쓴 뒤에야 hashchange 핸들러가 붙기 때문.
  const [internal, setInternal] = useState<boolean>(() => {
    if (window.location.hash === "#internal") { localStorage.setItem(INTERNAL_LS_KEY, "1"); return true; }
    return internalFromStorage();
  });
  const [page, setPage] = useState<PageKey>(() => pageFromHash(internalFromStorage()));
  useEffect(() => { window.history.replaceState(null, "", `#${page}`); }, [page]);
  useEffect(() => {
    const onHash = () => {
      if (window.location.hash === "#internal") {
        localStorage.setItem(INTERNAL_LS_KEY, "1");
        setInternal(true);
        setPage(pageFromHash(true)); // #internal 자체는 페이지가 아니라 kpi 로
        return;
      }
      setPage(pageFromHash(internal));
    };
    onHash(); // 초기 진입이 #internal 인 경우도 처리
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, [internal]);
  const exitInternal = () => {
    localStorage.removeItem(INTERNAL_LS_KEY);
    setInternal(false);
    if (PAGES.find((p) => p.key === page)?.internal) setPage("kpi");
  };
  const visiblePages = PAGES.filter((p) => internal || !p.internal);
  const clock = useClock();
  const health = useHealth();
  const feed = useFeedHealth();
  useMemo(() => { document.documentElement.setAttribute("data-lang", lang); }, [lang]);
  const pageName = (p: typeof PAGES[number]) => (lang === "ko" ? p.ko : p.en);

  return (
    <FeedHealthContext.Provider value={feed}>
      <div className="header">
        <img className="brand-logo" src="/clt-logo-w.png" alt="CLT" />
        <span className="brand-div" />
        <span className="logo">TT <span className="accent">AiOps</span><span className="logo-sub">Platform</span></span>
        <span className="site-chip" title={lang === "ko" ? "제품이 적용된 사이트" : "deployment site"}>
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
            <path d="M12 21s7-5.5 7-11a7 7 0 1 0-14 0c0 5.5 7 11 7 11Z" /><circle cx="12" cy="10" r="2.5" />
          </svg>
          <span className="site-k">SITE</span>Westports Malaysia
        </span>
        <span className="spacer" />
        {internal && (
          <button
            className="internal-chip"
            onClick={exitInternal}
            title={lang === "ko"
              ? "내부 모드 — 내부 전용 탭이 보입니다. 클릭하면 고객 화면으로 (다시 켜기: 주소 뒤 #internal)"
              : "Internal mode — internal-only tabs are visible. Click for customer view (re-enable: #internal)"}
          >INTERNAL ×</button>
        )}
        <HealthPill health={health} feed={feed} lang={lang} />
        <span className="clock">{clock}</span>
        <div className="lang-toggle">
          <button className={`lang-btn${lang === "ko" ? " active" : ""}`} onClick={() => setLang("ko")}>KO</button>
          <button className={`lang-btn${lang === "en" ? " active" : ""}`} onClick={() => setLang("en")}>EN</button>
        </div>
      </div>
      <OutageBanner feed={feed} lang={lang} />
      <OpsAlertBanner lang={lang} />
      <div className="app-body">
        <nav className="sidebar">
          {visiblePages.map((p) => (
            <button key={p.key} className={`side-item${page === p.key ? " active" : ""}${p.internal ? " internal" : ""}`} onClick={() => setPage(p.key)} title={pageName(p)}>
              <p.Icon />
              <span className="label">{p.label}</span>
            </button>
          ))}
          <span className="side-spacer" />
        </nav>
        <div className="main-col">
          <div className="tabbar">
            {visiblePages.map((p) => (
              <button key={p.key} className={`ptab${page === p.key ? " active" : ""}${p.internal ? " internal" : ""}`} onClick={() => setPage(p.key)}>
                <p.Icon /><span>{pageName(p)}</span>
              </button>
            ))}
          </div>
          {page === "kpi" ? <KpiPage lang={lang} /> : page === "board" ? <BoardPage lang={lang} /> : page === "tt" ? <TtPage lang={lang} /> : page === "stage2" ? <Stage2Page lang={lang} /> : page === "cycles" ? <CyclesPage lang={lang} /> : page === "learn" ? <LearnPage lang={lang} /> : page === "map" ? <LiveMapPage lang={lang} internal={internal} /> : page === "health" ? <HealthPage lang={lang} /> : <FeedHealthPage lang={lang} />}
        </div>
      </div>
    </FeedHealthContext.Provider>
  );
}
