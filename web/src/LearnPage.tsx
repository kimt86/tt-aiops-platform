// 학습 센터 — 2탭 구성:
//   🧠 예측 모델 — 각 모델 카드(입력·출력·담당역할 + KPI + KPI 트렌드 차트)
//   🗄️ 데이터 수집 — 수집 스트림 카드(설명·소스·쓰임·총/최근 건수 + 펼치면 최근 데이터)
import { useEffect, useMemo, useState } from "react";
import { type Lang } from "./i18n";
import { api, type LearnTopos, type LearnToposPoint, type LanesData, type TravelData, type TravelOd, type SoonIdleData, type SoonIdleLead, type DispatchPredData, type LearnExtra, type DataStat, type DataRow } from "./api";
import { LineChart } from "./charts";

const ko = (lang: Lang) => lang === "ko";
const fmtN = (n: number | null | undefined) => (n == null ? "—" : n.toLocaleString());
const mPrec = (m: number | null | undefined) => (m == null ? "—" : `${m.toFixed(1)}m`);
const stamp = (iso: string | null | undefined) =>
  iso ? new Date(iso).toLocaleString([], { timeZone: "Asia/Kuala_Lumpur", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false }) : "—";
const stampS = (iso: string | null | undefined) =>
  iso ? new Date(iso).toLocaleString([], { timeZone: "Asia/Kuala_Lumpur", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false }) : "—";
const mmss = (s: number | null | undefined) => (s == null ? "—" : `${Math.floor(s / 60)}:${String(Math.round(s % 60)).padStart(2, "0")}`);
const mDist = (m: number | null | undefined) => (m == null ? "—" : m >= 1000 ? `${(m / 1000).toFixed(2)}km` : `${Math.round(m)}m`);
const kmh = (v: number | null | undefined) => (v == null ? "—" : `${v.toFixed(1)}`);

const ISO_RE = /^\d{4}-\d\d-\d\dT\d\d:\d\d/;
// format one sample-table cell: timestamps→MYT, bools→✓/✗, integers→grouped, floats verbatim.
function fmtCell(v: string | number | boolean | null): string {
  if (v == null) return "—";
  if (typeof v === "boolean") return v ? "✓" : "·";
  if (typeof v === "number") return Number.isInteger(v) ? v.toLocaleString() : String(v);
  if (ISO_RE.test(v)) return stampS(v);
  return v;
}

// recent trend of a quality series. `higherBetter` decides what counts as "improving".
// Percent change is only meaningful for bounded metrics (recall, MAPE, spread). For accumulating
// counts (samples, points) the baseline starts near zero, so % explodes → flag `huge` and show
// direction only. Everything is clamped so no absurd figures ever render.
function trend(series: number[], higherBetter: boolean): { dir: 1 | -1 | 0; pct: number; huge: boolean; improving: boolean } | null {
  const v = series.filter((x) => x != null && !Number.isNaN(x));
  if (v.length < 2) return null;
  const first = v[0], last = v[v.length - 1];
  const delta = last - first;
  const base = Math.abs(first);
  // baseline is tiny vs the current value → grew from ~0 (accumulating): % is not meaningful
  const huge = base < Math.abs(last) / 5;
  const p = base > 1e-6 ? (delta / base) * 100 : 0;
  const dir = Math.abs(delta) < 1e-9 || (!huge && Math.abs(p) < 2) ? 0 : delta > 0 ? 1 : -1;
  return { dir, pct: Math.min(Math.abs(p), 999), huge, improving: higherBetter ? delta > 0 : delta < 0 };
}

function TrendBadge({ series, higherBetter, accumulating, lang }: { series: number[]; higherBetter: boolean; accumulating?: boolean; lang: Lang }) {
  // accumulating counts (samples, learned points) just grow — that's expected coverage, NOT the
  // model "improving". Show a neutral "accumulating", never a 개선/better judgment.
  if (accumulating) {
    const v = series.filter((x) => x != null && !Number.isNaN(x));
    if (v.length < 2) return <span className="ls-tb flat">{ko(lang) ? "수집 중" : "collecting"}</span>;
    return <span className="ls-tb flat">{v[v.length - 1] > v[0] ? `↑ ${ko(lang) ? "누적 중" : "accumulating"}` : (ko(lang) ? "안정" : "stable")}</span>;
  }
  const t = trend(series, higherBetter);
  if (!t) return <span className="ls-tb flat">{ko(lang) ? "수집 중" : "collecting"}</span>;
  if (t.dir === 0) return <span className="ls-tb flat">→ {ko(lang) ? "안정" : "stable"}</span>;
  const arrow = t.dir === 1 ? "↑" : "↓";
  const label = t.improving ? (ko(lang) ? "개선" : "better") : (ko(lang) ? "악화" : "worse");
  const body = t.huge ? label : `${t.pct.toFixed(0)}% ${label}`;
  return <span className={`ls-tb ${t.improving ? "up" : "bad"}`}>{arrow} {body}</span>;
}

// one panel (a column inside a card): a tag (📈 learning / 🧪 test) + content.
// a metric block: headline = last value of its trend series (always matches the chart) + badge + chart.
function Chip({ label, value, accent }: { label: string; value: string; accent?: string }) {
  return (
    <div className="ls-chip">
      <div className="ls-chip-l">{label}</div>
      <div className="ls-chip-v" style={accent ? { color: accent } : undefined}>{value}</div>
    </div>
  );
}

// input → output → role-in-our-logic strip. Shown at the top of every model card.
function IoStrip({ inputs, output, role, accent, lang }: { inputs: string; output: string; role: string; accent: string; lang: Lang }) {
  const k = ko(lang);
  return (
    <div className="ls-io">
      <div className="ls-io-row"><span className="ls-io-k">{k ? "입력" : "in"}</span><span className="ls-io-v">{inputs}</span></div>
      <div className="ls-io-row"><span className="ls-io-k">{k ? "출력" : "out"}</span><span className="ls-io-v">{output}</span></div>
      <div className="ls-io-row"><span className="ls-io-k role" style={{ color: accent, borderColor: accent + "66" }}>{k ? "담당" : "role"}</span><span className="ls-io-v role">{role}</span></div>
    </div>
  );
}

type Status = "live" | "shadow" | "selfcal";
function StatusBadge({ status, lang }: { status: Status; lang: Lang }) {
  const k = ko(lang);
  const m: Record<Status, [string, string, string]> = {
    live: [k ? "가동" : "LIVE", "#22c55e", k ? "우리 배차·맵·거리·비용·학습라벨에 실제 쓰이는 부품" : "in use — powers our dispatch/map/cost/labels"],
    shadow: [k ? "검증중" : "VALIDATING", "#f59e0b", k ? "우리 배차 계산에 쓰이나 정확도 검증 중 · 배차 전체는 TOS와 대조" : "used in our dispatch; accuracy validating (dispatch shadowed vs TOS)"],
    selfcal: [k ? "자가보정" : "SELF-CAL", "#38bdf8", k ? "예측오차를 스스로 되먹여 교정" : "self-correcting from its own error"],
  };
  const [label, c, tip] = m[status];
  return <span className="ls-badge" title={tip} style={{ color: c, borderColor: c + "66", background: c + "1e" }}>{label}</span>;
}

// collapsible model card: header (badge · title · headline metric) always visible; body on expand.
function PointRow({ p, lang }: { p: LearnToposPoint; lang: Lang }) {
  return (
    <div className={`learn-row${p.n >= 30 ? " conf" : ""}`}>
      <span className="mono">{p.topos}</span>
      <span style={{ color: p.is_crane ? "#f59e0b" : "#0ea5e9" }}>{p.is_crane ? (ko(lang) ? "크레인" : "crane") : (ko(lang) ? "블록" : "block")}</span>
      <span className="mono">{p.n}</span>
      <span className="mono">{p.obs.toLocaleString()}</span>
      <span className="mono">{mPrec(p.spread_m)}</span>
      <span className="mono" style={{ fontSize: 11 }}>{p.lat.toFixed(5)}, {p.lon.toFixed(5)}</span>
      <span className="mono" style={{ fontSize: 11, color: "var(--text-mute)" }}>{stamp(p.updated_at)}</span>
    </div>
  );
}

function OdRow({ o }: { o: TravelOd }) {
  return (
    <div className={`learn-od-row${o.n >= 10 ? " conf" : ""}`}>
      <span className="mono">{o.origin}</span><span className="mono">{o.dest}</span><span className="mono">{o.n}</span>
      <span className="mono">{mmss(o.median_s)}</span><span className="mono">{mDist(o.dist_m)}</span><span className="mono">{kmh(o.speed_kmh)}</span>
    </div>
  );
}

// "예측 후 실제 몇 분 뒤 유휴가 됐나" — per jobtype: median lead + p10~p90 range bar + recall/precision.
function LeadCard({ jt, accent, lead, recall, recallGps, precision, lang }: { jt: string; accent: string; lead: SoonIdleLead | undefined; recall: number | null; recallGps: number | null; precision: number | null; lang: Lang }) {
  const k = ko(lang);
  const MAXM = 20; // lead window is 20min, so scale the range bar 0..20
  const toMin = (s: number | null | undefined) => (s == null ? null : s / 60);
  const p10 = toMin(lead?.lead_p10_s), p50 = toMin(lead?.lead_p50_s), p90 = toMin(lead?.lead_p90_s);
  const clamp = (m: number) => Math.max(0, Math.min(100, (m / MAXM) * 100));
  return (
    <div className="ls-lead" style={{ borderTopColor: accent }}>
      <div className="ls-lead-jt" style={{ color: accent }}>{jt} {jt === "DS" ? (k ? "· 양하" : "· discharge") : (k ? "· 적하" : "· load")}</div>
      <div className="ls-lead-v">{p50 != null ? p50.toFixed(1) : "—"}<span className="ls-lead-u">{k ? "분 후 유휴 (예측·중앙)" : "min to idle (predicted)"}</span></div>
      <div className="ls-lead-track">
        {p10 != null && p90 != null && <div className="ls-lead-iqr" style={{ left: `${clamp(p10)}%`, width: `${Math.max(1, clamp(p90) - clamp(p10))}%`, background: accent + "55" }} />}
        {p50 != null && <div className="ls-lead-med" style={{ left: `${clamp(p50)}%`, background: accent }} />}
      </div>
      <div className="ls-lead-sub">{k ? "범위 p10~p90" : "p10~p90"} {p10 != null ? p10.toFixed(1) : "—"}~{p90 != null ? p90.toFixed(1) : "—"}{k ? "분" : "m"} · {k ? "적중" : "matched"} {lead?.matched ?? 0}{k ? "건" : ""}</div>
      <div className="ls-lead-acc">🎯 {k ? "분 예측 정확도" : "minutes accuracy"} — MAPE <b style={{ color: lead?.mape_pct != null && lead.mape_pct <= 35 ? "#34d399" : "#f59e0b" }}>{lead?.mape_pct != null ? `${lead.mape_pct.toFixed(0)}%` : "—"}</b> · {k ? "±30% 적중" : "within ±30%"} <b>{lead?.within_30pct != null ? `${lead.within_30pct.toFixed(0)}%` : "—"}</b></div>
      <div className="ls-lead-q">{k ? "재현율" : "recall"} <b style={{ color: "#34d399" }}>{recall != null ? `${recall.toFixed(0)}%` : "—"}</b>{recallGps != null && recall != null ? ` (GPS ${recallGps.toFixed(0)}→+${(recall - recallGps).toFixed(0)}%p)` : ""} · {k ? "정밀도" : "prec"} <b>{precision != null ? `${precision.toFixed(0)}%` : "—"}</b></div>
    </div>
  );
}

// ───────────────────────────── 예측 모델 탭 ─────────────────────────────
// ── redesign: config-driven scorecard board ──────────────────────────────────────────

// tiny inline-SVG trend spark (row scale); only rendered when ≥2 real snapshots exist.
function MicroSpark({ values, color }: { values: number[]; color: string }) {
  const v = values.filter((x) => x != null && !Number.isNaN(x));
  if (v.length < 2) return null;
  const w = 76, h = 22, pad = 3;
  const min = Math.min(...v), max = Math.max(...v), span = max - min || 1;
  const pts = v.map((y, i) => {
    const x = pad + (i / (v.length - 1)) * (w - 2 * pad);
    const yy = pad + (1 - (y - min) / span) * (h - 2 * pad);
    return `${x.toFixed(1)},${yy.toFixed(1)}`;
  });
  const last = pts[pts.length - 1].split(",");
  return (
    <svg className="ls-spark" width={w} height={h} viewBox={`0 0 ${w} ${h}`}>
      <polyline points={pts.join(" ")} fill="none" stroke={color} strokeWidth="1.5" strokeLinejoin="round" strokeLinecap="round" />
      <circle cx={last[0]} cy={last[1]} r="2" fill={color} />
    </svg>
  );
}

// current-value-vs-target track (for models with no time-series). neutral — never a fake trend line.
function TargetTrack({ value, target, lang }: { value: number; target: number; lang: Lang }) {
  const pct = Math.max(4, Math.min(100, target > 0 ? (value / target) * 100 : 0));
  return (
    <div className="ls-track" title={ko(lang) ? `현재 ${Math.round(value)} / 목표 ${Math.round(target)}` : `now ${Math.round(value)} / target ${Math.round(target)}`}>
      <div className="ls-track-bar"><div className="ls-track-fill" style={{ width: `${pct}%` }} /><div className="ls-track-dot" style={{ left: `${pct}%` }} /></div>
      <span className="ls-track-lbl">{ko(lang) ? "현재값 · 추세 준비중" : "current · trend pending"}</span>
    </div>
  );
}

// discrete 4-stage coverage/maturity (answers "learning well?"); separate from the trend cell.
function MaturityBar({ stage, lang }: { stage: number; lang: Lang }) {
  const labels = ko(lang) ? ["씨앗", "학습", "성숙", "포화"] : ["seed", "learning", "mature", "full"];
  const s = Math.max(1, Math.min(4, stage));
  return (
    <span className="ls-mat" title={ko(lang) ? "학습 성숙도(데이터 커버리지)" : "learning maturity (data coverage)"}>
      {[1, 2, 3, 4].map((i) => <i key={i} className={`ls-mat-pill${i <= s ? " on" : ""}`} />)}
      <span className="ls-mat-lbl">{labels[s - 1]}</span>
    </span>
  );
}

function CaveatChip({ text }: { text: string }) { return <span className="ls-caveat">⚠ {text}</span>; }
function SurfaceChips({ items, lang }: { items: string[]; lang: Lang }) {
  return <div className="ls-surfaces"><span className="ls-surfaces-k">{ko(lang) ? "쓰임 →" : "used in →"}</span>{items.map((s) => <span key={s} className="ls-surface">{s}</span>)}</div>;
}

// honest trend dispatcher: trend (spark) | current (target track) | collecting.
function TrendCell({ reading, series, higherBetter, value, target, lang }: { reading: "trend" | "current" | "collecting"; series: number[]; higherBetter: boolean; value: number; target: number; lang: Lang }) {
  const real = series.filter((x) => x != null && !Number.isNaN(x));
  const kind = real.length >= 2 ? "trend" : reading === "trend" ? "collecting" : reading;
  if (kind === "trend") {
    const t = trend(series, higherBetter);
    const color = t?.improving ? "#34d399" : t && t.dir !== 0 ? "#f43f5e" : "#94a3b8";
    return <div className="ls-trendcell"><MicroSpark values={series} color={color} /><TrendBadge series={series} higherBetter={higherBetter} lang={lang} /></div>;
  }
  if (kind === "current") return <TargetTrack value={value} target={target} lang={lang} />;
  return <span className="ls-tb flat">{ko(lang) ? "수집 중" : "collecting"}</span>;
}

type Headline = { value: string; unit: string; label: string; raw: number };
type ModelDef = {
  id: string; group: "live" | "validating"; status: Status; extra?: Status;
  typeTag: [string, string]; name: [string, string]; story: [string, string];
  headline: Headline; reading: "trend" | "current" | "collecting"; series: number[];
  higherBetter: boolean; target: number; maturity: number; caveat?: [string, string];
  surfaces: string[]; detail: React.ReactNode;
};

function ScoreRow({ m, lang }: { m: ModelDef; lang: Lang }) {
  const [open, setOpen] = useState(false);
  const i = ko(lang) ? 0 : 1;
  return (
    <div className={`sr${open ? " open" : ""}`}>
      <button className="sr-head" onClick={() => setOpen((o) => !o)}>
        <div className="sr-id">
          <div className="sr-name">{m.name[i]}<span className="sr-en">{m.name[1 - i]}</span></div>
          <div className="sr-tags"><span className="sr-type">▪{m.typeTag[i]}</span><MaturityBar stage={m.maturity} lang={lang} /></div>
        </div>
        <div className="sr-story">{m.story[i]}{m.caveat && <CaveatChip text={m.caveat[i]} />}</div>
        <div className="sr-headline"><span className="sr-num">{m.headline.value}</span><span className="sr-unit">{m.headline.unit}</span><span className="sr-hlabel">{m.headline.label}</span></div>
        <div className="sr-trend"><TrendCell reading={m.reading} series={m.series} higherBetter={m.higherBetter} value={m.headline.raw} target={m.target} lang={lang} /></div>
        <div className="sr-badges">{m.extra && <StatusBadge status={m.extra} lang={lang} />}</div>
        <span className="sr-chev">{open ? "▲" : "▼"}</span>
      </button>
      {open && <div className="sr-body"><SurfaceChips items={m.surfaces} lang={lang} />{m.detail}</div>}
    </div>
  );
}

function HealthStrip({ total, obs, updated, movers, lang }: { total: number; obs: string; updated: string; movers: string[]; lang: Lang }) {
  return (
    <div className="hs">
      <div className="hs-fleet"><b>{total} {ko(lang) ? "예측 모델" : "prediction models"}</b><span className="hs-legend">{ko(lang) ? "GPS·상태·날씨로 배차를 예측" : "predict dispatch from GPS, state, weather"}</span></div>
      {movers.length > 0 && <div className="hs-movers"><span className="hs-movers-k">{ko(lang) ? "이번 주 개선" : "improving"}</span>{movers.map((mv, i) => <span key={i} className="hs-mover">{mv}</span>)}</div>}
      <div className="hs-meta">{ko(lang) ? "관측" : "obs"} {obs} · {ko(lang) ? "갱신" : "upd"} {updated}</div>
    </div>
  );
}

function ModelsTab({ lang }: { lang: Lang }) {
  const k = ko(lang);
  const [d, setD] = useState<LearnTopos | null>(null);
  const [ln, setLn] = useState<LanesData | null>(null);
  const [tv, setTv] = useState<TravelData | null>(null);
  const [si, setSi] = useState<SoonIdleData | null>(null);
  const [dp, setDp] = useState<DispatchPredData | null>(null);
  const [ex, setEx] = useState<LearnExtra | null>(null);
  const [err, setErr] = useState(false);
  const [onlyBlock, setOnlyBlock] = useState(true);
  const [q, setQ] = useState("");

  useEffect(() => {
    let alive = true;
    const load = () => {
      api.learnTopos().then((r) => { if (alive) { setD(r); setErr(false); } }).catch(() => alive && setErr(true));
      api.learnLanes().then((r) => { if (alive) setLn(r); }).catch(() => {});
      api.learnTravel().then((r) => { if (alive) setTv(r); }).catch(() => {});
      api.learnSoonIdle().then((r) => { if (alive) setSi(r); }).catch(() => {});
      api.learnDispatchPred().then((r) => { if (alive) setDp(r); }).catch(() => {});
      api.learnExtra().then((r) => { if (alive) setEx(r); }).catch(() => {});
    };
    load();
    const id = setInterval(load, 30000);
    return () => { alive = false; clearInterval(id); };
  }, []);

  // ① travel — learning = accumulating samples; test = predicted-vs-actual (accuracy block)
  // ③ topos — learning = confident points (n≥30); test = positional residual (median spread ↓)
  const ms = d?.metric_series ?? [];
  const spreadVals = ms.map((p) => p.median_spread_m ?? 0).filter((v) => v > 0);
  // ④ lanes — learning = road cells; test = directional consistency (one-way fraction ↑)
  const lms = ln?.metric_series ?? [];
  const onewayVals = lms.map((p) => (p.oneway_frac ?? 0) * 100);
  // ⑤ soon-idle — per jobtype recall/precision/lead (DS + LD)
  const jobAgg = (jt: string) => {
    const rows = (si?.by_source ?? []).filter((s) => s.jobtype === jt);
    const pred = rows.reduce((a, s) => a + s.predictions, 0);
    const matched = rows.reduce((a, s) => a + s.matched, 0);
    const rec = si?.by_jobtype.find((j) => j.jobtype === jt);
    return { precision: pred ? (100 * matched) / pred : null, recall: rec?.recall_pct ?? null, recallGps: rec?.recall_gps_pct ?? null, lead: si?.lead_by_jobtype.find((l) => l.jobtype === jt) };
  };
  const dsJob = jobAgg("DS"), ldJob = jobAgg("LD");
  const siRecallSeries = (si?.metric_series ?? []).filter((p) => p.jobtype === "DS" && p.source === "ALL").map((p) => p.recall_pct ?? 0);
  const ldRecallSeries = (si?.metric_series ?? []).filter((p) => p.jobtype === "LD" && p.source === "ALL").map((p) => p.recall_pct ?? 0);
  const dse = si?.ds_eta; // ⑥ DS minutes-to-idle feature model

  const points = useMemo(() => {
    let pts = d?.points ?? [];
    if (onlyBlock) pts = pts.filter((p) => !p.is_crane);
    if (q) pts = pts.filter((p) => p.topos.toLowerCase().includes(q.toLowerCase()));
    return pts;
  }, [d, onlyBlock, q]);

  const acc = tv?.accuracy;
  // headline metric shown in each collapsed card header

  // ── config-driven scorecard board ──
  const stg = (n: number, a: number, b: number, c: number) => (n >= c ? 4 : n >= b ? 3 : n >= a ? 2 : 1);
  const MODELS: ModelDef[] = [
    {
      id: "travel", group: "live", status: "live", typeTag: ["예측", "predict"],
      name: ["TT 이동시간 예측", "Travel time"],
      story: [
        `빈 트럭 A→B 시간을 배워 · 10번 중 ${acc?.within_30pct != null ? Math.round(acc.within_30pct / 10) : "?"}번 ±30% 안 · 배차 비용표에 쓰임`,
        `learns empty-drive A→B time · ${acc?.within_30pct != null ? Math.round(acc.within_30pct) : "?"}% within ±30% · powers the dispatch cost table`,
      ],
      headline: { value: acc?.within_30pct != null ? `${Math.round(acc.within_30pct)}` : "—", unit: "%", label: k ? "±30% 적중" : "within ±30%", raw: acc?.within_30pct ?? 0 },
      reading: "current", series: [], higherBetter: true, target: 85,
      maturity: tv ? stg(tv.confident_pairs, 200, 800, 1600) : 1,
      caveat: ["같은 OD 변동 ±76% — 점이 아닌 분포로", "within-OD variance ±76% — use as a distribution"],
      surfaces: [k ? "비용행렬" : "cost", k ? "배차" : "dispatch"],
      detail: (
        <>
          <IoStrip accent="#60a5fa" lang={lang} inputs={k ? "출발존→도착존 + 경로거리·밀도·시간대·날씨" : "O→D zone + route dist, density, hour, weather"} output={k ? "구간 공차 이동시간(초) 분포" : "empty-travel seconds per O→D"} role={k ? "2단계 비용 = 순수주행. 도로망(④) 경로시간을 실측에 맞춰 보정" : "Stage-2 cost = pure drive; road route time calibrated to actual"} />
          <div className="ls-beat">{k ? "지금 성능 — 예측 vs 실제 (2일·신뢰 OD)" : "performance — predicted vs actual"}</div>
          {acc && acc.evaluated > 0 ? (
            <div className="ls-testchips">
              <Chip label="MAPE" value={acc.mape_pct != null ? `${acc.mape_pct.toFixed(0)}%` : "—"} accent="#f59e0b" />
              <Chip label={k ? "±30% 적중" : "within ±30%"} value={acc.within_30pct != null ? `${acc.within_30pct.toFixed(0)}%` : "—"} accent="#34d399" />
              <Chip label={k ? "중앙 절대오차" : "median abs err"} value={mmss(acc.median_abs_err_s)} />
              <Chip label={k ? "신뢰 OD쌍" : "confident O→D"} value={tv ? fmtN(tv.confident_pairs) : "—"} />
            </div>
          ) : <div className="cyc-empty">{k ? "trip 완료 대기 중" : "awaiting trips"}</div>}
          <details className="ls-detail"><summary>{k ? "상세 — 구간별 이동시간" : "detail — by O→D"}</summary>
            <div className="learn-od-cols" style={{ marginTop: 8 }}><span>{k ? "출발" : "orig"}</span><span>{k ? "도착" : "dest"}</span><span>n</span><span>{k ? "중앙" : "med"}</span><span>{k ? "거리" : "dist"}</span><span>km/h</span></div>
            <div className="learn-list">{(tv?.od ?? []).slice(0, 200).map((o) => <OdRow key={o.origin + o.dest} o={o} />)}</div>
          </details>
        </>
      ),
    },
    {
      id: "workpoints", group: "live", status: "live", typeTag: ["예측", "predict"],
      name: ["작업지점 좌표", "Work-points"],
      story: [
        `도착 GPS로 블록·안벽 좌표를 배워 · ±${d?.median_spread_m != null ? Math.round(d.median_spread_m) : "?"}m로 좁혀지는 중 · 거리·비용 앵커로 쓰임`,
        `learns block/wharf coords from arrival GPS · tightening to ±${d?.median_spread_m != null ? Math.round(d.median_spread_m) : "?"}m · distance/cost anchor`,
      ],
      headline: { value: d?.median_spread_m != null ? `±${Math.round(d.median_spread_m)}` : "—", unit: "m", label: k ? "정밀도" : "precision", raw: d?.median_spread_m ?? 0 },
      reading: "trend", series: spreadVals, higherBetter: false, target: 60,
      maturity: d ? stg(d.confident_topos, 500, 2000, 5000) : 1,
      surfaces: [k ? "배차" : "dispatch", k ? "맵" : "map", k ? "비용" : "cost", k ? "도로망" : "road"],
      detail: (
        <>
          <IoStrip accent="#0ea5e9" lang={lang} inputs={k ? "TT가 블록·안벽에 도착(ARRIVED)한 GPS" : "TT GPS at ARRIVED"} output={k ? "블록/안벽 중심좌표 + 정밀도" : "centroid coords + precision"} role={k ? "위치 앵커 → ④ 도로망 커넥터. 300m 이상치 게이트 + 250m 필터로 오라벨 제거" : "location anchor → ④ road connector; 300m gate + 250m filter"} />
          <div className="ls-beat">{k ? "지금 성능 — 위치 정밀도 추이 (낮을수록↑)" : "performance — precision trend (lower better)"}</div>
          <div className="ls-pchart">{spreadVals.length > 1 ? <LineChart values={spreadVals} color="#f59e0b" axes /> : <div className="cyc-empty">{k ? "스냅샷 수집 중" : "collecting"}</div>}</div>
          <div className="ls-chips"><Chip label={k ? "학습 지점" : "points"} value={d ? fmtN(d.distinct_topos) : "—"} /><Chip label={k ? "블록 지점" : "blocks"} value={d ? fmtN(d.block_points) : "—"} /><Chip label={k ? "누적 관측" : "obs"} value={d ? fmtN(d.total_obs) : "—"} /></div>
          <details className="ls-detail"><summary>{k ? "상세 — 학습된 지점" : "detail — learned points"}</summary>
            <div className="cyc-board-head" style={{ marginTop: 8, border: 0 }}><label style={{ fontSize: 11, color: "var(--text-dim)", cursor: "pointer" }}><input type="checkbox" checked={onlyBlock} onChange={(e) => setOnlyBlock(e.target.checked)} /> {k ? "블록만" : "blocks only"}</label><input className="cyc-search mono" placeholder={k ? "topos 검색" : "find"} value={q} onChange={(e) => setQ(e.target.value)} /></div>
            <div className="learn-cols"><span>topos</span><span>{k ? "종류" : "kind"}</span><span>n</span><span>{k ? "누적" : "obs"}</span><span>{k ? "정밀도" : "prec"}</span><span>{k ? "좌표" : "coord"}</span><span>{k ? "갱신" : "upd"}</span></div>
            <div className="learn-list">{points.slice(0, 300).map((p) => <PointRow key={p.topos} p={p} lang={lang} />)}</div>
          </details>
        </>
      ),
    },
    {
      id: "qc", group: "live", status: "live", typeTag: ["예측", "predict"],
      name: ["QC 안벽 작업지점", "QC handover point"],
      story: [
        `트럭 도착이 이루는 안벽선에 크레인을 투영해 흔들림 제거 · ${ex ? `${ex.qc_total}기 중 ${ex.qc_projected}기` : "?"} ±15m · 배차 목적지로 쓰임`,
        `projects cranes onto the truck-arrival quay line (swing removed) · ${ex ? `${ex.qc_projected}/${ex.qc_total}` : "?"} at ±15m · dispatch destination`,
      ],
      headline: { value: ex ? `${ex.qc_projected}/${ex.qc_total}` : "—", unit: "", label: k ? "±15m 해결" : "resolved ±15m", raw: ex?.qc_projected ?? 0 },
      reading: "current", series: [], higherBetter: true, target: ex?.qc_total ?? 61,
      maturity: ex && ex.qc_total ? stg(ex.qc_projected / ex.qc_total * 100, 50, 75, 90) : 1,
      caveat: ["스프레더 GPS 26m 진동 제거 → ~15m", "spreader GPS swing ~26m removed → ~15m"],
      surfaces: [k ? "배차 목적지" : "dispatch dest", k ? "맵" : "map"],
      detail: (
        <>
          <IoStrip accent="#f59e0b" lang={lang} inputs={k ? "트럭 도착 위치 → 안벽 직선(PCA) + 크레인별 최근중심(15분)" : "arrivals → quay line (PCA) + per-crane recency (15min)"} output={k ? "흔들림 제거 작업점 ~15m" : "swing-free work-point ~15m"} role={k ? "라이브 배차 목적지 + 맵 → ④ 커넥터" : "live dispatch dest + map → ④ connector"} />
          <div className="ls-note">{k ? "크레인 GPS는 스프레더에 달려 매 사이클 배↔트럭 ~26m 진동 → 그냥 평균내면 ~185m 뭉개짐. 트럭 도착 위치들이 이루는 안벽 직선에 크레인 최근중심을 투영해 진동만 제거." : "Crane GPS rides the spreader (~26m swing); plain average smears ~185m. We project the crane's recency centroid onto the quay line to drop the swing."}</div>
        </>
      ),
    },
    {
      id: "road", group: "live", status: "live", typeTag: ["공급", "feeds"],
      name: ["추론 도로망", "Road network"],
      story: [
        `이동 GPS로 도로·방향을 배워 · 방향일치 ${onewayVals.length ? Math.round(onewayVals[onewayVals.length - 1]) : "?"}%로 또렷해지는 중 · 경로거리·비용 공급`,
        `learns roads & direction from moving GPS · direction consistency ${onewayVals.length ? Math.round(onewayVals[onewayVals.length - 1]) : "?"}% · feeds route cost`,
      ],
      headline: { value: onewayVals.length ? `${Math.round(onewayVals[onewayVals.length - 1])}` : "—", unit: "%", label: k ? "방향일치" : "1-way", raw: onewayVals.length ? onewayVals[onewayVals.length - 1] : 0 },
      reading: "trend", series: onewayVals, higherBetter: true, target: 100,
      maturity: ln ? stg(ln.cells, 2000, 5000, 7000) : 1,
      caveat: ["원경로시간 상관 0.36 — ①의 보정곡선을 거쳐 사용", "raw route-time corr 0.36 — passed through ①'s cost curve"],
      surfaces: [k ? "경로비용" : "route cost", k ? "맵" : "map"],
      detail: (
        <>
          <IoStrip accent="#34d399" lang={lang} inputs={k ? "이동 TT의 GPS 트레이스" : "moving-TT GPS traces"} output={k ? "도로 노드·엣지·방향·속도" : "road nodes/edges/direction/speed"} role={k ? "방향 Dijkstra 라우터 → 배차 경로거리·경로시간; 맵" : "directed router → dispatch route dist/time; map"} />
          <div className="ls-beat">{k ? "지금 성능 — 방향 일관성 추이" : "performance — directional consistency"}</div>
          <div className="ls-pchart">{onewayVals.length > 1 ? <LineChart values={onewayVals} color="#a78bfa" axes /> : <div className="cyc-empty">{k ? "수집 중" : "collecting"}</div>}</div>
          <div className="ls-chips"><Chip label={k ? "도로 노드·엣지" : "nodes·edges"} value="~7.4k·7.6k" /><Chip label={k ? "도로망 길이" : "network"} value="~170km" /><Chip label={k ? "라우터 스냅" : "router snap"} value="96.6%" accent="#22d3ee" /></div>
        </>
      ),
    },
    {
      id: "dispatch1", group: "validating", status: "shadow", extra: "selfcal", typeTag: ["예측", "predict"],
      name: ["배차 작업시점 예측", "Dispatch work-time"],
      story: [
        `출항 역산+크레인 속도로 작업시각을 예측 · ±10분 안 ${dp?.ds_within10_pct != null ? Math.round(dp.ds_within10_pct) : "?"}% · 오차를 크레인별로 스스로 보정`,
        `predicts work time (departure backsolve + crane pace) · ${dp?.ds_within10_pct != null ? Math.round(dp.ds_within10_pct) : "?"}% within ±10min · self-corrects per crane`,
      ],
      headline: { value: dp?.ds_within10_pct != null ? `${Math.round(dp.ds_within10_pct)}` : "—", unit: "%", label: k ? "±10분 적중" : "within ±10m", raw: dp?.ds_within10_pct ?? 0 },
      reading: "current", series: [], higherBetter: true, target: 85,
      maturity: dp ? stg(dp.resolved_total, 5000, 50000, 200000) : 1,
      surfaces: [k ? "배차 마감" : "deadline", k ? "긴급도 순서" : "urgency"],
      detail: (
        <>
          <IoStrip accent="#f472b6" lang={lang} inputs={k ? "출항 역산 + 학습 크레인 속도 + 베이 큐" : "departure backsolve + learned crane pace + bay queue"} output={k ? "컨테이너별 작업시각 → 배차 마감" : "per-container work time → deadline"} role={k ? "배차 마감·긴급도 순서 결정 (Stage-2가 사용)" : "sets dispatch deadline & urgency (used by Stage-2)"} />
          <div className="ls-beat">{k ? "지금 성능 — 예측 vs 실제 작업시각" : "performance — predicted vs actual"}</div>
          {dp && dp.ds_eval > 0 ? (
            <div className="ls-testchips"><Chip label={k ? "양하 ±10분" : "DS ±10m"} value={dp.ds_within10_pct != null ? `${dp.ds_within10_pct.toFixed(0)}%` : "—"} accent="#34d399" /><Chip label={k ? "양하 오차" : "DS err"} value={dp.ds_med_err_min != null ? `${dp.ds_med_err_min >= 0 ? "+" : ""}${dp.ds_med_err_min.toFixed(1)}m` : "—"} accent="#f59e0b" /><Chip label={k ? "적하 오차" : "LD err"} value={dp.ld_med_err_min != null ? `${dp.ld_med_err_min >= 0 ? "+" : ""}${dp.ld_med_err_min.toFixed(1)}m` : "—"} /><Chip label={k ? "검증 누적" : "validated"} value={dp ? fmtN(dp.resolved_total) : "—"} /></div>
          ) : <div className="cyc-empty">{k ? "작업 완료 대기" : "awaiting"}</div>}
          <div className="ls-note" style={{ borderLeft: "2px solid #38bdf8", paddingLeft: 8 }}>🔵 {k ? "자가보정: 크레인별 예측오차(7일 중앙)를 다음 예측에 되먹여 스스로 교정 (~20분 갱신). 현재 양하≈0·적하 +약9분 낙관." : "Self-correction: per-crane 7-day median error fed back into the next prediction. DS≈0, LD ~+9min."}</div>
        </>
      ),
    },
    {
      id: "timetofree", group: "validating", status: "shadow", extra: "selfcal", typeTag: ["예측", "predict"],
      name: ["곧 빔 · 유휴 시각", "Time-to-free"],
      story: [
        `각 트럭이 언제 빌지 예측 · LD 10번 중 ${ldJob.recall != null ? Math.round(ldJob.recall / 10) : "?"}번 놓치지 않고 잡고 · 남은시간은 사이클 단계별 실측으로 자가보정 · 배차 후보풀·가용시각에 쓰임`,
        `predicts when each truck frees · catches LD ${ldJob.recall != null ? Math.round(ldJob.recall) : "?"}% · time-to-free self-corrected per cycle stage · feeds pool + availability`,
      ],
      headline: { value: ldJob.recall != null ? `${Math.round(ldJob.recall)}` : "—", unit: "%", label: k ? "LD 놓침없이 잡음" : "LD recall", raw: ldJob.recall ?? 0 },
      reading: "trend", series: ldRecallSeries, higherBetter: true, target: 100,
      maturity: si ? stg(si.predictions, 5000, 50000, 150000) : 1,
      surfaces: [k ? "배차 후보풀" : "candidate pool", k ? "후보 가용시각" : "availability"],
      detail: (
        <>
          <IoStrip accent="#a78bfa" lang={lang} inputs={k ? "트럭 사이클 단계·RTG 거리·발화 신호(GPS/PLC/TOS)" : "cycle stage, RTG dist, firing signal"} output={k ? "'곧 빔' 여부 + 유휴까지 남은시간" : "soon-idle flag + seconds-to-free"} role={k ? "곧 빌 트럭을 배차 후보풀에 넣고, 언제 가용해지는지로 비용 산정 (Stage-2가 사용)" : "adds soon-free trucks to the pool + times their availability (used by Stage-2)"} />
          <div className="ls-beat">{k ? "① 탐지 — 놓치지 않고 잡았나 (재현율 추이 DS·LD)" : "① detection — did we catch them (recall)"}</div>
          <div className="learn-charts"><div className="cyc-tp"><div className="cyc-sec-h">DS <TrendBadge series={siRecallSeries} higherBetter lang={lang} /></div><div className="cyc-tp-box">{siRecallSeries.length > 1 ? <LineChart values={siRecallSeries} color="#fb923c" axes /> : <div className="cyc-empty">{k ? "수집 중" : "collecting"}</div>}</div></div><div className="cyc-tp"><div className="cyc-sec-h">LD <TrendBadge series={ldRecallSeries} higherBetter lang={lang} /></div><div className="cyc-tp-box">{ldRecallSeries.length > 1 ? <LineChart values={ldRecallSeries} color="#22d3ee" axes /> : <div className="cyc-empty">{k ? "수집 중" : "collecting"}</div>}</div></div></div>
          <div className="ls-leads"><LeadCard jt="DS" accent="#fb923c" lead={dsJob.lead} recall={dsJob.recall} recallGps={dsJob.recallGps} precision={dsJob.precision} lang={lang} /><LeadCard jt="LD" accent="#22d3ee" lead={ldJob.lead} recall={ldJob.recall} recallGps={ldJob.recallGps} precision={ldJob.precision} lang={lang} /></div>
          <div className="ls-beat">{k ? "② 시각 — 사이클 단계별 유휴까지 남은시간 (상수 → 실측 자가보정)" : "② timing — seconds-to-free per stage (const → learned)"}</div>
          <div style={{ marginTop: 4 }}>{(ex?.fi_stages ?? []).map((s) => {
            const cst = ({ delivering: 1030, approaching: 480, wait_rtg: 480, soon_idle: 120 } as Record<string, number>)[s.state] ?? 0;
            const lbl = ({ delivering: ["운반 중", "delivering"], approaching: ["접근 중", "approaching"], wait_rtg: ["RTG 대기", "wait RTG"], soon_idle: ["곧 빔", "soon idle"] } as Record<string, [string, string]>)[s.state] ?? [s.state, s.state];
            return (
              <div key={s.state + s.jobtype} style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12, padding: "3px 0", borderBottom: "1px solid var(--border)" }}>
                <span style={{ minWidth: 118 }}>{k ? lbl[0] : lbl[1]}{s.jobtype === "LD" ? " · LD" : " · DS"}</span>
                <span style={{ color: "var(--text-mute)", textDecoration: "line-through" }}>{cst}s</span>
                <span style={{ color: "var(--text-dim)" }}>→</span>
                <span style={{ color: "#38bdf8", fontWeight: 700, fontVariantNumeric: "tabular-nums" }}>{s.med_rem_s}s</span>
                <span style={{ color: s.med_rem_s - cst >= 0 ? "#f59e0b" : "#34d399", fontSize: 11 }}>{s.med_rem_s - cst >= 0 ? "+" : ""}{s.med_rem_s - cst}s</span>
                <span style={{ marginLeft: "auto", color: "var(--text-dim)", fontSize: 11 }}>n={fmtN(s.n)}</span>
              </div>
            );
          })}</div>
          {dse ? <div className="ls-testchips" style={{ marginTop: 8 }}><Chip label={k ? "시각 정확도(DS MAPE)" : "timing DS MAPE"} value={dse.feat_mape_pct != null ? `${dse.feat_mape_pct.toFixed(0)}%` : "—"} accent="#fb923c" /><Chip label={k ? "±30% 적중" : "within ±30%"} value={dse.within_30pct != null ? `${dse.within_30pct.toFixed(0)}%` : "—"} accent="#34d399" /><Chip label={k ? "LD MAPE" : "LD MAPE"} value={ldJob.lead?.mape_pct != null ? `${ldJob.lead.mape_pct.toFixed(0)}%` : "—"} /></div> : null}
          <div className="ls-note" style={{ borderLeft: "2px solid #38bdf8", paddingLeft: 8, marginTop: 10 }}>🔵 {k
            ? `자가보정 1 (남은시간): 매 사이클 단계에서 실제 유휴까지 걸린 시간(free_in_sample 라벨)의 median으로 상수를 대체 — 전 단계·jobtype·RTG거리별, 7일창·15분 갱신. 상수는 양방향으로 어긋나 있었음(soon_idle 120→~300 과소 / delivering 1030→~570 과대).`
            : `Self-correction 1 (time): replaces each stage constant with the measured median time-to-free (per stage × jobtype × RTG bin, 7-day / 15-min). Constants were off both ways (soon_idle 120→~300, delivering 1030→~570).`}</div>
          <div className="ls-note" style={{ borderLeft: "2px solid #38bdf8", paddingLeft: 8, marginTop: 6 }}>🔵 {k
            ? `자가보정 2 (판정 문턱, 양방향): DS 곧빔 판정의 RTG 거리 문턱을 정밀도 0.82 유지하도록 자동 산출 — 현재 ${ex?.si_gate_m != null ? ex.si_gate_m.toFixed(0) : "50"}m (기본 50m, 정밀도 ${ex?.si_gate_prec ?? "?"}%). 게이트 밖 근접-미스 ${ex ? fmtN(ex.si_gate_nearmiss_n) : "?"}건도 관측해 좁히기·넓히기 모두 가능${ex && ex.si_gate_nearmiss_n < 200 ? " · 넓히기용 데이터 수집 중" : ""}.`
            : `Self-correction 2 (gate, both ways): DS soon-idle RTG-distance gate learned to hold precision ≥0.82 — now ${ex?.si_gate_m != null ? ex.si_gate_m.toFixed(0) : "50"}m (default 50m, precision ${ex?.si_gate_prec ?? "?"}%). Near-misses (${ex ? fmtN(ex.si_gate_nearmiss_n) : "?"}) let it tighten AND loosen${ex && ex.si_gate_nearmiss_n < 200 ? " · collecting" : ""}.`}</div>
        </>
      ),
    },
  ];

  const movers: string[] = [];
  const addMover = (label: string, series: number[], hb: boolean) => {
    const t = trend(series, hb);
    if (t && t.dir !== 0) movers.push(`${label} ${t.dir === 1 ? "↑" : "↓"}${t.improving ? (k ? "개선" : "up") : (k ? "악화" : "dn")}`);
  };
  addMover(k ? "작업지점" : "work-pt", spreadVals, false);
  addMover(k ? "도로방향" : "road", onewayVals, true);
  addMover(k ? "곧빔LD" : "soonLD", ldRecallSeries, true);
  const obsStr = d ? (d.total_obs >= 1e6 ? `${Math.round(d.total_obs / 1e6)}M+` : fmtN(d.total_obs)) : "—";
  const updStr = d?.metric_series?.length ? stamp(d.metric_series[d.metric_series.length - 1].captured_at) : "—";

  return (
    <>
      {err && <div className="cyc-err" style={{ marginBottom: 8 }}>{k ? "· 연결 오류" : "· offline"}</div>}
      <HealthStrip total={MODELS.length} obs={obsStr} updated={updStr} movers={movers.slice(0, 3)} lang={lang} />
      <div className="sb">{MODELS.map((m) => <ScoreRow key={m.id} m={m} lang={lang} />)}</div>
    </>
  );
}

// ───────────────────────────── 데이터 수집 탭 ─────────────────────────────
type StreamMeta = { key: string; name: [string, string]; source: [string, string]; usage: [string, string]; desc: [string, string] };
type Category = { title: [string, string]; accent: string; streams: StreamMeta[] };

const DATA_CATALOG: Category[] = [
  {
    title: ["원시 GPS · 위치", "Raw GPS · position"], accent: "#60a5fa",
    streams: [
      { key: "truck_pos_hifreq",
        name: ["3초 고빈도 GPS", "3s hi-freq GPS"],
        source: ["WP-TT GPS 웹소켓 (이동 트럭만)", "GPS websocket (moving trucks)"],
        usage: ["도로망 추론 · 순수 주행시간", "road-graph inference · pure drive-time"],
        desc: ["이동 중인 트럭의 위치를 3초마다 기록. 도로 중심선(skeleton) 추론과 정지 제외 순수 주행 leg의 원재료. 5일 보존.", "Moving-truck position every 3s. Feeds road-centerline inference + stop-excluded pure-drive legs. 5-day retention."] },
      { key: "truck_pos_hist",
        name: ["30초 위치 이력", "30s position history"],
        source: ["GPS 웹소켓 (전체 트럭)", "GPS websocket (all trucks)"],
        usage: ["이력 · 리플레이 · 배차 비교", "history · replay · dispatch compare"],
        desc: ["모든 트럭 위치를 30초마다 기록. 라이브맵 리플레이와 배차 비교의 위치 기준.", "All trucks every 30s. Position basis for live-map replay and dispatch comparison."] },
    ],
  },
  {
    title: ["사이클 · 이동시간", "Cycles · travel time"], accent: "#34d399",
    streams: [
      { key: "tt_cycle_v2",
        name: ["TT 작업 사이클 (5이벤트)", "TT work cycle (5-event)"],
        source: ["GPS 사이클 + 크레인 실측 교정", "GPS cycle + crane-truth"],
        usage: ["모든 학습의 백본 · 이동시간/곧빔 정답", "backbone for all learning"],
        desc: ["트럭 한 바퀴를 5시점(배차→픽업도착→픽업떠남→부하도착→드롭)·4구간으로 기록. 픽업떠남은 크레인 상차 실측으로 교정. 이동시간 학습·'곧 빔' 정답의 출처.", "One cycle as 5 events (dispatch→pickup-arr→pickup-left→laden-arr→drop). Pickup-left refined with crane ground truth. Source of travel labels + soon-idle truth."] },
      { key: "learn_travel_sample",
        name: ["이동시간 표본 (OD)", "travel-time samples (OD)"],
        source: ["사이클에서 수확 (5분)", "harvested from cycles (5min)"],
        usage: ["TT 이동시간 모델·비용행렬", "TT travel-time model"],
        desc: ["출발존→도착존 한 leg의 실제(정체 포함) 소요시간·거리·피처. 이동시간 예측·비용행렬의 학습 표본.", "Per O→D leg: actual seconds, distance, features. Training rows for travel prediction + cost matrix."] },
    ],
  },
  {
    title: ["예측 · 정확도 검증", "Predictions · accuracy validation"], accent: "#a78bfa",
    streams: [
      { key: "tt_soon_idle_pred",
        name: ["곧 빔 예측 로그", "soon-idle predictions"],
        source: ["라이브 예측기 (30초)", "live predictor (30s)"],
        usage: ["Soon-idle 정확도 검증", "soon-idle accuracy"],
        desc: ["'이 트럭이 곧 빈다'고 부른 매 예측을 기록 → 실제 빔과 대조(재현율·정밀도·리드타임).", "Every 'this truck frees soon' call, matched to the real free moment (recall/precision/lead)."] },
      { key: "dispatch_pred_sample",
        name: ["1단계 작업시점 예측", "stage-1 work-time prediction"],
        source: ["배차 1단계 예측기 (2분)", "stage-1 predictor (2min)"],
        usage: ["배차 마감 예측 검증", "dispatch-deadline validation"],
        desc: ["크레인이 각 컨테이너를 작업할 시각 예측 + 실제 작업시각 backfill → 정확도 검증.", "Predicted per-container work time + backfilled actual → accuracy validation."] },
      { key: "free_in_sample",
        name: ["잔여시간 학습셋", "free-in training set"],
        source: ["바쁜 트럭 스냅샷 (60초)", "busy-truck snapshot (60s)"],
        usage: ["free_in 모델 (예정)", "free_in model (planned)"],
        desc: ["바쁜 트럭의 피처 + 현재 우리 예측을 스냅샷, 10분 뒤 실제 빔까지 잔여초를 backfill = 라벨+검증.", "Busy-truck features + our current prediction; actual remaining-seconds backfilled = label + verification."] },
      { key: "stage2_match_shadow",
        name: ["2단계 매칭 그림자", "stage-2 match shadow"],
        source: ["라이브 2단계 매처 (60초)", "live stage-2 matcher (60s)"],
        usage: ["배차 권고 기록 · 비교", "dispatch recommendation log"],
        desc: ["우리 2단계 매처가 미배차 작업에 실제로 낸 트럭 권고(도착초·마감여유·비용티어).", "The truck our Stage-2 matcher actually recommends per unassigned work (arrival, slack, cost-tier)."] },
      { key: "dispatch_compare_shadow",
        name: ["TOS vs 우리 배차", "TOS vs ours"],
        source: ["배차 비교기 (60초)", "dispatch comparator (60s)"],
        usage: ["배차 일치 / 우열 비교", "agree / divergence compare"],
        desc: ["같은 작업에 대해 TOS가 고른 트럭과 우리가 고른 트럭의 도착시간 차이.", "For the same work, the arrival-time gap between TOS's truck and ours."] },
      { key: "fair_compare_detail",
        name: ["공정 1:1 비교 상세", "fair 1:1 detail"],
        source: ["공정 비교기 (5분)", "fair comparator (5min)"],
        usage: ["가치 입증 (절감 분해)", "value breakdown"],
        desc: ["TOS 실현 풀을 사후 최적 재매칭한 페어별 공차초(TOS vs 우리). 작업유형·거리·크레인별 절감 분해의 원천.", "Per-pair empty-seconds from re-optimizing TOS's realized pool (TOS vs ours). Source of the savings breakdown."] },
    ],
  },
  {
    title: ["환경 · 외부", "Environment · external"], accent: "#22d3ee",
    streams: [
      { key: "congestion_edge",
        name: ["도로 엣지 혼잡", "road-edge congestion"],
        source: ["매시간 cron map-match", "hourly cron map-match"],
        usage: ["혼잡 신호 · 시뮬", "congestion signal · sim"],
        desc: ["추론 도로 엣지별 중위 통과속도(이번 시간). 라이브맵 혼잡색 + 이동시간 피처.", "Per inferred road-edge median pass speed (this hour). Live-map congestion color + travel feature."] },
      { key: "weather_hourly",
        name: ["시간별 날씨", "hourly weather"],
        source: ["외부 기상 API", "external weather API"],
        usage: ["이동시간 피처 · 진단", "travel feature · diagnostic"],
        desc: ["강수·바람·시정. 비 올 때 이동시간 변화(±5%) 신호 + 진단 용도.", "Precip/wind/visibility. Rain→travel-time signal (±5%) + diagnostics."] },
      { key: "qc_wait_sample",
        name: ["QC 굶주림 스냅샷", "QC starvation snapshot"],
        source: ["30초 샘플러", "30s sampler"],
        usage: ["QC 대기 KPI (GPS)", "QC-wait KPI (GPS)"],
        desc: ["TT 부족으로 굶는 크레인을 topos·GPS 두 방식으로 측정 → K_QC_TT_WAIT_GPS.", "Cranes starved of TTs, measured by topos and GPS → K_QC_TT_WAIT_GPS."] },
    ],
  },
  {
    title: ["TOS 정답 · KPI 입력", "TOS ground truth · KPI input"], accent: "#f472b6",
    streams: [
      { key: "tos_handover_label",
        name: ["TOS 권위 완료 라벨", "TOS authoritative completions"],
        source: ["TOS Oracle (추출기)", "TOS Oracle (extractor)"],
        usage: ["곧빔 · 작업시점 정답", "ground truth"],
        desc: ["컨테이너 하역/완료 시각의 권위 기록. 곧빔·작업시점 예측의 정답(검증) 기준.", "Authoritative discharge/complete timestamps. The validation truth for soon-idle and work-time."] },
      { key: "learn_qc_move_time",
        name: ["크레인 작업속도 학습", "crane pace (learned)"],
        source: ["사이클에서 학습 (크레인·작업·주야)", "learned per crane/job/shift"],
        usage: ["1단계 작업시점 예측 입력", "stage-1 prediction input"],
        desc: ["크레인별 한 무브 중위 소요초(주야 구분). 배차 마감 역산의 속도 입력.", "Per-crane median seconds per move (day/night). The pace input for deadline backsolve."] },
    ],
  },
];

function SampleTable({ rows, loading, lang }: { rows: DataRow[] | null; loading: boolean; lang: Lang }) {
  const k = ko(lang);
  if (loading) return <div className="cyc-empty">{k ? "불러오는 중…" : "loading…"}</div>;
  if (!rows || rows.length === 0) return <div className="cyc-empty">{k ? "데이터 없음" : "no rows"}</div>;
  const cols = Object.keys(rows[0]);
  return (
    <div className="ds-table-wrap">
      <table className="ds-table mono">
        <thead><tr>{cols.map((c) => <th key={c}>{c}</th>)}</tr></thead>
        <tbody>
          {rows.map((r, i) => (
            <tr key={i}>{cols.map((c) => <td key={c}>{fmtCell(r[c])}</td>)}</tr>
          ))}
        </tbody>
      </table>
      <div className="ds-table-note">{k ? `최근 ${rows.length}행 (수집 시각 내림차순)` : `latest ${rows.length} rows (newest first)`}</div>
    </div>
  );
}

function isFresh(stat: DataStat | undefined): boolean {
  if (!stat?.latest) return false;
  return Date.now() - Date.parse(stat.latest) < 15 * 60 * 1000; // seen within 15 min
}
// data stream as a scorecard row (mirrors the model rows): freshness dot + 24h rate + expand.
function StreamRow({ meta, stat, lang }: { meta: StreamMeta; stat: DataStat | undefined; lang: Lang }) {
  const k = ko(lang);
  const i = k ? 0 : 1;
  const [open, setOpen] = useState(false);
  const [rows, setRows] = useState<DataRow[] | null>(null);
  const [loading, setLoading] = useState(false);
  const toggle = () => {
    const next = !open;
    setOpen(next);
    if (next && rows == null && !loading) {
      setLoading(true);
      api.learnDataSample(meta.key).then((r) => { setRows(r); setLoading(false); }).catch(() => { setRows([]); setLoading(false); });
    }
  };
  const fresh = isFresh(stat);
  return (
    <div className={`sr${open ? " open" : ""}`}>
      <button className="sr-head data" onClick={toggle}>
        <span className="sr-dot" style={{ background: fresh ? "#22c55e" : "#f59e0b" }} title={fresh ? (k ? "정상 흐름" : "flowing") : (k ? "지연" : "stale")} />
        <div className="sr-id"><div className="sr-name">{meta.name[i]}<code className="sr-en">{meta.key}</code></div><div className="sr-tags"><span className="sr-type">{meta.source[i]}</span></div></div>
        <div className="sr-story">{meta.desc[i]}</div>
        <div className="sr-headline"><span className="sr-num">{fmtN(stat?.n_24h)}</span><span className="sr-hlabel">{k ? "최근 24h 행" : "rows / 24h"}</span></div>
        <div className="sr-trend"><span className="ls-track-lbl">{k ? "1h" : "1h"} {fmtN(stat?.n_1h)} · {stamp(stat?.latest)}</span></div>
        <span className="sr-chev">{open ? "▲" : "▼"}</span>
      </button>
      {open && <div className="sr-body">
        <div className="ls-surfaces"><span className="ls-surfaces-k">{k ? "쓰임" : "used by"}</span><span className="ls-surface">{meta.usage[i]}</span></div>
        <SampleTable rows={rows} loading={loading} lang={lang} />
      </div>}
    </div>
  );
}

function DataTab({ lang }: { lang: Lang }) {
  const k = ko(lang);
  const [cat, setCat] = useState<DataStat[] | null>(null);
  const [err, setErr] = useState(false);
  useEffect(() => {
    let alive = true;
    const load = () => api.learnDataCatalog().then((r) => { if (alive) { setCat(r); setErr(false); } }).catch(() => alive && setErr(true));
    load();
    const id = setInterval(load, 60000);
    return () => { alive = false; clearInterval(id); };
  }, []);
  const statOf = (key: string) => cat?.find((s) => s.key === key);

  const streams = DATA_CATALOG.flatMap((c) => c.streams);
  const nFresh = streams.filter((m) => isFresh(statOf(m.key))).length;
  const nStale = streams.length - nFresh;
  const total24 = (cat ?? []).reduce((a, s) => a + (s.n_24h ?? 0), 0);
  const latest = (cat ?? []).map((s) => s.latest).filter(Boolean).sort().slice(-1)[0];
  const total = streams.length || 1;

  return (
    <>
      <div className="hs">
        <div className="hs-fleet">
          <b>{streams.length} {k ? "스트림" : "streams"}</b>
          <div className="hs-bar"><div className="hs-seg" style={{ width: `${(nFresh / total) * 100}%`, background: "#22c55e" }} /><div className="hs-seg" style={{ width: `${(nStale / total) * 100}%`, background: "#f59e0b" }} /></div>
          <span className="hs-legend"><span style={{ color: "#22c55e" }}>● {nFresh} {k ? "정상" : "flowing"}</span> · <span style={{ color: "#f59e0b" }}>◐ {nStale} {k ? "지연" : "stale"}</span></span>
        </div>
        {err && <span className="cyc-err">{k ? "· 연결 오류" : "· offline"}</span>}
        <div className="hs-meta">{k ? "최근 24h" : "24h"} {fmtN(total24)}{k ? "행" : ""} · {k ? "갱신" : "upd"} {stamp(latest)}</div>
      </div>
      {DATA_CATALOG.map((c) => (
        <div className="sb" key={c.title[0]}>
          <div className="sb-h"><span className="sb-t">{k ? c.title[0] : c.title[1]}</span><span className="sb-count">{c.streams.length}{k ? "개" : ""}</span></div>
          {c.streams.map((m) => <StreamRow key={m.key} meta={m} stat={statOf(m.key)} lang={lang} />)}
        </div>
      ))}
    </>
  );
}

// ───────────────────────────── 학습 센터 (탭 셸) ─────────────────────────────
export default function LearnPage({ lang }: { lang: Lang }) {
  const k = ko(lang);
  const [tab, setTab] = useState<"models" | "data">("models");
  return (
    <div className="content cyc-page">
      <div className="cyc-head">
        <div className="cyc-title">
          <h2>{k ? "학습 센터" : "Learning Center"}</h2>
          <span className="cyc-title-sub">{tab === "models"
            ? (k ? "우리 배차를 만드는 학습 모델 — 카드를 눌러 펼치기 · 배지=가동/검증중/자가보정" : "the learning models behind our dispatch — click to expand · badge = live / validating / self-cal")
            : (k ? "우리가 수집하는 데이터 — 소스·쓰임·수집량 + 최근 내역" : "data we collect — source/usage/volume + recent rows")}</span>
        </div>
        <div className="ls-tabs">
          <button className={`ls-tab${tab === "models" ? " active" : ""}`} onClick={() => setTab("models")}>🧠 {k ? "예측 모델" : "Models"}</button>
          <button className={`ls-tab${tab === "data" ? " active" : ""}`} onClick={() => setTab("data")}>🗄️ {k ? "데이터 수집" : "Data"}</button>
        </div>
      </div>
      {tab === "models" ? <ModelsTab lang={lang} /> : <DataTab lang={lang} />}
    </div>
  );
}
