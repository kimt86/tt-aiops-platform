// TT work-cycle history. Reads tt_cycle_recon (via /api/tt-cycles/*).
// A "cycle" = one PHYSICAL TRIP: boundaries are TOS-authoritative (tt_move_log dispatch→last free,
// twins collapsed), and each trip is split by GPS into 7 phases that reconcile exactly to cycle_s:
//   배차대기 → 공차주행 → 공차정지 → 픽업대기 → 부하주행 → 부하정지(큐) → 드롭대기.
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

// 3 semantic families (bar colours + legend); the 7 phases map onto them by kind.
const FAM = { drive: "#0ea5e9", stop: "#f59e0b", wait: "#64748b" } as const;
const FAM_LABEL = [
  { k: "drive", c: FAM.drive, ko: "주행", en: "Driving" },
  { k: "stop", c: FAM.stop, ko: "정지·큐", en: "Stop/queue" },
  { k: "wait", c: FAM.wait, ko: "대기", en: "Wait" },
] as const;
// the 7 phases, in trip order. `f` = the CycleRow field (seconds); `fam` = colour family.
const PHASES = [
  { field: "dispatch_wait_s", fam: "wait", ko: "배차 대기", en: "Dispatch wait" },
  { field: "e_drive_s", fam: "drive", ko: "공차 주행", en: "Empty drive" },
  { field: "e_stop_s", fam: "stop", ko: "공차 정지", en: "Empty stop" },
  { field: "pickup_dwell_s", fam: "wait", ko: "픽업 대기", en: "Pickup dwell" },
  { field: "l_drive_s", fam: "drive", ko: "부하 주행", en: "Laden drive" },
  { field: "l_stop_s", fam: "stop", ko: "부하 정지(큐)", en: "Laden queue" },
  { field: "drop_dwell_s", fam: "wait", ko: "드롭 대기", en: "Drop dwell" },
] as const;

// Build the sequential phase segments (durations) for one trip. Returns null when GPS did not observe
// a drive segment (gps_covered=false) — the split is unavailable, only cycle_s is meaningful.
function tripPhases(c: CycleRow): { key: string; sec: number; fam: string; label: (l: Lang) => string }[] | null {
  if (!c.gps_covered) return null;
  return PHASES.map((p) => ({
    key: p.field,
    sec: (c[p.field] as number) ?? 0,
    fam: p.fam,
    label: (l: Lang) => (ko(l) ? p.ko : p.en),
  })).filter((s) => s.sec > 0);
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
  const segs = tripPhases(c);
  // contnos is the single source of truth for BOTH the ×N badge count and the listed IDs, so they can never
  // disagree. Fallback to the single representative container only if contnos is absent (defensive).
  const boxes = c.contnos && c.contnos.length ? c.contnos : (c.container ? [c.container] : []);
  const nb = boxes.length;
  return (
    <div className="cyc-lane">
      <span className="cyc-lane-time mono">{hhmm(c.free_ts)}</span>
      <span className="cyc-lane-track" style={{ position: "relative", display: "block" }}>
        {segs ? (
          <span style={{ display: "flex", width: `${scale > 0 ? ((c.cycle_s ?? 0) / scale) * 100 : 100}%`, height: "100%" }}>
            {segs.map((s, i) => (
              <span
                key={s.key + i}
                style={{ flex: `0 0 ${(c.cycle_s ?? 1) > 0 ? (s.sec / (c.cycle_s ?? 1)) * 100 : 0}%`, background: FAM[s.fam as keyof typeof FAM], height: "100%" }}
                title={`${s.label(lang)} · ${mmss(s.sec)}`}
              />
            ))}
          </span>
        ) : (
          <span
            style={{ display: "block", width: `${scale > 0 ? ((c.cycle_s ?? 0) / scale) * 100 : 100}%`, height: "100%", background: "repeating-linear-gradient(45deg,#334155,#334155 5px,#1e293b 5px,#1e293b 10px)", opacity: 0.6 }}
            title={ko(lang) ? "GPS 상세 없음 (정지·침묵/보존만료)" : "no GPS detail (silent / aged out)"}
          />
        )}
      </span>
      <span className="cyc-lane-meta">
        {c.jobtype && <span className="cyc-lane-job" style={{ borderColor: jobColor(c.jobtype), color: jobColor(c.jobtype) }}>{c.jobtype.toUpperCase()}</span>}
        {nb > 1 && <span className="cyc-lane-qc mono" title={ko(lang) ? `트윈 (${nb}컨테이너 1트립): ${boxes.join(", ")}` : `twin (${nb} boxes / 1 trip): ${boxes.join(", ")}`}>×{nb}</span>}
        {c.free_crane && <span className="cyc-lane-qc mono">{c.free_crane}</span>}
        {nb > 0 && <span className="cyc-lane-cnt mono" title={nb > 1 ? boxes.join(", ") : undefined}>{boxes.join(" + ")}</span>}
        {segs && driveKm(c) > 0 && <span className="cyc-lane-vsl mono" title={ko(lang) ? "주행 거리" : "driven distance"}>{driveKm(c).toFixed(1)}km</span>}
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
          {FAM_LABEL.map((p) => (
            <span key={p.k}><span className="cyc-dot" style={{ background: p.c }} />{ko(lang) ? p.ko : p.en}</span>
          ))}
        </span>
      </div>
      <div className="cyc-sec-h" style={{ fontWeight: 400, opacity: 0.7, marginTop: -2 }}>
        {ko(lang) ? "배차대기 → 공차주행 → 공차정지 → 픽업대기 → 부하주행 → 부하정지(큐) → 드롭대기 · 빗금 = GPS 상세 없음" : "dispatch → empty drive → empty stop → pickup → laden drive → laden queue → drop · hatched = no GPS detail"}
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
          <span className="cyc-title-sub">{ko(lang) ? "물리 트립 단위 · TOS 경계(tt_move_log) + GPS 주행/정지 분해" : "per physical trip · TOS boundaries + GPS drive/stop split"}</span>
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
