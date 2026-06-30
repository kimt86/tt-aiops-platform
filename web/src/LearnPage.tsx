// 학습 센터 — 2탭 구성:
//   🧠 예측 모델 — 각 모델 카드(입력·출력·담당역할 + KPI + KPI 트렌드 차트)
//   🗄️ 데이터 수집 — 수집 스트림 카드(설명·소스·쓰임·총/최근 건수 + 펼치면 최근 데이터)
import { useEffect, useMemo, useState } from "react";
import { type Lang } from "./i18n";
import { api, type LearnTopos, type LearnToposPoint, type LanesData, type TravelData, type TravelOd, type SoonIdleData, type SoonIdleLead, type DispatchPredData, type EvalPoint, type DataStat, type DataRow } from "./api";
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
const mins = (s: number | null | undefined) => (s == null ? "—" : `${(s / 60).toFixed(1)}`);
const distLabel = (b: number, k: boolean) => (b === 0 ? "≤30m" : b === 1 ? "30–80m" : b === 2 ? "80–150m" : b === 3 ? ">150m" : k ? "RTG없음" : "no-RTG");

const ISO_RE = /^\d{4}-\d\d-\d\dT\d\d:\d\d/;
// format one sample-table cell: timestamps→MYT, bools→✓/✗, integers→grouped, floats verbatim.
function fmtCell(v: string | number | boolean | null): string {
  if (v == null) return "—";
  if (typeof v === "boolean") return v ? "✓" : "·";
  if (typeof v === "number") return Number.isInteger(v) ? v.toLocaleString() : String(v);
  if (ISO_RE.test(v)) return stampS(v);
  return v;
}

// first→last change of a quality series. `higherBetter` decides what counts as "improving".
function trend(series: number[], higherBetter: boolean): { dir: 1 | -1 | 0; pct: number; improving: boolean } | null {
  const v = series.filter((x) => x != null && !Number.isNaN(x));
  if (v.length < 2) return null;
  const first = v[0], last = v[v.length - 1];
  const delta = last - first;
  const p = first !== 0 ? (delta / Math.abs(first)) * 100 : delta !== 0 ? 100 : 0;
  const dir = Math.abs(p) < 2 ? 0 : delta > 0 ? 1 : -1;
  return { dir, pct: Math.abs(p), improving: higherBetter ? delta > 0 : delta < 0 };
}

function TrendBadge({ series, higherBetter, lang }: { series: number[]; higherBetter: boolean; lang: Lang }) {
  const t = trend(series, higherBetter);
  if (!t) return <span className="ls-tb flat">{ko(lang) ? "수집 중" : "collecting"}</span>;
  if (t.dir === 0) return <span className="ls-tb flat">→ {ko(lang) ? "안정" : "stable"}</span>;
  const arrow = t.dir === 1 ? "↑" : "↓";
  return <span className={`ls-tb ${t.improving ? "up" : "bad"}`}>{arrow} {t.pct.toFixed(0)}% {t.improving ? (ko(lang) ? "개선" : "better") : (ko(lang) ? "악화" : "worse")}</span>;
}

// one panel (a column inside a card): a tag (📈 learning / 🧪 test) + content.
function Panel({ tag, test, children }: { tag: string; test?: boolean; children: React.ReactNode }) {
  return (
    <div className={`ls-panel${test ? " test" : ""}`}>
      <div className="ls-ptag">{test ? "🧪 " : "📈 "}{tag}</div>
      {children}
    </div>
  );
}

// a metric block: headline = last value of its trend series (always matches the chart) + badge + chart.
function Metric({ series, fmt, label, color, higherBetter, lang }: { series: number[]; fmt: (v: number) => string; label: string; color: string; higherBetter: boolean; lang: Lang }) {
  const v = series.filter((x) => x != null && !Number.isNaN(x));
  const last = v.length ? v[v.length - 1] : null;
  return (
    <>
      <div className="ls-pv">{last != null ? fmt(last) : "—"} <TrendBadge series={series} higherBetter={higherBetter} lang={lang} /></div>
      <div className="ls-plabel">{label}</div>
      <div className="ls-pchart">{series.length > 1 ? <LineChart values={series} color={color} axes /> : <div className="cyc-empty">{ko(lang) ? "스냅샷 수집 중" : "collecting snapshots"}</div>}</div>
    </>
  );
}

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

function Session({ n, title, sub, accent, children }: { n: number; title: string; sub: string; accent: string; children: React.ReactNode }) {
  return (
    <section className="ls-card" style={{ borderTopColor: accent }}>
      <div className="ls-head">
        <span className="ls-n" style={{ color: accent }}>{n}</span>
        <div><h3>{title}</h3><span className="ls-sub">{sub}</span></div>
      </div>
      {children}
    </section>
  );
}

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
function ModelsTab({ lang }: { lang: Lang }) {
  const k = ko(lang);
  const [d, setD] = useState<LearnTopos | null>(null);
  const [ln, setLn] = useState<LanesData | null>(null);
  const [tv, setTv] = useState<TravelData | null>(null);
  const [si, setSi] = useState<SoonIdleData | null>(null);
  const [dp, setDp] = useState<DispatchPredData | null>(null);
  const [ev, setEv] = useState<EvalPoint[] | null>(null);
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
      api.learnEval().then((r) => { if (alive) setEv(r); }).catch(() => {});
    };
    load();
    const id = setInterval(load, 30000);
    return () => { alive = false; clearInterval(id); };
  }, []);

  // ① travel — learning = accumulating samples; test = predicted-vs-actual (accuracy block)
  const sampVals = (tv?.metric_series ?? []).map((p) => Number(p.samples));
  // ③ topos — learning = confident points (n≥30); test = positional residual (median spread ↓)
  const ms = d?.metric_series ?? [];
  const confTopoVals = ms.map((p) => p.confident_topos);
  const spreadVals = ms.map((p) => p.median_spread_m ?? 0).filter((v) => v > 0);
  // ④ lanes — learning = road cells; test = directional consistency (one-way fraction ↑)
  const lms = ln?.metric_series ?? [];
  const roadVals = lms.map((p) => p.road_cells);
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
  const siGrid = "50px 84px 56px 64px 64px 72px";
  const dse = si?.ds_eta; // ⑥ DS minutes-to-idle feature model

  const points = useMemo(() => {
    let pts = d?.points ?? [];
    if (onlyBlock) pts = pts.filter((p) => !p.is_crane);
    if (q) pts = pts.filter((p) => p.topos.toLowerCase().includes(q.toLowerCase()));
    return pts;
  }, [d, onlyBlock, q]);

  const acc = tv?.accuracy;

  return (
    <>
      {err && <div className="cyc-err" style={{ marginBottom: 8 }}>{k ? "· 연결 오류" : "· offline"}</div>}

      {/* ① TT 이동시간 */}
      <Session n={1} accent="#60a5fa" title={k ? "TT 이동시간 예측" : "TT travel time"} sub={k ? "사이클에서 수확한 출발→도착 trip + 피처(경로거리·존·밀도·날씨)" : "trips harvested from cycles + features"}>
        <IoStrip accent="#60a5fa" lang={lang}
          inputs={k ? "출발존→도착존 + 경로거리(맨해튼)·밀도·시간대·날씨" : "origin→dest zone + route dist, density, hour, weather"}
          output={k ? "구간 공차 이동시간(초) 분포 — 중앙·p50/p90" : "empty-travel seconds per O→D (median, p50/p90)"}
          role={k ? "2단계 비용행렬의 '공차 도착시간(arr)' — 배차 효율의 핵심 입력" : "the empty-travel arrival in the Stage-2 cost matrix"} />
        <div className="ls-cols">
          <Panel tag={k ? "학습 추이 — 누적 학습 표본" : "learning — samples"}>
            <Metric series={sampVals} fmt={fmtN} label={k ? "누적 학습 표본 (커버리지)" : "accumulating samples"} color="#60a5fa" higherBetter lang={lang} />
          </Panel>
          <Panel test tag={k ? "최신 테스트 — 예측(OD 중앙값) vs 실제 (지난 2일·신뢰 OD)" : "test — OD median vs actual (2d)"}>
            {acc && acc.evaluated > 0 ? (
              <div className="ls-testchips">
                <Chip label={k ? "중앙 오차율 (MAPE)" : "MAPE"} value={acc.mape_pct != null ? `${acc.mape_pct.toFixed(0)}%` : "—"} accent="#f59e0b" />
                <Chip label={k ? "±30% 적중률" : "within ±30%"} value={acc.within_30pct != null ? `${acc.within_30pct.toFixed(0)}%` : "—"} accent="#34d399" />
                <Chip label={k ? "중앙 절대오차" : "median abs err"} value={mmss(acc.median_abs_err_s)} />
                <Chip label={k ? "평가 trip" : "evaluated"} value={fmtN(acc.evaluated)} />
              </div>
            ) : <div className="cyc-empty">{k ? "trip 완료 대기 중" : "awaiting trips"}</div>}
            <div className="ls-paside">{k ? "trip 완료마다 갱신. 처음 예측(OD 중앙값)과 실제 시간의 차이." : "updates per completed trip — predicted vs actual."}</div>
          </Panel>
        </div>
        <div className="ls-chips">
          <Chip label={k ? "신뢰 OD쌍 (n≥10)" : "confident O→D"} value={tv ? fmtN(tv.confident_pairs) : "—"} accent="#34d399" />
          <Chip label={k ? "OD쌍" : "O→D pairs"} value={tv ? fmtN(tv.od_pairs) : "—"} />
          <Chip label={k ? "중앙 속도" : "median speed"} value={tv ? `${kmh(tv.median_speed_kmh)} km/h` : "—"} />
        </div>
        <div className="ls-note">{k ? "표본·신뢰 OD쌍이 늘며 커버리지는 개선됨. 단 같은 OD의 시간 변동(±50%)은 야드 확률성에 의한 본질적 천장 — 점예측보다 분포로 사용." : "Coverage grows with samples; within-OD variance (±50%) is a structural ceiling — use as a distribution."}</div>
        <details className="ls-detail">
          <summary>{k ? "상세 — 구간별 이동시간 (표본 많은 순)" : "detail — travel time by O→D"}</summary>
          <div className="learn-od-cols" style={{ marginTop: 8 }}>
            <span>{k ? "출발" : "origin"}</span><span>{k ? "도착" : "dest"}</span><span>n</span><span>{k ? "중앙시간" : "median"}</span><span>{k ? "거리" : "dist"}</span><span>km/h</span>
          </div>
          <div className="learn-list">
            {(tv?.od ?? []).length === 0 && <div className="cyc-empty">{k ? "아직 구간 표본 없음 (사이클에서 수확 중)" : "none yet"}</div>}
            {(tv?.od ?? []).slice(0, 250).map((o) => <OdRow key={o.origin + "→" + o.dest} o={o} />)}
          </div>
        </details>
      </Session>

      {/* ② 순수 주행시간 예측 */}
      {(() => {
        const evA = [...(ev ?? [])].reverse(); // ascending for charts
        const last = ev?.[0];
        const odMape = evA.map((p) => p.od_mape ?? NaN);
        return (
          <Session n={2} accent="#a78bfa" title={k ? "순수 주행시간 예측" : "Pure driving-time prediction"} sub={k ? "정지/큐 제외 순수 주행 — 매시간 자동 학습·테스트(예측 vs 실측)" : "stop-excluded driving — hourly train/test"}>
            <IoStrip accent="#a78bfa" lang={lang}
              inputs={k ? "3초 GPS 트레이스(정지·큐 구간 제외) → 순수 주행 leg" : "3s GPS traces, stop/queue excluded → pure-drive leg"}
              output={k ? "순수 주행시간(초) + 추론 도로망 그래프" : "pure driving seconds + inferred road graph"}
              role={k ? "이동시간 예측의 상한 정확도 검증 + 방향 도로 라우팅의 기반" : "upper-bound accuracy check + basis for directed routing"} />
            <div className="ls-cols">
              <Panel tag={k ? "학습 추이 — 예측 정확도 (OD MAPE, 낮을수록 좋음)" : "learning — accuracy (OD MAPE, lower better)"}>
                <Metric series={odMape} fmt={(v) => `${v.toFixed(0)}%`} label={k ? "OD-순수 모델 오차율" : "OD-pure MAPE"} color="#a78bfa" higherBetter={false} lang={lang} />
              </Panel>
              <Panel test tag={k ? "최신 테스트 — 예측 vs 실측 (지난 2일 leg, 매시간)" : "test — predicted vs actual (2d, hourly)"}>
                {last && (last.n_legs ?? 0) > 0 ? (
                  <div className="ls-testchips">
                    <Chip label={k ? "OD모델 오차율" : "OD MAPE"} value={last.od_mape != null ? `${last.od_mape.toFixed(0)}%` : "—"} accent="#34d399" />
                    <Chip label={k ? "맨해튼 기준선" : "Manhattan"} value={last.manh_mape != null ? `${last.manh_mape.toFixed(0)}%` : "—"} accent="#f59e0b" />
                    <Chip label={k ? "OD 절대오차" : "OD MAE"} value={last.od_mae_s != null ? `${last.od_mae_s}s` : "—"} />
                    <Chip label={k ? "평가 leg" : "legs"} value={last.n_legs != null ? fmtN(last.n_legs) : "—"} />
                  </div>
                ) : <div className="cyc-empty">{k ? "평가 대기 중" : "awaiting eval"}</div>}
                <div className="ls-paside">{k ? "매시간 자동. 학습 OD 모델이 맨해튼 기준선보다 얼마나 정확한가." : "hourly — learned OD vs Manhattan baseline."}</div>
              </Panel>
            </div>
            <div className="ls-chips">
              <Chip label={k ? "3초 GPS 누적 (도로망 추론용)" : "3s GPS pts"} value={last ? fmtN(Number(last.hifreq_pts ?? 0)) : "—"} accent="#a78bfa" />
              <Chip label={k ? "순수주행 표본" : "drive samples"} value={last ? fmtN(Number(last.drive_samples ?? 0)) : "—"} accent="#34d399" />
              <Chip label={k ? "순수 OD쌍 (n≥10)" : "pure O→D pairs"} value={last ? fmtN(last.pure_pairs ?? 0) : "—"} />
            </div>
            <div className="ls-note">{k ? "데이터가 쌓이며 순수 OD 커버리지·정확도가 개선됨. 3초 GPS는 도로망 그래프 추론용으로 누적 중 — 며칠 후 재추론·라우팅 재백테스트 예정." : "Coverage/accuracy improve as data accumulates; 3s GPS feeds road-network graph inference."}</div>
          </Session>
        );
      })()}

      {/* ③ 작업지점 좌표 */}
      <Session n={3} accent="#0ea5e9" title={k ? "작업지점 좌표" : "Work-point coordinates"} sub={k ? "TT가 작업점에 도착한 GPS를 누적 → 블록·크레인 중심좌표" : "GPS at arrival accumulated → centroid coords"}>
        <IoStrip accent="#0ea5e9" lang={lang}
          inputs={k ? "TT가 작업점에 도착(ARRIVED)한 순간의 GPS" : "GPS at the moment a TT arrives at a work-point"}
          output={k ? "블록·크레인 중심좌표(lat,lon) + 신뢰도" : "block/crane centroid coords (lat,lon) + confidence"}
          role={k ? "존 정의·거리/비용 계산의 위치 앵커 (이동 크레인은 GPS snap)" : "the location anchor for zones & cost distances"} />
        <div className="ls-cols">
          <Panel tag={k ? "학습 추이 — 확신 작업지점 (n≥30)" : "learning — confident points"}>
            <Metric series={confTopoVals} fmt={fmtN} label={k ? "잘 학습된 점 수" : "confident points (n≥30)"} color="#34d399" higherBetter lang={lang} />
          </Panel>
          <Panel test tag={k ? "최신 테스트 — 위치 잔차 (학습좌표 vs 실제 GPS)" : "test — positional residual"}>
            <Metric series={spreadVals} fmt={mPrec} label={k ? "중앙 잔차 ±m (낮을수록 정확)" : "median residual ±m (lower better)"} color="#f59e0b" higherBetter={false} lang={lang} />
          </Panel>
        </div>
        <div className="ls-chips">
          <Chip label={k ? "학습 지점" : "learned points"} value={d ? fmtN(d.distinct_topos) : "—"} />
          <Chip label={k ? "블록 지점" : "block points"} value={d ? fmtN(d.block_points) : "—"} />
          <Chip label={k ? "누적 관측" : "observations"} value={d ? fmtN(d.total_obs) : "—"} />
        </div>
        <div className="ls-note">{k ? "GPS가 쌓일수록 좌표 군집이 좁아져(잔차 ±m↓) 더 정밀해지고 확신 지점이 늘어납니다. 라이브맵에서 신뢰도 색으로 확인." : "More GPS → tighter clusters (residual ±m↓) + more confident points."}</div>
        <details className="ls-detail">
          <summary>{k ? "상세 — 학습된 작업지점 (관측 많은 순)" : "detail — learned work-points"}</summary>
          <div className="cyc-board-head" style={{ marginTop: 8, border: 0 }}>
            <span style={{ display: "flex", gap: 10, alignItems: "center" }}>
              <label style={{ fontSize: 11, color: "var(--text-dim)", cursor: "pointer" }}>
                <input type="checkbox" checked={onlyBlock} onChange={(e) => setOnlyBlock(e.target.checked)} /> {k ? "블록만" : "blocks only"}
              </label>
              <input className="cyc-search mono" placeholder={k ? "topos 검색" : "find topos"} value={q} onChange={(e) => setQ(e.target.value)} />
            </span>
          </div>
          <div className="learn-cols">
            <span>topos</span><span>{k ? "종류" : "kind"}</span><span>n</span><span>{k ? "누적" : "obs"}</span><span>{k ? "정밀도" : "prec"}</span><span>{k ? "좌표" : "coord"}</span><span>{k ? "갱신" : "updated"}</span>
          </div>
          <div className="learn-list">
            {points.length === 0 && <div className="cyc-empty">{k ? "아직 학습된 지점 없음" : "none yet"}</div>}
            {points.slice(0, 300).map((p) => <PointRow key={p.topos} p={p} lang={lang} />)}
          </div>
        </details>
      </Session>

      {/* ④ 주행 차선 */}
      <Session n={4} accent="#34d399" title={k ? "주행 차선·도로 방향" : "Driving lanes & direction"} sub={k ? "이동 TT의 GPS 트레이스를 22m 격자에 집계 → 도로·방향" : "moving-TT traces aggregated to a 22m grid → roads & direction"}>
        <IoStrip accent="#34d399" lang={lang}
          inputs={k ? "이동 TT의 GPS 트레이스 (22m 격자에 집계)" : "moving-TT GPS traces, binned to a 22m grid"}
          output={k ? "도로 셀·통행 방향(heading)·평균 속도" : "road cells, travel direction (heading), mean speed"}
          role={k ? "방향성 도로 그래프 → 경로 라우팅(맨해튼 대비 개선)" : "directed road graph → route distances (beats Manhattan)"} />
        <div className="ls-cols">
          <Panel tag={k ? "학습 추이 — 학습된 도로 셀 (통과≥20)" : "learning — road cells"}>
            <Metric series={roadVals} fmt={fmtN} label={k ? "도로 커버리지 (셀 수)" : "road coverage (cells)"} color="#34d399" higherBetter lang={lang} />
          </Panel>
          <Panel test tag={k ? "최신 테스트 — 방향 일관성 (학습 방향 vs 실제)" : "test — directional consistency"}>
            <Metric series={onewayVals} fmt={(v) => `${v.toFixed(0)}%`} label={k ? "일방통행으로 또렷한 셀 비율" : "clearly one-way cells"} color="#a78bfa" higherBetter lang={lang} />
          </Panel>
        </div>
        <div className="ls-chips">
          <Chip label={k ? "전체 셀" : "total cells"} value={ln ? fmtN(ln.cells) : "—"} />
          <Chip label={k ? "누적 통과" : "passes"} value={ln ? fmtN(ln.total_passes) : "—"} />
        </div>
        <div className="ls-note">{k ? "트럭 GPS가 쌓일수록 더 많은 도로 셀이 학습되고, 셀 방향이 한쪽으로 또렷(일방 비율↑)해집니다 = 학습 방향이 실제 흐름과 일치. 차선망은 라이브맵 → 레이어 → 주행 차선에서 화살표로 확인." : "More GPS → more road cells + clearer per-cell direction (one-way↑). See arrows on the live map."}</div>
      </Session>

      {/* ⑤ Soon-idle 예측 정확도 */}
      <Session n={5} accent="#a78bfa" title={k ? "Soon-idle (곧 빔) 예측" : "Soon-idle prediction"} sub={k ? "그림자: 예측 vs 실제 트럭 빔 — GPS 우선(사이클 dropped_at)·TOS 폴백 · DS·LD" : "shadow: prediction vs physical truck-freed — GPS-first (cycle dropped_at), TOS fallback"}>
        <IoStrip accent="#a78bfa" lang={lang}
          inputs={k ? "트럭 상태·RTG 거리·발화 신호(GPS/PLC/TOS)" : "truck state, RTG distance, firing signal (GPS/PLC/TOS)"}
          output={k ? "'곧 빔' 여부 + 몇 분 후 유휴(리드타임)" : "soon-idle flag + minutes-to-idle (lead time)"}
          role={k ? "배차 공급 예측 — 곧 빔 트럭을 2단계 후보 풀에 선편입" : "supply forecast — pre-admit soon-free trucks to the Stage-2 pool"} />
        <div className="ls-ptag" style={{ marginBottom: 8 }}>📈 {k ? "학습 추이 — DS·LD 재현율 (24h 스냅샷)" : "learning — DS·LD recall trend"}</div>
        <div className="learn-charts">
          <div className="cyc-tp">
            <div className="cyc-sec-h">{k ? "DS 재현율" : "DS recall"} <TrendBadge series={siRecallSeries} higherBetter lang={lang} /></div>
            <div className="cyc-tp-box">{siRecallSeries.length > 1 ? <LineChart values={siRecallSeries} color="#fb923c" axes /> : <div className="cyc-empty">{k ? "수집 중" : "collecting"}</div>}</div>
          </div>
          <div className="cyc-tp">
            <div className="cyc-sec-h">{k ? "LD 재현율" : "LD recall"} <TrendBadge series={ldRecallSeries} higherBetter lang={lang} /></div>
            <div className="cyc-tp-box">{ldRecallSeries.length > 1 ? <LineChart values={ldRecallSeries} color="#22d3ee" axes /> : <div className="cyc-empty">{k ? "수집 중" : "collecting"}</div>}</div>
          </div>
        </div>
        <div className="ls-ptag test" style={{ margin: "12px 0 8px" }}>🧪 {k ? "최신 테스트 — '몇 분 후 유휴' 예측(학습 중앙값) vs 실제 (적중분)" : "test — 'minutes-to-idle' prediction (learned median) vs actual"}</div>
        <div className="ls-leads">
          <LeadCard jt="DS" accent="#fb923c" lead={dsJob.lead} recall={dsJob.recall} recallGps={dsJob.recallGps} precision={dsJob.precision} lang={lang} />
          <LeadCard jt="LD" accent="#22d3ee" lead={ldJob.lead} recall={ldJob.recall} recallGps={ldJob.recallGps} precision={ldJob.precision} lang={lang} />
        </div>
        <div className="ls-chips">
          <Chip label={k ? "예측 (7일)" : "predictions (7d)"} value={si ? fmtN(si.predictions) : "—"} />
          <Chip label={k ? "적중" : "matched"} value={si ? fmtN(si.matched) : "—"} />
          <Chip label={k ? "전체 정밀도" : "precision"} value={si?.precision_pct != null ? `${si.precision_pct.toFixed(0)}%` : "—"} />
        </div>
        <div className="ls-note">{k ? "정답 = 트럭 GPS가 잡은 '실제 빈 순간'(tt_cycle_v2.dropped_at) 우선 — TOS 라벨보다 커버리지 넓고(LD 적중 6.4k→11.3k) 물리적 빔과 0.5분 일치. GPS 공백 시 TOS 폴백. 분 예측=학습 중앙값이나 per-건은 QC/RTG 큐 변동으로 오차 큼(분포로 사용)." : "Ground truth = GPS-first physical free (tt_cycle_v2.dropped_at) — broader coverage than TOS. Per-truck error is large (queue stochasticity) — use as a distribution."}</div>
        <details className="ls-detail">
          <summary>{k ? "상세 — 신호별 정밀도·리드타임" : "detail — precision & lead by signal"}</summary>
          <div className="learn-list" style={{ marginTop: 8 }}>
            <div style={{ display: "grid", gridTemplateColumns: siGrid, gap: 8, padding: "3px 6px", fontWeight: 600, color: "var(--text-dim)", fontSize: 12 }}>
              <span>{k ? "작업" : "job"}</span><span>{k ? "신호" : "signal"}</span><span>{k ? "예측" : "pred"}</span><span>{k ? "적중" : "match"}</span><span>{k ? "정밀도" : "prec"}</span><span>{k ? "리드p50" : "lead"}</span>
            </div>
            {(si?.by_source ?? []).map((s) => (
              <div key={`${s.jobtype}-${s.source}`} style={{ display: "grid", gridTemplateColumns: siGrid, gap: 8, padding: "3px 6px" }}>
                <span className="mono">{s.jobtype}</span>
                <span className="mono" style={{ color: s.source === "tos_actv" ? "#a78bfa" : s.source === "gps_rtg" ? "#34d399" : s.source === "qc_plc" ? "#0ea5e9" : s.source === "both" ? "#22d3ee" : "#94a3b8" }}>{s.source}</span>
                <span className="mono">{s.predictions}</span><span className="mono">{s.matched}</span>
                <span className="mono">{s.precision_pct != null ? `${s.precision_pct.toFixed(0)}%` : "—"}</span>
                <span className="mono">{mmss(s.lead_p50_s)}</span>
              </div>
            ))}
            {(si?.by_source?.length ?? 0) === 0 && <div className="cyc-empty">{k ? "예측 수집 중" : "collecting"}</div>}
          </div>
        </details>
      </Session>

      {/* ⑥ 유휴 분 예측 — 피처 정밀화 (DS·LD 통합) */}
      <Session n={6} accent="#fb923c" title={k ? "유휴 분 예측 — 피처 정밀화 (DS·LD)" : "minutes-to-idle — feature refinement (DS·LD)"} sub={k ? "'몇 분 후 유휴'를 피처로 더 맞출 수 있나 — DS=RTG거리·신호 / LD=안벽이라 피처 없음" : "can features sharpen 'minutes-to-idle' — DS uses RTG distance×signal; LD is quay-side"}>
        <IoStrip accent="#fb923c" lang={lang}
          inputs={k ? "DS=RTG 거리 × 발화 신호 / LD=안벽(유효 피처 없음)" : "DS = RTG distance × firing signal / LD = quay-side (no feature)"}
          output={k ? "셀별 '몇 분 후 유휴' 중앙 예측 + p10~p90" : "per-cell minutes-to-idle median + p10~p90"}
          role={k ? "Soon-idle 분 예측을 거리·신호로 정밀화 (배차 공급 타이밍)" : "sharpens soon-idle minutes via distance×signal"} />
        <div className="ls-ptag" style={{ marginBottom: 8 }}>📈 {k ? "예측기 — 작업유형별 '몇 분 후 유휴'" : "predictor — minutes-to-idle per jobtype"}</div>
        <div className="ls-cols">
          {/* DS predictor — distance × signal cells */}
          <div className="ls-panel" style={{ borderTop: "2px solid #fb923c" }}>
            <div className="ls-ptag" style={{ color: "#fb923c" }}>DS · {k ? "양하 — RTG 거리 × 신호" : "discharge — RTG dist × signal"}</div>
            <div className="ds-cells">
              <div className="ds-cell hd"><span>{k ? "RTG거리" : "dist"}</span><span>{k ? "신호" : "signal"}</span><span>{k ? "예측" : "pred"}</span><span>p10~p90</span><span>n</span></div>
              {(si?.ds_eta_cells ?? []).map((c) => (
                <div className="ds-cell" key={`${c.dist_bin}-${c.source}`}>
                  <span className="mono">{distLabel(c.dist_bin, k)}</span>
                  <span className="mono" style={{ fontSize: 11 }}>{c.source}</span>
                  <span className="mono" style={{ color: "#fb923c", fontWeight: 700 }}>{mins(c.pred_s)}{k ? "분" : "m"}</span>
                  <span className="mono" style={{ fontSize: 11, color: "var(--text-mute)" }}>{mins(c.p10_s)}~{mins(c.p90_s)}</span>
                  <span className="mono">{c.n}</span>
                </div>
              ))}
              {(si?.ds_eta_cells?.length ?? 0) === 0 && <div className="cyc-empty">{k ? "수집 중" : "collecting"}</div>}
            </div>
            <div className="ls-paside">{k ? "거리가 중앙 예측을 밀어줌(≤30m 4분 → >150m 6.7분)." : "Distance shifts the central estimate."}</div>
          </div>
          {/* LD predictor — flat median (no useful feature, quay-side) */}
          <div className="ls-panel" style={{ borderTop: "2px solid #22d3ee" }}>
            <div className="ls-ptag" style={{ color: "#22d3ee" }}>LD · {k ? "적하 — 유효 피처 없음" : "load — no useful feature"}</div>
            <div className="ls-pv">{ldJob.lead?.lead_p50_s != null ? mins(ldJob.lead.lead_p50_s) : "—"}<span className="ls-lead-u">{k ? "분 후 유휴 (중앙)" : "min (median)"}</span></div>
            <div className="ls-plabel">p10~p90 {mins(ldJob.lead?.lead_p10_s)}~{mins(ldJob.lead?.lead_p90_s)}{k ? "분" : "m"} · n {ldJob.lead?.matched ?? "—"}</div>
            <div className="ls-paside">{k ? "안벽이라 RTG 거리=NULL·신호=qc_plc 고정. QC·시간대·큐 모두 홀드아웃 검증했으나 flat과 동일(~65%) — QC 큐 확률성 지배. 예측=학습 중앙값." : "Quay-side: RTG dist=NULL, signal fixed. QC/hour/queue all tested held-out ≈ flat (~65%). Predict the learned median."}</div>
          </div>
        </div>
        <div className="ls-ptag test" style={{ margin: "12px 0 8px" }}>🧪 {k ? "최신 테스트 — 예측 vs 실제 (GPS-우선 정답, 7일)" : "test — predicted vs actual (GPS-first truth, 7d)"}</div>
        <div className="ls-cols">
          {/* DS test */}
          <div className="ls-panel" style={{ borderTop: "2px solid #fb923c" }}>
            <div className="ls-ptag" style={{ color: "#fb923c" }}>DS</div>
            <div className="ls-pv">{dse?.feat_mape_pct != null ? `${dse.feat_mape_pct.toFixed(0)}%` : "—"}<span className="ls-lead-u" style={{ marginLeft: 6 }}>MAPE</span></div>
            <div className="ls-plabel">{k ? "평균" : "flat"} {dse?.flat_mape_pct != null ? `${dse.flat_mape_pct.toFixed(0)}%` : "—"} → {k ? "거리·신호" : "features"} <b style={{ color: "#fb923c" }}>{dse?.feat_mape_pct != null ? `${dse.feat_mape_pct.toFixed(0)}%` : "—"}</b> · ±30% {k ? "적중" : ""} {dse?.within_30pct != null ? `${dse.within_30pct.toFixed(0)}%` : "—"} · {k ? "평가" : "n"} {dse ? fmtN(dse.evaluated) : "—"}</div>
          </div>
          {/* LD test */}
          <div className="ls-panel" style={{ borderTop: "2px solid #22d3ee" }}>
            <div className="ls-ptag" style={{ color: "#22d3ee" }}>LD</div>
            <div className="ls-pv">{ldJob.lead?.mape_pct != null ? `${ldJob.lead.mape_pct.toFixed(0)}%` : "—"}<span className="ls-lead-u" style={{ marginLeft: 6 }}>MAPE</span></div>
            <div className="ls-plabel">±30% {k ? "적중" : ""} <b style={{ color: "#22d3ee" }}>{ldJob.lead?.within_30pct != null ? `${ldJob.lead.within_30pct.toFixed(0)}%` : "—"}</b> · {k ? "평가" : "n"} {ldJob.lead?.matched ?? "—"}</div>
          </div>
        </div>
        <div className="ls-note">{k ? "예측기 = 작업유형별 학습 중앙(+DS는 거리·신호 셀). 정답=GPS-우선 빔, 7일. 둘 다 per-건 오차는 QC/RTG 큐 변동으로 큼 — 점이 아닌 분포로 사용." : "Predictor = learned median per jobtype (DS adds distance×signal cells). Truth=GPS-first. Per-truck error is large — use as a distribution."}</div>
      </Session>

      {/* ⑦ 배차 작업시점 예측 (1단계) */}
      <Session n={7} accent="#f472b6" title={k ? "배차 작업시점 예측 (1단계)" : "Dispatch work-time prediction (Stage 1)"} sub={k ? "출항 역산 + 학습한 크레인 속도로 '크레인이 각 컨테이너를 작업할 시각'을 예측 → 배차 마감 산정" : "vessel-departure backsolve + learned crane pace → per-container work time → dispatch deadline"}>
        <IoStrip accent="#f472b6" lang={lang}
          inputs={k ? "출항(estdep) 역산 + 학습 크레인 속도(learn_qc_move_time) + 베이 작업큐" : "departure backsolve + learned crane pace + bay work-queue"}
          output={k ? "컨테이너별 작업 예정시각 → 배차 마감시각" : "per-container work time → dispatch deadline"}
          role={k ? "1단계 — 긴급도·배차 마감 산정 (위치 무관)" : "Stage 1 — urgency & dispatch deadline (location-independent)"} />
        <div className="ls-cols">
          <Panel tag={k ? "학습 추이 — 누적 검증 표본" : "learning — validated samples"}>
            <Metric series={dp?.samples ?? []} fmt={fmtN} label={k ? "실제와 대조 완료한 예측 (누적)" : "predictions validated vs actual (cumulative)"} color="#f472b6" higherBetter lang={lang} />
          </Panel>
          <Panel test tag={k ? "최신 테스트 — 예측 vs 실제 작업시각 (근거리 20분내·2일)" : "test — predicted vs actual work time (near 20m · 2d)"}>
            {dp && dp.ds_eval > 0 ? (
              <div className="ls-testchips">
                <Chip label={k ? "양하 ±10분 적중" : "DS within ±10m"} value={dp.ds_within10_pct != null ? `${dp.ds_within10_pct.toFixed(0)}%` : "—"} accent="#34d399" />
                <Chip label={k ? "양하 중앙오차" : "DS median err"} value={dp.ds_med_err_min != null ? `${dp.ds_med_err_min >= 0 ? "+" : ""}${dp.ds_med_err_min.toFixed(1)}${k ? "분" : "m"}` : "—"} accent="#f59e0b" />
                <Chip label={k ? "양하 평가" : "DS evaluated"} value={fmtN(dp.ds_eval)} />
                <Chip label={k ? "적하 중앙오차" : "LD median err"} value={dp.ld_med_err_min != null ? `${dp.ld_med_err_min >= 0 ? "+" : ""}${dp.ld_med_err_min.toFixed(1)}${k ? "분" : "m"}` : "—"} />
              </div>
            ) : <div className="cyc-empty">{k ? "작업 완료 대기 중" : "awaiting worked containers"}</div>}
            <div className="ls-paside">{k ? "예측한 작업시각 vs 실제 작업된 시각. 양하는 정답이 정확, 적하는 ~수분 지연." : "predicted vs actual work time. DS truth exact; LD lagged a few min."}</div>
          </Panel>
        </div>
        <div className="ls-chips">
          <Chip label={k ? "검증된 예측 (누적)" : "validated (total)"} value={dp ? fmtN(dp.resolved_total) : "—"} accent="#34d399" />
          <Chip label={k ? "고유 컨테이너" : "distinct containers"} value={dp ? fmtN(dp.distinct_cont) : "—"} />
        </div>
        <div className="ls-note">{k ? "운영을 바꾸지 않고 예측만 옆에서 기록 → 실제 작업시각과 대조(그림자 검증). 평균은 잘 맞으나 컨테이너 개별 정밀도는 작업순서 데이터 한계로 거침 — 크레인 단위 신호로 유효." : "Shadow validation: log predictions without touching operations, compare to actual work time. Average calibrated; per-container precision rough — use as a crane-level signal."}</div>
      </Session>
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
        name: ["TT 작업 사이클 (6단계)", "TT work cycle (6-event)"],
        source: ["GPS+TOS 결합 추론", "derived from GPS + TOS"],
        usage: ["모든 학습의 백본 · 이동시간/곧빔 정답", "backbone for all learning"],
        desc: ["트럭 한 사이클(빈차 출발→픽업→적재 도착→하차)을 6개 시점으로 기록. 이동시간 학습과 '곧 빔' 정답의 출처.", "One truck cycle as 6 timestamps. Source of travel labels and the soon-idle ground truth."] },
      { key: "learn_travel_sample",
        name: ["이동시간 표본 (OD)", "travel-time samples (OD)"],
        source: ["사이클에서 수확 (5분)", "harvested from cycles (5min)"],
        usage: ["TT 이동시간 모델", "TT travel-time model"],
        desc: ["출발존→도착존 한 leg의 실제 소요시간·거리·피처(밀도·날씨·시간대). 이동시간 예측의 학습 표본.", "Per O→D leg: actual seconds, distance, features (density/weather/hour). Training rows for travel prediction."] },
      { key: "learn_travel_drive_sample",
        name: ["순수 주행 표본", "pure-driving samples"],
        source: ["3초 GPS (정지 제외)", "3s GPS, stop-excluded"],
        usage: ["순수 주행시간 모델", "pure drive-time model"],
        desc: ["큐·정지를 뺀 순수 주행 leg. 이동시간 예측의 상한 정확도 검증.", "Pure-drive leg with queue/stop removed. Upper-bound accuracy check for travel prediction."] },
    ],
  },
  {
    title: ["예측 · 검증 (그림자)", "Predictions · validation (shadow)"], accent: "#a78bfa",
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
        desc: ["크레인이 각 컨테이너를 작업할 시각 예측 + 실제 작업시각 backfill. 운영 미변경 그림자 검증.", "Predicted per-container work time + backfilled actual. Shadow validation without touching ops."] },
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

function DataCard({ meta, stat, accent, lang }: { meta: StreamMeta; stat: DataStat | undefined; accent: string; lang: Lang }) {
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
  return (
    <section className="ds-card" style={{ borderTopColor: accent }}>
      <div className="ds-card-h">
        <div className="ds-card-name">{meta.name[i]}</div>
        <code className="ds-card-tbl">{meta.key}</code>
      </div>
      <div className="ds-card-desc">{meta.desc[i]}</div>
      <div className="ds-card-meta">
        <div className="ds-mrow"><span className="ds-mk">{k ? "소스" : "source"}</span><span>{meta.source[i]}</span></div>
        <div className="ds-mrow"><span className="ds-mk">{k ? "쓰임" : "used by"}</span><span>{meta.usage[i]}</span></div>
      </div>
      <div className="ds-stats">
        {/* reltuples is an estimate; for young/fast tables it can lag the live 24h count → clamp up */}
        <div className="ds-stat"><b style={{ color: accent }}>{fmtN(stat ? Math.max(stat.total, stat.n_24h) : undefined)}</b><span>{k ? "총 수집" : "total"}</span></div>
        <div className="ds-stat"><b>{fmtN(stat?.n_24h)}</b><span>{k ? "최근 24시간" : "24h"}</span></div>
        <div className="ds-stat"><b>{fmtN(stat?.n_1h)}</b><span>{k ? "최근 1시간" : "1h"}</span></div>
        <div className="ds-stat"><b className="ds-stamp">{stamp(stat?.latest)}</b><span>{k ? "최근 수집" : "latest"}</span></div>
      </div>
      <button className={`ds-toggle${open ? " open" : ""}`} onClick={toggle}>
        {open ? (k ? "▲ 닫기" : "▲ close") : (k ? "▼ 최근 수집 데이터 보기" : "▼ view recent rows")}
      </button>
      {open && <SampleTable rows={rows} loading={loading} lang={lang} />}
    </section>
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
  const grand = (cat ?? []).reduce((a, s) => a + Math.max(s.total ?? 0, s.n_24h ?? 0), 0);

  return (
    <>
      <div className="ds-summary">
        {k ? "우리가 실시간으로 수집·축적하는 데이터 스트림. 각 카드에서 소스·쓰임·총/최근 건수를 보고, 펼치면 최근 수집 내역을 직접 확인." : "Data streams we collect and accumulate live. Each card shows source, usage, total/recent counts; expand to inspect the latest rows."}
        {err && <span className="cyc-err">{k ? " · 연결 오류" : " · offline"}</span>}
        {cat && <span className="ds-grand"> · {k ? "전체 누적" : "grand total"} <b>{fmtN(grand)}</b> {k ? "건" : "rows"}</span>}
      </div>
      {DATA_CATALOG.map((c) => (
        <div className="ds-group" key={c.title[0]}>
          <h3 className="ds-group-h" style={{ borderLeftColor: c.accent }}>{k ? c.title[0] : c.title[1]}</h3>
          <div className="ds-grid">
            {c.streams.map((m) => <DataCard key={m.key} meta={m} stat={statOf(m.key)} accent={c.accent} lang={lang} />)}
          </div>
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
            ? (k ? "우리가 쓰는 예측 모델 — 입력·출력·담당역할 + 검증/학습 추이" : "our prediction models — in/out/role + validation & trend")
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
