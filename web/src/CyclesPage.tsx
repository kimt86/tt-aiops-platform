// TT work-cycle history. Reads the accumulated tt_cycle_log via /api/tt-cycles/*.
// Left: fleet overview (KPI tiles + throughput) and a selectable truck leaderboard.
// Right: the selected truck's cycle timeline (each cycle = empty leg + laden leg, colored
// by job type), a cycle-time trend, and a detail table. A "cycle" = one validated
// container delivery (the truck physically carried the box ≥150m).
import { useEffect, useMemo, useRef, useState } from "react";
import { type Lang } from "./i18n";
import { api, type CycleSummary, type CycleDetail, type CycleTruckAgg, type CycleRow } from "./api";
import { LineChart } from "./charts";

const ko = (lang: Lang) => lang === "ko";

// job-type palette (shared with the timeline + legend)
const JOB: Record<string, { c: string; ko: string; en: string }> = {
  LD: { c: "#0ea5e9", ko: "적하", en: "Load" },
  DS: { c: "#f59e0b", ko: "양하", en: "Disch" },
  MI: { c: "#a78bfa", ko: "야드 입고", en: "Yard in" },
  MO: { c: "#c084fc", ko: "야드 출고", en: "Yard out" },
  LC: { c: "#34d399", ko: "야드 이동", en: "Yard move" },
};
const jobColor = (j: string | null | undefined) => JOB[(j ?? "").toUpperCase()]?.c ?? "#64748b";

// The current 5-event cycle model: ①배차 → ②픽업도착 → ③픽업떠남 → ④부하도착 → ⑤드롭.
// The segmented bar shows the 4 segments between those events (unobserved events leave gaps —
// never fabricated). The old v1 4-phase and v2 6-event (with 빈차출발) models are retired.
const PHASES = [
  { key: "empty", ko: "공차이동", en: "Empty travel", c: "#64748b" }, // ①배차 → ②픽업 도착
  { key: "pickup", ko: "받기", en: "Pick up", c: "#22c55e" }, //         ②픽업 도착 → ③픽업 떠남
  { key: "laden", ko: "부하이동", en: "Laden travel", c: "#0ea5e9" }, // ③픽업 떠남 → ④부하 도착
  { key: "drop", ko: "주기", en: "Drop", c: "#f59e0b" }, //              ④부하 도착 → ⑤드롭
] as const;
const PHASE_C: Record<string, string> = Object.fromEntries(PHASES.map((p) => [p.key, p.c]));

// Split one cycle into the 4 segments of the 5-event model, from the v2 event columns. The
// deprecated "빈차 출발" event is not used — the empty phase is 배차→픽업 도착 as ONE segment.
// Absolute-positioned (startSec from the cycle open); unobserved events leave gaps — we never
// fabricate a phase. Returns null when the cycle has no usable 배차/드롭.
//   공차이동 배차(opened)→픽업도착(empty_arrived) · 받기 픽업도착→픽업떠남(pickup_left)
//   부하이동 픽업떠남→부하도착(laden_arrived) · 주기 부하도착→드롭(dropped)
function cyclePhases(c: CycleRow): { totalSec: number; segs: { key: string; startSec: number; sec: number }[] } | null {
  const ms = (s: string | null) => (s ? Date.parse(s) : null);
  const t0 = ms(c.v2_opened_at); //         ①배차
  const drop = ms(c.dropped_at); //          ⑤드롭
  if (t0 == null || drop == null || drop <= t0) return null;
  const ea = ms(c.v2_empty_arrived_at); //   ②픽업 도착
  const pl = ms(c.v2_pickup_left_at); //     ③픽업 떠남
  const la = ms(c.v2_laden_arrived_at); //   ④부하 도착
  const segs: { key: string; startSec: number; sec: number }[] = [];
  const add = (key: string, a: number | null, b: number | null) => {
    if (a != null && b != null && b > a && a >= t0 && b <= drop) segs.push({ key, startSec: (a - t0) / 1000, sec: (b - a) / 1000 });
  };
  add("empty", t0, ea);
  add("pickup", ea, pl);
  add("laden", pl, la);
  add("drop", la, drop);
  return { totalSec: (drop - t0) / 1000, segs };
}

const mmss = (s: number | null | undefined) =>
  s == null ? "—" : `${Math.floor(s / 60)}:${String(Math.round(s % 60)).padStart(2, "0")}`;
const km2 = (m: number | null | undefined) => (m == null ? "—" : (m / 1000).toFixed(2));
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
      <span className="cyc-trow-km mono">{km2(t.laden_km != null ? t.laden_km * 1000 : null)}</span>
      <span className="cyc-trow-spark"><span style={{ width: `${tot ? (t.ld / tot) * 100 : 0}%` }} /></span>
    </button>
  );
}

// one cycle as a single segmented bar (5-event model, absolute-positioned; unobserved phases
// show as gaps). width ∝ seconds, shared scale across cycles.
function CycleLane({ c, scale, lang }: { c: CycleRow; scale: number; lang: Lang }) {
  const ph = cyclePhases(c);
  const name = (k: string) => { const p = PHASES.find((x) => x.key === k)!; return ko(lang) ? p.ko : p.en; };
  return (
    <div className="cyc-lane">
      <span className="cyc-lane-time mono">{hhmm(c.dropped_at)}</span>
      <span className="cyc-lane-track">
        {ph &&
          ph.segs.map((s) => (
            <span
              key={s.key}
              className="cyc-seg-abs"
              style={{ left: `${scale > 0 ? (s.startSec / scale) * 100 : 0}%`, width: `${scale > 0 ? (s.sec / scale) * 100 : 0}%`, background: PHASE_C[s.key] }}
              title={`${name(s.key)} · ${mmss(s.sec)}`}
            />
          ))}
      </span>
      <span className="cyc-lane-meta">
        {c.jobtype && <span className="cyc-lane-job" style={{ borderColor: jobColor(c.jobtype), color: jobColor(c.jobtype) }}>{c.jobtype.toUpperCase()}</span>}
        {c.vessel && <span className="cyc-lane-vsl">{c.vessel}</span>}
        {c.qc && <span className="cyc-lane-qc mono">{c.qc}</span>}
        {c.container && <span className="cyc-lane-cnt mono">{c.container}</span>}
        {c.container_to_container && <span className="cyc-lane-c2c" title={ko(lang) ? "연속 적재 (공차 구간 없음)" : "back-to-back (no empty leg)"}>↻</span>}
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
  // shared time scale across the shown cycles = the longest total cycle, so bars compare 1:1
  const scale = useMemo(() => Math.max(1, ...cycles.map((c) => cyclePhases(c)?.totalSec ?? (c.cycle_s ?? 0))), [cycles]);
  const trend = useMemo(() => [...cycles].reverse().map((c) => c.cycle_s ?? 0).filter((v) => v > 0), [cycles]);
  const stats = useMemo(() => {
    if (!cycles.length) return null;
    const ld = cycles.filter((c) => c.jobtype === "LD").length;
    const ds = cycles.filter((c) => c.jobtype === "DS").length;
    const other = cycles.length - ld - ds;
    const kms = cycles.reduce((a, c) => a + (c.laden_leg_m ?? 0), 0) / 1000;
    const med = [...cycles.map((c) => c.cycle_s ?? 0)].filter((v) => v > 0).sort((a, b) => a - b);
    const median = med.length ? med[Math.floor(med.length / 2)] : null;
    return { ld, ds, other, kms, median, span: cycles.length };
  }, [cycles]);

  return (
    <div className="cyc-detail">
      <div className="cyc-detail-head">
        <div className="cyc-detail-id">
          <span className="mono">{ytno}</span>
          {stats && <span className="cyc-detail-sub">{ko(lang) ? `${stats.span}회 · 중위 ${mmss(stats.median)} · 적재 ${stats.kms.toFixed(1)}km` : `${stats.span} cycles · median ${mmss(stats.median)} · ${stats.kms.toFixed(1)}km laden`}</span>}
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
        <span className="cyc-sec-h">{ko(lang) ? "사이클 단계별 타임라인 (최신순)" : "Cycle timeline by phase (latest first)"}</span>
        <span className="cyc-phase-legend">
          {PHASES.map((p) => (
            <span key={p.key}><span className="cyc-dot" style={{ background: p.c }} />{ko(lang) ? p.ko : p.en}</span>
          ))}
        </span>
      </div>
      <div className="cyc-sec-h" style={{ fontWeight: 400, opacity: 0.7, marginTop: -2 }}>
        {ko(lang) ? "5단계: 배차 → 픽업도착 → 픽업떠남 → 부하도착 → 드롭 · 빈칸 = 미관측(추정 안 함)" : "5-event: assign → pickup-arr → pickup-left → drop-arr → dropped · gaps = unobserved"}
      </div>
      <div className="cyc-lanes">
        {loading && <div className="cyc-empty">{ko(lang) ? "불러오는 중…" : "loading…"}</div>}
        {!loading && cycles.length === 0 && <div className="cyc-empty">{ko(lang) ? "이 범위에 사이클 없음" : "no cycles in range"}</div>}
        {cycles.map((c, i) => <CycleLane key={c.dropped_at + i} c={c} scale={scale} lang={lang} />)}
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
          <span className="cyc-title-sub">{ko(lang) ? "검증된 컨테이너 인도 단위 · 학습 데이터 누적" : "per validated container delivery · accumulated training data"}</span>
        </div>
        <div className="cyc-range">
          {RANGES.map((r) => (
            <button key={r.h} className={`cyc-range-btn${hours === r.h ? " active" : ""}`} onClick={() => setHours(r.h)}>{ko(lang) ? r.ko : r.en}</button>
          ))}
        </div>
      </div>

      <div className="cyc-tiles">
        <Tile label={ko(lang) ? "총 사이클" : "Total cycles"} value={sum ? String(sum.total_cycles) : "—"} accent="#60a5fa" />
        <Tile label={ko(lang) ? "가동 트럭" : "Active trucks"} value={sum ? String(sum.trucks) : "—"} accent="#0ea5e9" />
        <Tile label={ko(lang) ? "시간당 사이클" : "Cycles / hr"} value={sum ? sum.cycles_per_hr.toFixed(1) : "—"} accent="#34d399" />
        <Tile label={ko(lang) ? "플릿 중위 사이클" : "Fleet median"} value={sum ? mmss(sum.fleet_median_s) : "—"} accent="#f59e0b" />
        <Tile label={ko(lang) ? "총 적재 거리" : "Laden distance"} value={sum ? sum.fleet_laden_km.toFixed(0) : "—"} unit="km" accent="#a78bfa" />
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
            <span>{ko(lang) ? "트럭별 (사이클 많은 순)" : "By truck (most cycles)"}</span>
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
