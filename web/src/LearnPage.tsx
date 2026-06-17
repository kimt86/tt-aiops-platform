// 학습 센터 — 4개 학습 모델을 "세션 카드"로, 품질이 시간이 갈수록 좋아지는지(품질 추이)를 주인공으로.
//   ① TT 이동시간  ② 작업지점 좌표  ③ 주행 차선  ④ Soon-idle 예측 정확도
// 각 세션: 핵심 품질지표(현재값) + 추세 배지(개선/안정/악화) + 품질 추이 차트 + 보조 칩 + 해석 + 상세(접이식).
import { useEffect, useMemo, useState } from "react";
import { type Lang } from "./i18n";
import { api, type LearnTopos, type LearnToposPoint, type LanesData, type TravelData, type TravelOd, type SoonIdleData, type SoonIdleLead } from "./api";
import { LineChart } from "./charts";

const ko = (lang: Lang) => lang === "ko";
const fmtN = (n: number) => n.toLocaleString();
const mPrec = (m: number | null | undefined) => (m == null ? "—" : `${m.toFixed(1)}m`);
const pct = (f: number | null | undefined) => (f == null ? "—" : `${Math.round(f * 100)}%`);
const stamp = (iso: string | null | undefined) =>
  iso ? new Date(iso).toLocaleString([], { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false }) : "—";
const mmss = (s: number | null | undefined) => (s == null ? "—" : `${Math.floor(s / 60)}:${String(Math.round(s % 60)).padStart(2, "0")}`);
const mDist = (m: number | null | undefined) => (m == null ? "—" : m >= 1000 ? `${(m / 1000).toFixed(2)}km` : `${Math.round(m)}m`);
const kmh = (v: number | null | undefined) => (v == null ? "—" : `${v.toFixed(1)}`);

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

// hero headline = the LAST value of its own trend series (so the number always matches its chart),
// formatted via `fmt`. Avoids live-summary vs metric-series mismatches.
function Hero({ series, fmt, label, color, higherBetter, lang }: { series: number[]; fmt: (v: number) => string; label: string; color: string; higherBetter: boolean; lang: Lang }) {
  const v = series.filter((x) => x != null && !Number.isNaN(x));
  const last = v.length ? v[v.length - 1] : null;
  return (
    <div className="ls-hero">
      <div className="ls-hero-metric">
        <div className="ls-hero-v">{last != null ? fmt(last) : "—"}</div>
        <div className="ls-hero-l">{label} <TrendBadge series={series} higherBetter={higherBetter} lang={lang} /></div>
      </div>
      <div className="ls-hero-chart">
        {series.length > 1 ? <LineChart values={series} color={color} axes /> : <div className="cyc-empty">{ko(lang) ? "스냅샷 수집 중" : "collecting snapshots"}</div>}
      </div>
    </div>
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
      <div className="ls-lead-v">{p50 != null ? p50.toFixed(1) : "—"}<span className="ls-lead-u">{k ? "분 후 유휴 (중앙)" : "min to idle (median)"}</span></div>
      <div className="ls-lead-track">
        {p10 != null && p90 != null && <div className="ls-lead-iqr" style={{ left: `${clamp(p10)}%`, width: `${Math.max(1, clamp(p90) - clamp(p10))}%`, background: accent + "55" }} />}
        {p50 != null && <div className="ls-lead-med" style={{ left: `${clamp(p50)}%`, background: accent }} />}
      </div>
      <div className="ls-lead-sub">{k ? "범위 p10~p90" : "p10~p90"} {p10 != null ? p10.toFixed(1) : "—"}~{p90 != null ? p90.toFixed(1) : "—"}{k ? "분" : "m"} · {k ? "적중" : "matched"} {lead?.matched ?? 0}{k ? "건" : ""}</div>
      <div className="ls-lead-q">{k ? "재현율" : "recall"} <b style={{ color: "#34d399" }}>{recall != null ? `${recall.toFixed(0)}%` : "—"}</b>{recallGps != null && recall != null ? ` (GPS ${recallGps.toFixed(0)}→+${(recall - recallGps).toFixed(0)}%p)` : ""} · {k ? "정밀도" : "prec"} <b>{precision != null ? `${precision.toFixed(0)}%` : "—"}</b></div>
    </div>
  );
}

export default function LearnPage({ lang }: { lang: Lang }) {
  const k = ko(lang);
  const [d, setD] = useState<LearnTopos | null>(null);
  const [ln, setLn] = useState<LanesData | null>(null);
  const [tv, setTv] = useState<TravelData | null>(null);
  const [si, setSi] = useState<SoonIdleData | null>(null);
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
    };
    load();
    const id = setInterval(load, 30000);
    return () => { alive = false; clearInterval(id); };
  }, []);

  // ① travel — accumulating learning samples (consistent live↔series; coverage proxy)
  const tms = tv?.metric_series ?? [];
  const sampVals = tms.map((p) => Number(p.samples));
  // ② topos — confident learned points (n≥30), growing
  const ms = d?.metric_series ?? [];
  const confTopoVals = ms.map((p) => p.confident_topos);
  // ③ lanes — learned road cells (coverage)
  const lms = ln?.metric_series ?? [];
  const roadVals = lms.map((p) => p.road_cells);
  // ④ soon-idle — quality = DS recall over time
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

  const points = useMemo(() => {
    let pts = d?.points ?? [];
    if (onlyBlock) pts = pts.filter((p) => !p.is_crane);
    if (q) pts = pts.filter((p) => p.topos.toLowerCase().includes(q.toLowerCase()));
    return pts;
  }, [d, onlyBlock, q]);

  return (
    <div className="content cyc-page">
      <div className="cyc-head">
        <div className="cyc-title">
          <h2>{k ? "학습 센터" : "Learning Center"}</h2>
          <span className="cyc-title-sub">{k ? "4개 학습 모델 · 시간이 갈수록 품질이 좋아지는가 (품질 추이 중심)" : "4 learning models · is quality improving over time"}{err && <span className="cyc-err">{k ? " · 연결 오류" : " · offline"}</span>}</span>
        </div>
      </div>

      {/* ① TT 이동시간 */}
      <Session n={1} accent="#60a5fa" title={k ? "TT 이동시간" : "TT travel time"} sub={k ? "사이클에서 수확한 출발→도착 trip + 피처(경로거리·존·밀도·날씨)" : "trips harvested from cycles + features"}>
        <Hero series={sampVals} fmt={fmtN} label={k ? "누적 학습 표본 (커버리지)" : "accumulating samples (coverage)"} color="#60a5fa" higherBetter lang={lang} />
        <div className="ls-chips">
          <Chip label={k ? "신뢰 OD쌍 (n≥10)" : "confident O→D"} value={tv ? fmtN(tv.confident_pairs) : "—"} accent="#34d399" />
          <Chip label={k ? "OD쌍" : "O→D pairs"} value={tv ? fmtN(tv.od_pairs) : "—"} />
          <Chip label={k ? "중앙 속도" : "median speed"} value={tv ? `${kmh(tv.median_speed_kmh)} km/h` : "—"} />
        </div>
        {tv && tv.accuracy.evaluated > 0 && (
          <>
            <div className="cyc-sec-h" style={{ marginTop: 6 }}>{k ? "최신 예측 테스트 — 예측(OD 중앙값) vs 실제 (지난 2일, 신뢰 OD)" : "latest prediction test — OD median vs actual (last 2d)"}</div>
            <div className="ls-chips">
              <Chip label={k ? "중앙 오차율(MAPE)" : "MAPE"} value={tv.accuracy.mape_pct != null ? `${tv.accuracy.mape_pct.toFixed(0)}%` : "—"} accent="#f59e0b" />
              <Chip label={k ? "중앙 절대오차" : "median abs err"} value={tv.accuracy.median_abs_err_s != null ? mmss(tv.accuracy.median_abs_err_s) : "—"} />
              <Chip label={k ? "±30% 적중률" : "within ±30%"} value={tv.accuracy.within_30pct != null ? `${tv.accuracy.within_30pct.toFixed(0)}%` : "—"} accent="#34d399" />
              <Chip label={k ? "평가 trip" : "evaluated"} value={fmtN(tv.accuracy.evaluated)} />
            </div>
          </>
        )}
        <div className="ls-note">{k ? "표본·신뢰 OD쌍이 늘며 커버리지는 개선됨. 단 같은 OD의 시간 변동(±50%)은 야드 확률성에 의한 본질적 천장 — 점예측보다 분포로 사용." : "Coverage grows with samples; but within-OD variance (±50%) is a structural ceiling — use as a distribution."}</div>
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

      {/* ② 작업지점 좌표 */}
      <Session n={2} accent="#0ea5e9" title={k ? "작업지점 좌표" : "Work-point coordinates"} sub={k ? "TT가 작업점에 도착한 GPS를 누적 → 블록·크레인 중심좌표" : "GPS at arrival accumulated → centroid coords"}>
        <Hero series={confTopoVals} fmt={fmtN} label={k ? "확신 작업지점 (n≥30) — 잘 학습된 점 수" : "confident points (n≥30)"} color="#34d399" higherBetter lang={lang} />
        <div className="ls-chips">
          <Chip label={k ? "중앙 정밀도" : "median precision"} value={d ? mPrec(d.median_spread_m) : "—"} accent="#f59e0b" />
          <Chip label={k ? "학습 지점" : "learned points"} value={d ? fmtN(d.distinct_topos) : "—"} />
          <Chip label={k ? "블록 지점" : "block points"} value={d ? fmtN(d.block_points) : "—"} />
          <Chip label={k ? "누적 관측" : "observations"} value={d ? fmtN(d.total_obs) : "—"} />
        </div>
        <div className="ls-note">{k ? "GPS가 쌓일수록 좌표 군집이 좁아져(±m↓) 더 정밀해지고 확신 지점이 늘어납니다. 라이브맵에서 신뢰도 색으로 확인." : "More GPS → tighter clusters (±m↓) + more confident points."}</div>
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

      {/* ③ 주행 차선 */}
      <Session n={3} accent="#34d399" title={k ? "주행 차선" : "Driving lanes"} sub={k ? "이동 TT의 GPS 트레이스를 22m 격자에 집계 → 도로·방향" : "moving-TT traces aggregated to a 22m grid → roads & direction"}>
        <Hero series={roadVals} fmt={fmtN} label={k ? "학습된 도로 셀 (통과≥20) — 커버리지" : "learned road cells (passes≥20)"} color="#34d399" higherBetter lang={lang} />
        <div className="ls-chips">
          <Chip label={k ? "일방통행 비율" : "one-way frac"} value={ln ? pct(ln.oneway_frac) : "—"} accent="#a78bfa" />
          <Chip label={k ? "전체 셀" : "total cells"} value={ln ? fmtN(ln.cells) : "—"} />
          <Chip label={k ? "누적 통과" : "passes"} value={ln ? fmtN(ln.total_passes) : "—"} />
        </div>
        <div className="ls-note">{k ? "트럭 GPS가 쌓일수록 더 많은 도로 셀이 학습되고 방향이 또렷(일방 비율↑)해집니다. 차선망은 라이브맵 → 레이어 → 주행 차선에서 화살표로 확인." : "More GPS → more road cells + clearer directions. See arrows on the live map."}</div>
      </Session>

      {/* ④ Soon-idle 예측 정확도 */}
      <Session n={4} accent="#a78bfa" title={k ? "Soon-idle 예측 정확도" : "Soon-idle prediction"} sub={k ? "그림자: 예측 vs 권위 정답(comp_ts, 실제 유휴 시각)" : "shadow: prediction vs authoritative idle"}>
        <div className="ls-note" style={{ margin: "0 0 9px" }}>{k ? "곧유휴로 예측한 차량이 실제로 몇 분 뒤 유휴가 됐나 — 예측 적중분 기준 (예측 시점 → 실제 유휴까지)" : "of predicted-soon-idle trucks that did go idle — minutes from prediction to actual idle"}</div>
        <div className="ls-leads">
          <LeadCard jt="DS" accent="#fb923c" lead={dsJob.lead} recall={dsJob.recall} recallGps={dsJob.recallGps} precision={dsJob.precision} lang={lang} />
          <LeadCard jt="LD" accent="#22d3ee" lead={ldJob.lead} recall={ldJob.recall} recallGps={ldJob.recallGps} precision={ldJob.precision} lang={lang} />
        </div>
        <div className="ls-chips">
          <Chip label={k ? "예측 (7일)" : "predictions (7d)"} value={si ? fmtN(si.predictions) : "—"} />
          <Chip label={k ? "적중" : "matched"} value={si ? fmtN(si.matched) : "—"} />
          <Chip label={k ? "전체 정밀도" : "precision"} value={si?.precision_pct != null ? `${si.precision_pct.toFixed(0)}%` : "—"} />
        </div>
        <div className="learn-charts" style={{ marginTop: 6 }}>
          <div className="cyc-tp">
            <div className="cyc-sec-h">{k ? "DS 재현율 추이 (24h)" : "DS recall trend"} <TrendBadge series={siRecallSeries} higherBetter lang={lang} /></div>
            <div className="cyc-tp-box">{siRecallSeries.length > 1 ? <LineChart values={siRecallSeries} color="#fb923c" axes /> : <div className="cyc-empty">{k ? "수집 중" : "collecting"}</div>}</div>
          </div>
          <div className="cyc-tp">
            <div className="cyc-sec-h">{k ? "LD 재현율 추이 (24h)" : "LD recall trend"} <TrendBadge series={ldRecallSeries} higherBetter lang={lang} /></div>
            <div className="cyc-tp-box">{ldRecallSeries.length > 1 ? <LineChart values={ldRecallSeries} color="#22d3ee" axes /> : <div className="cyc-empty">{k ? "수집 중" : "collecting"}</div>}</div>
          </div>
        </div>
        <div className="ls-note">{k ? "정답=실제 유휴(comp_ts). 예측이 완료를 지나야 채점됨(갓 적재분 제외). DS는 ACTV 보정, LD는 안벽 QC PLC가 주 신호 — 리드타임 카드에 GPS단독→TOS 순이득(+%p) 병기." : "Ground truth = comp_ts. DS uses the ACTV hook; LD uses quay QC PLC. Lead cards show the GPS→TOS recall gain."}</div>
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
    </div>
  );
}
