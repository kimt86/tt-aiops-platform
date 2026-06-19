import { useEffect, useMemo, useState, type ReactElement } from "react";
import { api, type BreakdownResponse, type HealthResponse, type KpiCard, type KpisResponse, type LiveKpi, type LiveResponse, type QcRow, type TrendResponse, type VesselRow, type VesselsResponse } from "./api";
import { LineChart } from "./charts";
import { deltaLabel, fmtValue, isImprovement } from "./format";
import { t, type Lang } from "./i18n";
import TtPage from "./TtPage";
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

function HealthPill({ health, lang }: { health: HealthResponse | null; lang: Lang }) {
  const s = t(lang);
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

function SmallCard({ c, trend, lang, ws, extras }: { c: KpiCard; trend?: TrendResponse; lang: Lang; ws?: WsLive | null; extras?: KExtra[] }) {
  const s = t(lang);
  const ko = lang === "ko";
  const imp = isImprovement(c);
  const dl = deltaLabel(c);
  // K_QC_TT_WAIT card: secondary = live GPS-verified QC starvation (K_QC_TT_WAIT_GPS)
  const gpsAux = c.key === "K_QC_TT_WAIT" && ws?.connected
    ? ((ws.qc_starving ?? 0) > 0 ? `${ws.qc_starving}${ko ? "대" : ""} · ${ws.qc_wait_live_s}s` : (ko ? "없음" : "none"))
    : null;
  return (
    <div className={`kpi${c.tier === "PRIMARY" ? " primary" : ""}`}>
      <div className="label">{name(c, lang)}<SourceBadge src={gpsAux != null ? "dual" : "tos"} ko={ko} /></div>
      <div className="vrow">
        <span className="val">{mainValue(c.key, c.value, c.unit).val}</span>
        <span className="unit">{mainValue(c.key, c.value, c.unit).unit}</span>
      </div>
      <KpiExtras extras={extras} />
      {gpsAux != null && <WsAux val={gpsAux} title={ko ? "K_QC_TT_WAIT_GPS — 지금 트럭을 기다리는 가동 QC 수 · 평균 대기. GPS로 40m 내 트럭 없음 + 그 크레인이 남은 작업 보유 + 3분 평균(라이브 신호 GPS 교체·평활)" : "K_QC_TT_WAIT_GPS — QC starving now: count · avg wait. No truck within 40m (GPS) + crane has pending work + 3-min smoothed"} ko={ko} />}
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
  if (key === "K_QC_TT_WAIT")
    return { kind: "dual", auxVal: (w.qc_starving ?? 0) > 0 ? `${w.qc_starving}${ko ? "대" : ""} · ${w.qc_wait_live_s}s` : (ko ? "없음" : "none"), auxTitle: ko ? "K_QC_TT_WAIT_GPS — 지금 트럭을 기다리는 가동 QC 수 · 평균 대기. GPS로 40m 내 트럭 없음 + 그 크레인이 남은 작업 보유 + 3분 평균(라이브 GPS 교체·평활)" : "K_QC_TT_WAIT_GPS — QC starving now: count · avg wait. No truck within 40m (GPS) + crane has pending work + 3-min smoothed" };
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
      <div className="section-title" style={{ marginTop: 18 }}>{s.activeVessels}<span className="section-sub">{vrows.length}</span></div>
      <VesselPanel vessels={vrows} lang={lang} onSelect={(v) => setSelKey(vkey(v))} />

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
  const ws = useWsLive(); // for the K_QC_TT_WAIT card's live GPS-verified secondary (K_QC_TT_WAIT_GPS)
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
              {kpis.map((c) => <SmallCard key={c.key} c={c} trend={data.trends[c.key]} lang={lang} ws={ws} extras={distanceExtras(kpis, c.key, lang)} />)}
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

type PageKey = "kpi" | "tt" | "cycles" | "learn" | "map" | "health" | "feed";
const PAGES: { key: PageKey; label: string; Icon: () => ReactElement; ko: string; en: string }[] = [
  { key: "kpi", label: "KPI", Icon: IconKpi, ko: "KPI 운영 지표", en: "KPI Metrics" },
  { key: "tt", label: "TT", Icon: IconTt, ko: "TT 배차 현황", en: "TT Dispatch" },
  { key: "cycles", label: "CYCLES", Icon: IconCycles, ko: "사이클 이력", en: "Cycle History" },
  { key: "learn", label: "LEARN", Icon: IconLearn, ko: "학습 센터", en: "Learning Center" },
  { key: "map", label: "MAP", Icon: IconMap, ko: "라이브 맵", en: "Live Map" },
  { key: "health", label: "HEALTH", Icon: IconHealth, ko: "AI 배차 헬스", en: "Dispatch Health" },
  { key: "feed", label: "FEED", Icon: IconFeed, ko: "WS 데이터 헬스", en: "WS Data Health" },
];

export default function App() {
  const [lang, setLang] = useState<Lang>("en");
  const [page, setPage] = useState<PageKey>("kpi");
  const clock = useClock();
  const health = useHealth();
  useMemo(() => { document.documentElement.setAttribute("data-lang", lang); }, [lang]);
  const pageName = (p: typeof PAGES[number]) => (lang === "ko" ? p.ko : p.en);

  return (
    <>
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
        <HealthPill health={health} lang={lang} />
        <span className="clock">{clock}</span>
        <div className="lang-toggle">
          <button className={`lang-btn${lang === "ko" ? " active" : ""}`} onClick={() => setLang("ko")}>KO</button>
          <button className={`lang-btn${lang === "en" ? " active" : ""}`} onClick={() => setLang("en")}>EN</button>
        </div>
      </div>
      <div className="app-body">
        <nav className="sidebar">
          {PAGES.map((p) => (
            <button key={p.key} className={`side-item${page === p.key ? " active" : ""}`} onClick={() => setPage(p.key)} title={pageName(p)}>
              <p.Icon />
              <span className="label">{p.label}</span>
            </button>
          ))}
          <span className="side-spacer" />
        </nav>
        <div className="main-col">
          <div className="tabbar">
            {PAGES.map((p) => (
              <button key={p.key} className={`ptab${page === p.key ? " active" : ""}`} onClick={() => setPage(p.key)}>
                <p.Icon /><span>{pageName(p)}</span>
              </button>
            ))}
          </div>
          {page === "kpi" ? <KpiPage lang={lang} /> : page === "tt" ? <TtPage lang={lang} /> : page === "cycles" ? <CyclesPage lang={lang} /> : page === "learn" ? <LearnPage lang={lang} /> : page === "map" ? <LiveMapPage lang={lang} /> : page === "health" ? <HealthPage lang={lang} /> : <FeedHealthPage lang={lang} />}
        </div>
      </div>
    </>
  );
}
