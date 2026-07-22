// TT work-cycle history. Reads tt_cycle_recon (via /api/tt-cycles/*).
// A "cycle" = one PHYSICAL TRIP with 3 TOS event points (tt_move_log): 작업할당(dispatch_ts) →
// 픽업완료(pickup_ts) → drop-off완료(free_ts). Between them the websocket (GPS) splits each span into
// 주행(moving) vs 대기(the rest): 공차주행 | 부하주행. Twins collapsed; empty+laden = cycle_s.
// Left: fleet overview + truck leaderboard. Right: the selected truck's cycles as phase bars.
import { useEffect, useMemo, useRef, useState } from "react";
import { type Lang } from "./i18n";
import { api, type CycleSummary, type CycleDetail, type CycleTruckAgg, type CycleRow } from "./api";
import { LineChart } from "./charts";

const ko = (lang: Lang) => lang === "ko";

// job-type palette (leaderboard mix + job chip)
const JOB: Record<string, { c: string; ko: string; en: string }> = {
  LD: { c: "#0ea5e9", ko: "적하", en: "Load" },
  DS: { c: "#f59e0b", ko: "양하", en: "Disch" },
  MI: { c: "#a78bfa", ko: "야드 입고", en: "Yard in" },
  MO: { c: "#c084fc", ko: "야드 출고", en: "Yard out" },
  LC: { c: "#34d399", ko: "야드 이동", en: "Yard move" },
};
const jobColor = (j: string | null | undefined) => JOB[(j ?? "").toUpperCase()]?.c ?? "#64748b";

// segment colours: two drive shades (empty vs laden) + one wait tone. Non-driving time in a span = 대기.
const SEG = { eDrive: "#38bdf8", lDrive: "#0369a1", wait: "#475569" } as const;
const LEGEND = [
  { c: SEG.eDrive, ko: "공차 주행", en: "Empty drive" },
  { c: SEG.lDrive, ko: "부하 주행", en: "Laden drive" },
  { c: SEG.wait, ko: "대기", en: "Wait" },
] as const;

const secBetween = (a?: string | null, b?: string | null): number =>
  a && b ? Math.max(0, (new Date(b).getTime() - new Date(a).getTime()) / 1000) : 0;

type Seg = { key: string; sec: number; color: string; label: (l: Lang) => string };

// 5-stage model: 작업할당(dispatch_ts) ─[공차주행 + 공차대기]─ 픽업완료(pickup_ts) ─[부하주행 + 부하대기]─
// drop-off완료(free_ts). The two drive spans are websocket-derived (e_drive_s / l_drive_s); the remainder of
// each TOS span is 대기. Geometry is always returned; segs=null when GPS didn't observe the split (gps_covered=false).
function tripSegs(c: CycleRow): { segs: Seg[] | null; total: number; pickupFrac: number | null } {
  const emptyS = secBetween(c.dispatch_ts, c.pickup_ts);
  const ladenS = secBetween(c.pickup_ts, c.free_ts);
  const total = emptyS + ladenS || (c.cycle_s ?? 1);
  const pickupFrac = c.pickup_ts && total > 0 ? emptyS / total : null;
  if (!c.gps_covered || !c.pickup_ts) return { segs: null, total, pickupFrac };
  const eDrive = Math.min(c.e_drive_s ?? 0, emptyS);
  const lDrive = Math.min(c.l_drive_s ?? 0, ladenS);
  const segs: Seg[] = [
    { key: "e_drive", sec: eDrive, color: SEG.eDrive, label: (l: Lang) => (ko(l) ? "공차 주행" : "Empty drive") },
    { key: "e_wait", sec: emptyS - eDrive, color: SEG.wait, label: (l: Lang) => (ko(l) ? "공차 대기" : "Empty wait") },
    { key: "l_drive", sec: lDrive, color: SEG.lDrive, label: (l: Lang) => (ko(l) ? "부하 주행" : "Laden drive") },
    { key: "l_wait", sec: ladenS - lDrive, color: SEG.wait, label: (l: Lang) => (ko(l) ? "부하 대기" : "Laden wait") },
  ].filter((s) => s.sec > 0.5);
  return { segs, total, pickupFrac };
}
const driveKm = (c: CycleRow) => (c.e_drive_m + c.l_drive_m) / 1000;

const mmss = (s: number | null | undefined) =>
  s == null ? "—" : `${Math.floor(s / 60)}:${String(Math.round(s % 60)).padStart(2, "0")}`;
const hhmm = (iso: string | null | undefined) =>
  iso ? new Date(iso).toLocaleTimeString([], { timeZone: "Asia/Kuala_Lumpur", hour: "2-digit", minute: "2-digit", hour12: false }) : "—";

const RANGES = [
  { h: 1, ko: "1시간", en: "1h" },
  { h: 4, ko: "4시간", en: "4h" },
  { h: 12, ko: "12시간", en: "12h" },
  { h: 24, ko: "24시간", en: "24h" },
  { h: 72, ko: "3일", en: "3d" },
];

function Tile({ label, value, unit, accent }: { label: string; value: string; unit?: string; accent?: string }) {
  return (
    <div className="cyc-tile" style={accent ? { borderTopColor: accent } : undefined}>
      <div className="cyc-tile-l">{label}</div>
      <div className="cyc-tile-v">
        {value}
        {unit && <span className="cyc-tile-u">{unit}</span>}
      </div>
    </div>
  );
}

function TruckRow({ t, max, sel, onSel, lang }: { t: CycleTruckAgg; max: number; sel: boolean; onSel: () => void; lang: Lang }) {
  const pct = max > 0 ? (t.cycles / max) * 100 : 0;
  const tot = t.ds + t.ld + t.other || 1;
  return (
    <button className={`cyc-trow${sel ? " sel" : ""}`} onClick={onSel}>
      <span className="cyc-trow-id mono">{t.ytno}</span>
      <span className="cyc-trow-bar">
        <span className="cyc-trow-fill" style={{ width: `${pct}%` }}>
          <span className="cyc-seg" style={{ flex: t.ld, background: JOB.LD.c }} />
          <span className="cyc-seg" style={{ flex: t.ds, background: JOB.DS.c }} />
          <span className="cyc-seg" style={{ flex: t.other, background: "#64748b" }} />
        </span>
      </span>
      <span className="cyc-trow-n mono">{t.cycles}</span>
      <span className="cyc-trow-med mono" title={ko(lang) ? "중위 사이클" : "median cycle"}>{mmss(t.median_s)}</span>
      <span className="cyc-trow-km mono">{t.drive_km != null ? t.drive_km.toFixed(1) : "—"}</span>
      <span className="cyc-trow-spark"><span style={{ width: `${tot ? (t.ld / tot) * 100 : 0}%` }} /></span>
    </button>
  );
}

// one trip as a sequential phase bar (durations ∝ width, shared scale across trips). gps_covered=false
// renders a single muted "no GPS detail" bar (only cycle_s is known).
function CycleLane({ c, scale, lang }: { c: CycleRow; scale: number; lang: Lang }) {
  const trip = tripSegs(c);
  // contnos is the single source of truth for BOTH the ×N badge count and the listed IDs, so they can never
  // disagree. Fallback to the single representative container only if contnos is absent (defensive).
  const boxes = c.contnos && c.contnos.length ? c.contnos : (c.container ? [c.container] : []);
  const nb = boxes.length;
  const barW = scale > 0 ? (trip.total / scale) * 100 : 100;
  // twin intermediate crane events (A→B→C): render each as a tick along the bar (empty for singles)
  const wps = (c.waypoint_ts ?? []).map((ts, i) => ({
    ts, crane: c.waypoint_crane?.[i] ?? null, kind: c.waypoint_kind?.[i] ?? "drop",
    frac: trip.total > 0 ? secBetween(c.dispatch_ts, ts) / trip.total : 0,
  })).filter((w) => w.frac > 0.002 && w.frac < 0.998);
  return (
    <div className="cyc-lane">
      <span className="cyc-lane-time mono">{hhmm(c.free_ts)}</span>
      <span className="cyc-lane-track" style={{ position: "relative", display: "block" }}>
        <span style={{ position: "relative", display: "block", width: `${barW}%`, height: "100%" }}>
          {trip.segs ? (
            <span style={{ display: "flex", width: "100%", height: "100%" }}>
              {trip.segs.map((s, i) => (
                <span
                  key={s.key + i}
                  style={{ flex: `0 0 ${trip.total > 0 ? (s.sec / trip.total) * 100 : 0}%`, background: s.color, height: "100%" }}
                  title={`${s.label(lang)} · ${mmss(s.sec)}`}
                />
              ))}
            </span>
          ) : (
            <span
              style={{ display: "block", width: "100%", height: "100%", background: "repeating-linear-gradient(45deg,#334155,#334155 5px,#1e293b 5px,#1e293b 10px)", opacity: 0.6 }}
              title={ko(lang) ? "GPS 상세 없음 (정지·침묵/보존만료)" : "no GPS detail (silent / aged out)"}
            />
          )}
          {trip.pickupFrac != null && (
            <span
              style={{ position: "absolute", top: 0, bottom: 0, left: `${trip.pickupFrac * 100}%`, width: 2, background: "#e2e8f0", transform: "translateX(-1px)", pointerEvents: "none" }}
              title={ko(lang) ? `픽업 완료 ${hhmm(c.pickup_ts)}` : `Pickup ${hhmm(c.pickup_ts)}`}
            />
          )}
          {wps.map((w, i) => (
            <span
              key={"wp" + i}
              style={{ position: "absolute", top: 0, bottom: 0, left: `${w.frac * 100}%`, width: 2, background: "#f59e0b", transform: "translateX(-1px)", pointerEvents: "none" }}
              title={`${w.kind === "pickup" ? (ko(lang) ? "중간 픽업" : "pickup") : (ko(lang) ? "중간 드롭" : "drop")}${w.crane ? " " + w.crane : ""} · ${hhmm(w.ts)}`}
            />
          ))}
        </span>
      </span>
      <span className="cyc-lane-meta">
        {c.jobtype && <span className="cyc-lane-job" style={{ borderColor: jobColor(c.jobtype), color: jobColor(c.jobtype) }}>{c.jobtype.toUpperCase()}</span>}
        {nb > 1 && <span className="cyc-lane-qc mono" title={ko(lang) ? `트윈 (${nb}컨테이너 1트립): ${boxes.join(", ")}` : `twin (${nb} boxes / 1 trip): ${boxes.join(", ")}`}>×{nb}</span>}
        {c.free_crane && <span className="cyc-lane-qc mono">{c.free_crane}</span>}
        {nb > 0 && <span className="cyc-lane-cnt mono" title={nb > 1 ? boxes.join(", ") : undefined}>{boxes.join(" + ")}</span>}
        {trip.segs && driveKm(c) > 0 && <span className="cyc-lane-vsl mono" title={ko(lang) ? "주행 거리" : "driven distance"}>{driveKm(c).toFixed(1)}km</span>}
      </span>
      <span className="cyc-lane-dur mono">{mmss(c.cycle_s)}</span>
    </div>
  );
}

function TruckDetail({ ytno, hours, lang }: { ytno: string; hours: number; lang: Lang }) {
  const [det, setDet] = useState<CycleDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const cur = useRef("");
  useEffect(() => {
    let alive = true;
    cur.current = ytno;
    setLoading(true);
    api.cycleDetail(ytno, hours, 120).then((d) => { if (alive && cur.current === ytno) { setDet(d); setLoading(false); } }).catch(() => alive && setLoading(false));
    return () => { alive = false; };
  }, [ytno, hours]);

  const cycles = det?.cycles ?? [];
  // shared time scale across the shown trips = the longest cycle, so bars compare 1:1
  const scale = useMemo(() => Math.max(1, ...cycles.map((c) => c.cycle_s ?? 0)), [cycles]);
  const trend = useMemo(() => [...cycles].reverse().map((c) => c.cycle_s ?? 0).filter((v) => v > 0), [cycles]);
  const stats = useMemo(() => {
    if (!cycles.length) return null;
    const ld = cycles.filter((c) => c.jobtype === "LD").length;
    const ds = cycles.filter((c) => c.jobtype === "DS").length;
    const other = cycles.length - ld - ds;
    const km = cycles.reduce((a, c) => a + driveKm(c), 0);
    const med = [...cycles.map((c) => c.cycle_s ?? 0)].filter((v) => v > 0).sort((a, b) => a - b);
    const median = med.length ? med[Math.floor(med.length / 2)] : null;
    // fleet drive vs wait split over the covered trips
    const cov = cycles.filter((c) => c.gps_covered);
    const drv = cov.reduce((a, c) => a + c.e_drive_s + c.l_drive_s, 0);
    const tot = cov.reduce((a, c) => a + (c.cycle_s ?? 0), 0);
    const driveFrac = tot > 0 ? drv / tot : null;
    return { ld, ds, other, km, median, span: cycles.length, driveFrac, covPct: cycles.length ? (cov.length / cycles.length) * 100 : 0 };
  }, [cycles]);

  return (
    <div className="cyc-detail">
      <div className="cyc-detail-head">
        <div className="cyc-detail-id">
          <span className="mono">{ytno}</span>
          {stats && <span className="cyc-detail-sub">{ko(lang)
            ? `${stats.span}트립 · 중위 ${mmss(stats.median)} · 주행 ${stats.km.toFixed(1)}km${stats.driveFrac != null ? ` · 주행비율 ${(stats.driveFrac * 100).toFixed(0)}%` : ""}`
            : `${stats.span} trips · median ${mmss(stats.median)} · ${stats.km.toFixed(1)}km driven${stats.driveFrac != null ? ` · ${(stats.driveFrac * 100).toFixed(0)}% driving` : ""}`}</span>}
        </div>
        {stats && (
          <div className="cyc-detail-split">
            <span style={{ color: JOB.LD.c }}>● {ko(lang) ? "적하" : "LD"} {stats.ld}</span>
            <span style={{ color: JOB.DS.c }}>● {ko(lang) ? "양하" : "DS"} {stats.ds}</span>
            {stats.other > 0 && <span style={{ color: "#64748b" }}>● {ko(lang) ? "기타" : "Other"} {stats.other}</span>}
          </div>
        )}
      </div>

      {trend.length > 1 && (
        <div className="cyc-detail-trend">
          <div className="cyc-sec-h">{ko(lang) ? "사이클타임 추이 (초)" : "Cycle time trend (s)"}</div>
          <div className="cyc-trend-box"><LineChart values={trend} color="#60a5fa" axes /></div>
        </div>
      )}

      <div className="cyc-phase-h">
        <span className="cyc-sec-h">{ko(lang) ? "트립 단계별 (최신순)" : "Trips by phase (latest first)"}</span>
        <span className="cyc-phase-legend">
          {LEGEND.map((p) => (
            <span key={p.en}><span className="cyc-dot" style={{ background: p.c }} />{ko(lang) ? p.ko : p.en}</span>
          ))}
        </span>
      </div>
      <div className="cyc-sec-h" style={{ fontWeight: 400, opacity: 0.7, marginTop: -2, display: "flex", flexWrap: "wrap", alignItems: "center", gap: "4px 16px" }}>
        <span>{ko(lang) ? "작업할당 → 공차주행 → 픽업완료 → 부하주행 → drop-off완료" : "dispatch → empty drive → pickup → laden drive → drop"}</span>
        <span style={{ display: "inline-flex", alignItems: "center", gap: 5 }}>
          <span style={{ display: "inline-block", width: 2, height: 11, background: "#e2e8f0", flex: "none" }} />
          {ko(lang) ? "픽업 시점" : "pickup"}
        </span>
        <span style={{ display: "inline-flex", alignItems: "center", gap: 5 }}>
          <span style={{ display: "inline-block", width: 2, height: 11, background: "#f59e0b", flex: "none" }} />
          {ko(lang) ? "트윈 중간 경유" : "twin waypoint"}
        </span>
        <span style={{ display: "inline-flex", alignItems: "center", gap: 5 }}>
          <span style={{ display: "inline-block", width: 13, height: 11, flex: "none", background: "repeating-linear-gradient(45deg,#334155,#334155 3px,#1e293b 3px,#1e293b 6px)" }} />
          {ko(lang) ? "GPS 상세 없음" : "no GPS detail"}
        </span>
      </div>
      <div className="cyc-lanes">
        {loading && <div className="cyc-empty">{ko(lang) ? "불러오는 중…" : "loading…"}</div>}
        {!loading && cycles.length === 0 && <div className="cyc-empty">{ko(lang) ? "이 범위에 트립 없음" : "no trips in range"}</div>}
        {cycles.map((c, i) => <CycleLane key={c.free_ts + i} c={c} scale={scale} lang={lang} />)}
      </div>
    </div>
  );
}

export default function CyclesPage({ lang }: { lang: Lang }) {
  const [hours, setHours] = useState(12);
  const [sum, setSum] = useState<CycleSummary | null>(null);
  const [sel, setSel] = useState<string>("");
  const [q, setQ] = useState("");
  const [err, setErr] = useState(false);

  useEffect(() => {
    let alive = true;
    const load = () => api.cycleSummary(hours).then((s) => { if (alive) { setSum(s); setErr(false); } }).catch(() => alive && setErr(true));
    load();
    const id = setInterval(load, 30000);
    return () => { alive = false; clearInterval(id); };
  }, [hours]);

  // auto-select the busiest truck once data lands
  useEffect(() => {
    if (sum && sum.trucks_list.length && !sum.trucks_list.some((t) => t.ytno === sel)) {
      setSel(sum.trucks_list[0].ytno);
    }
  }, [sum]); // eslint-disable-line react-hooks/exhaustive-deps

  const list = sum?.trucks_list ?? [];
  const maxCycles = list.reduce((a, t) => Math.max(a, t.cycles), 0);
  const filtered = q ? list.filter((t) => t.ytno.toLowerCase().includes(q.toLowerCase())) : list;
  const tpVals = (sum?.buckets ?? []).map((b) => b.n);
  const tpLabels = (sum?.buckets ?? []).map((b) => hhmm(b.t));

  return (
    <div className="content cyc-page">
      <div className="cyc-head">
        <div className="cyc-title">
          <h2>{ko(lang) ? "TT 작업 사이클 이력" : "TT Work-Cycle History"}</h2>
          <span className="cyc-title-sub">{ko(lang) ? "물리 트립 단위 · TOS 이벤트 3점(할당·픽업·드롭) + GPS 주행/대기 분해" : "per physical trip · 3 TOS events (assign·pickup·drop) + GPS drive/wait split"}</span>
        </div>
        <div className="cyc-range">
          {RANGES.map((r) => (
            <button key={r.h} className={`cyc-range-btn${hours === r.h ? " active" : ""}`} onClick={() => setHours(r.h)}>{ko(lang) ? r.ko : r.en}</button>
          ))}
        </div>
      </div>

      <div className="cyc-tiles">
        <Tile label={ko(lang) ? "총 트립" : "Total trips"} value={sum ? String(sum.total_cycles) : "—"} accent="#60a5fa" />
        <Tile label={ko(lang) ? "가동 트럭" : "Active trucks"} value={sum ? String(sum.trucks) : "—"} accent="#0ea5e9" />
        <Tile label={ko(lang) ? "시간당 트립" : "Trips / hr"} value={sum ? sum.cycles_per_hr.toFixed(1) : "—"} accent="#34d399" />
        <Tile label={ko(lang) ? "플릿 중위 사이클" : "Fleet median"} value={sum ? mmss(sum.fleet_median_s) : "—"} accent="#f59e0b" />
        <Tile label={ko(lang) ? "총 주행 거리" : "Driven distance"} value={sum ? sum.fleet_drive_km.toFixed(0) : "—"} unit="km" accent="#a78bfa" />
      </div>

      <div className="cyc-tp">
        <div className="cyc-sec-h">
          {ko(lang) ? `처리량 추이 · ${sum?.bucket_min ?? "—"}분 단위` : `Throughput · per ${sum?.bucket_min ?? "—"} min`}
          {err && <span className="cyc-err">{ko(lang) ? " · 연결 오류" : " · offline"}</span>}
        </div>
        <div className="cyc-tp-box">
          {tpVals.length > 1 ? <LineChart values={tpVals} labels={tpLabels} color="#38bdf8" axes /> : <div className="cyc-empty">{ko(lang) ? "데이터 수집 중" : "collecting"}</div>}
        </div>
      </div>

      <div className="cyc-body">
        <div className="cyc-board">
          <div className="cyc-board-head">
            <span>{ko(lang) ? "트럭별 (트립 많은 순)" : "By truck (most trips)"}</span>
            <input className="cyc-search mono" placeholder={ko(lang) ? "TT 검색" : "find TT"} value={q} onChange={(e) => setQ(e.target.value)} />
          </div>
          <div className="cyc-board-cols">
            <span>TT</span><span>{ko(lang) ? "분포" : "mix"}</span><span>{ko(lang) ? "회" : "n"}</span><span>{ko(lang) ? "중위" : "med"}</span><span>km</span><span></span>
          </div>
          <div className="cyc-board-list">
            {filtered.length === 0 && <div className="cyc-empty">{ko(lang) ? "없음" : "none"}</div>}
            {filtered.map((t) => <TruckRow key={t.ytno} t={t} max={maxCycles} sel={t.ytno === sel} onSel={() => setSel(t.ytno)} lang={lang} />)}
          </div>
          <div className="cyc-legend">
            {Object.entries(JOB).slice(0, 4).map(([k, v]) => (
              <span key={k}><span className="cyc-dot" style={{ background: v.c }} />{ko(lang) ? v.ko : v.en}</span>
            ))}
          </div>
        </div>

        <div className="cyc-pane">
          {sel ? <TruckDetail ytno={sel} hours={hours} lang={lang} /> : <div className="cyc-empty">{ko(lang) ? "트럭을 선택하세요" : "select a truck"}</div>}
        </div>
      </div>
    </div>
  );
}
