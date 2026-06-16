// 학습 센터 — 축적되는 학습데이터와 모델 성능·개선 추이.
// ② 블록 작업지점 좌표: TT가 topos 타깃에 ARRIVED한 GPS를 누적 → 좌표.
// ③ 차량 주행 차선: 이동 TT의 GPS 트레이스를 격자에 집계 → 도로·방향.
import { useEffect, useMemo, useState } from "react";
import { type Lang } from "./i18n";
import { api, type LearnTopos, type LearnToposPoint, type LanesData, type TravelData, type TravelOd, type SoonIdleData } from "./api";
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
      <span className="mono">{o.origin}</span>
      <span className="mono">{o.dest}</span>
      <span className="mono">{o.n}</span>
      <span className="mono">{mmss(o.median_s)}</span>
      <span className="mono">{mDist(o.dist_m)}</span>
      <span className="mono">{kmh(o.speed_kmh)}</span>
    </div>
  );
}

export default function LearnPage({ lang }: { lang: Lang }) {
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

  const ms = d?.metric_series ?? [];
  const covVals = ms.map((p) => p.confident_topos);
  const spreadVals = ms.map((p) => p.median_spread_m ?? 0).filter((v) => v > 0);
  const obsVals = ms.map((p) => Number(p.total_obs));
  const lms = ln?.metric_series ?? [];
  const roadVals = lms.map((p) => p.road_cells);
  const passVals = lms.map((p) => Number(p.total_passes));
  const tms = tv?.metric_series ?? [];
  const odVals = tms.map((p) => p.od_pairs);
  const sampVals = tms.map((p) => Number(p.samples));
  // ④ soon-idle accuracy
  const dsRecall = si?.by_jobtype.find((j) => j.jobtype === "DS");
  const dsRecallPct = dsRecall?.recall_pct ?? null;
  const dsDelta = dsRecall && dsRecall.recall_pct != null && dsRecall.recall_gps_pct != null ? dsRecall.recall_pct - dsRecall.recall_gps_pct : null;
  const dsLeadP50 = si?.by_source.find((s) => s.jobtype === "DS" && s.source === "tos_actv")?.lead_p50_s ?? null;
  const siRecallSeries = (si?.metric_series ?? []).filter((p) => p.jobtype === "DS" && p.source === "ALL").map((p) => p.recall_pct ?? 0);
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
          <h2>{ko(lang) ? "학습 센터" : "Learning Center"}</h2>
          <span className="cyc-title-sub">{ko(lang) ? "① TT 이동시간 · ② 블록 좌표 · ③ 주행 차선 · ④ Soon-idle 정확도(그림자)" : "① travel time · ② block coords · ③ lanes · ④ soon-idle accuracy"}{err && <span className="cyc-err">{ko(lang) ? " · 연결 오류" : " · offline"}</span>}</span>
        </div>
      </div>

      <div className="cyc-sec-h" style={{ marginTop: 4 }}>{ko(lang) ? "① TT 이동시간 (출발→도착 · v0 베이스라인=중앙값)" : "① TT travel time (origin→dest · v0 baseline)"}</div>
      <div className="cyc-tiles">
        <Tile label={ko(lang) ? "학습 표본" : "Samples"} value={tv ? fmtN(tv.samples) : "—"} accent="#60a5fa" />
        <Tile label={ko(lang) ? "구간(O→D) 쌍" : "O→D pairs"} value={tv ? fmtN(tv.od_pairs) : "—"} accent="#0ea5e9" />
        <Tile label={ko(lang) ? "확신 쌍 (n≥10)" : "Confident (n≥10)"} value={tv ? fmtN(tv.confident_pairs) : "—"} accent="#34d399" />
        <Tile label={ko(lang) ? "중앙 속도" : "Median speed"} value={tv ? kmh(tv.median_speed_kmh) : "—"} unit="km/h" accent="#f59e0b" />
      </div>
      <div className="learn-charts">
        <div className="cyc-tp">
          <div className="cyc-sec-h">{ko(lang) ? "개선 — 구간 쌍 수 (↑ 커버리지)" : "Improving — O→D pairs (↑)"}</div>
          <div className="cyc-tp-box">{odVals.length > 1 ? <LineChart values={odVals} color="#34d399" axes /> : <div className="cyc-empty">{ko(lang) ? "스냅샷 수집 중" : "collecting"}</div>}</div>
        </div>
        <div className="cyc-tp">
          <div className="cyc-sec-h">{ko(lang) ? "누적 학습 표본" : "Accumulating samples"}</div>
          <div className="cyc-tp-box">{sampVals.length > 1 ? <LineChart values={sampVals} color="#60a5fa" axes /> : <div className="cyc-empty">{ko(lang) ? "수집 중" : "collecting"}</div>}</div>
        </div>
      </div>
      <div className="cyc-board" style={{ marginTop: 4 }}>
        <div className="cyc-board-head"><span>{ko(lang) ? "구간별 이동시간 (표본 많은 순)" : "Travel time by O→D (by samples)"}</span></div>
        <div className="learn-od-cols">
          <span>{ko(lang) ? "출발" : "origin"}</span><span>{ko(lang) ? "도착" : "dest"}</span><span>n</span><span>{ko(lang) ? "중앙시간" : "median"}</span><span>{ko(lang) ? "거리" : "dist"}</span><span>km/h</span>
        </div>
        <div className="learn-list">
          {(tv?.od ?? []).length === 0 && <div className="cyc-empty">{ko(lang) ? "아직 구간 표본 없음 (사이클에서 수확 중)" : "none yet (harvesting from cycles)"}</div>}
          {(tv?.od ?? []).slice(0, 250).map((o) => <OdRow key={o.origin + "→" + o.dest} o={o} />)}
        </div>
      </div>

      <div className="cyc-sec-h" style={{ marginTop: 4 }}>{ko(lang) ? "② 블록 작업지점 좌표" : "② Block work-point coordinates"}</div>
      <div className="cyc-tiles">
        <Tile label={ko(lang) ? "누적 관측" : "Observations"} value={d ? fmtN(d.total_obs) : "—"} accent="#60a5fa" />
        <Tile label={ko(lang) ? "학습된 작업지점" : "Learned points"} value={d ? fmtN(d.distinct_topos) : "—"} accent="#0ea5e9" />
        <Tile label={ko(lang) ? "확신 (n≥30)" : "Confident (n≥30)"} value={d ? fmtN(d.confident_topos) : "—"} accent="#34d399" />
        <Tile label={ko(lang) ? "블록 작업지점" : "Block points"} value={d ? fmtN(d.block_points) : "—"} accent="#a78bfa" />
        <Tile label={ko(lang) ? "중앙 정밀도" : "Median precision"} value={d ? mPrec(d.median_spread_m) : "—"} accent="#f59e0b" />
      </div>
      <div className="learn-charts">
        <div className="cyc-tp">
          <div className="cyc-sec-h">{ko(lang) ? "개선 — 확신 지점 수 (↑)" : "Improving — confident points (↑)"}</div>
          <div className="cyc-tp-box">{covVals.length > 1 ? <LineChart values={covVals} color="#34d399" axes /> : <div className="cyc-empty">{ko(lang) ? "스냅샷 수집 중" : "collecting"}</div>}</div>
        </div>
        <div className="cyc-tp">
          <div className="cyc-sec-h">{ko(lang) ? "개선 — 중앙 정밀도 (↓ m)" : "Improving — precision (↓ m)"}</div>
          <div className="cyc-tp-box">{spreadVals.length > 1 ? <LineChart values={spreadVals} color="#f59e0b" axes /> : <div className="cyc-empty">{ko(lang) ? "수집 중" : "collecting"}</div>}</div>
        </div>
      </div>
      <div className="cyc-tp">
        <div className="cyc-sec-h">{ko(lang) ? "누적 학습데이터 — 관측 수" : "Accumulating data — observations"}</div>
        <div className="cyc-tp-box">{obsVals.length > 1 ? <LineChart values={obsVals} color="#60a5fa" axes /> : <div className="cyc-empty">{ko(lang) ? "수집 중" : "collecting"}</div>}</div>
      </div>
      <div className="cyc-board" style={{ marginTop: 14 }}>
        <div className="cyc-board-head">
          <span>{ko(lang) ? "학습된 작업지점 (관측 많은 순)" : "Learned work-points (by observations)"}</span>
          <span style={{ display: "flex", gap: 10, alignItems: "center" }}>
            <label style={{ fontSize: 11, color: "var(--text-dim)", cursor: "pointer" }}>
              <input type="checkbox" checked={onlyBlock} onChange={(e) => setOnlyBlock(e.target.checked)} /> {ko(lang) ? "블록만" : "blocks only"}
            </label>
            <input className="cyc-search mono" placeholder={ko(lang) ? "topos 검색" : "find topos"} value={q} onChange={(e) => setQ(e.target.value)} />
          </span>
        </div>
        <div className="learn-cols">
          <span>topos</span><span>{ko(lang) ? "종류" : "kind"}</span><span>n</span><span>{ko(lang) ? "누적" : "obs"}</span><span>{ko(lang) ? "정밀도" : "prec"}</span><span>{ko(lang) ? "좌표" : "coord"}</span><span>{ko(lang) ? "갱신" : "updated"}</span>
        </div>
        <div className="learn-list">
          {points.length === 0 && <div className="cyc-empty">{ko(lang) ? "아직 학습된 지점 없음 (적재 중)" : "none yet (accumulating)"}</div>}
          {points.slice(0, 300).map((p) => <PointRow key={p.topos} p={p} lang={lang} />)}
        </div>
      </div>

      <div className="cyc-sec-h" style={{ marginTop: 22 }}>{ko(lang) ? "③ 차량 주행 차선 (GPS 트레이스 → 도로·방향)" : "③ Driving lanes (GPS traces → roads·direction)"}</div>
      <div className="cyc-tiles">
        <Tile label={ko(lang) ? "누적 통과" : "Passes"} value={ln ? fmtN(ln.total_passes) : "—"} accent="#60a5fa" />
        <Tile label={ko(lang) ? "도로 셀 (통과≥20)" : "Road cells (≥20)"} value={ln ? fmtN(ln.road_cells) : "—"} accent="#34d399" />
        <Tile label={ko(lang) ? "전체 셀" : "Total cells"} value={ln ? fmtN(ln.cells) : "—"} accent="#0ea5e9" />
        <Tile label={ko(lang) ? "일방통행 비율" : "One-way frac"} value={ln ? pct(ln.oneway_frac) : "—"} accent="#a78bfa" />
      </div>
      <div className="learn-charts">
        <div className="cyc-tp">
          <div className="cyc-sec-h">{ko(lang) ? "개선 — 도로 셀 수 (↑ 커버리지)" : "Improving — road cells (↑)"}</div>
          <div className="cyc-tp-box">{roadVals.length > 1 ? <LineChart values={roadVals} color="#34d399" axes /> : <div className="cyc-empty">{ko(lang) ? "스냅샷 수집 중" : "collecting"}</div>}</div>
        </div>
        <div className="cyc-tp">
          <div className="cyc-sec-h">{ko(lang) ? "누적 통과 수" : "Accumulating passes"}</div>
          <div className="cyc-tp-box">{passVals.length > 1 ? <LineChart values={passVals} color="#60a5fa" axes /> : <div className="cyc-empty">{ko(lang) ? "수집 중" : "collecting"}</div>}</div>
        </div>
      </div>
      <div style={{ fontSize: 12, color: "var(--text-mute)", margin: "2px 2px 8px" }}>
        {ko(lang) ? "차선망 지도는 라이브맵 → 레이어 → ③ 주행 차선에서 위성 위로 확인하세요." : "See the lane network on the live map → Layers → ③ Driving lanes."}
      </div>

      <div className="cyc-sec-h" style={{ marginTop: 22 }}>{ko(lang) ? "④ Soon-idle 예측 정확도 (그림자 — 예측 vs 실제 유휴 comp_ts)" : "④ Soon-idle prediction accuracy (shadow)"}</div>
      <div className="cyc-tiles">
        <Tile label={ko(lang) ? "예측 (7일)" : "Predictions (7d)"} value={si ? fmtN(si.predictions) : "—"} accent="#60a5fa" />
        <Tile label={ko(lang) ? "DS 재현율" : "DS recall"} value={dsRecallPct != null ? `${dsRecallPct.toFixed(0)}%` : "—"} accent="#34d399" />
        <Tile label={ko(lang) ? "ΔRecall · TOS 기여" : "ΔRecall · TOS"} value={dsDelta != null ? `+${dsDelta.toFixed(0)}%p` : "—"} accent="#a78bfa" />
        <Tile label={ko(lang) ? "DS 리드타임 p50" : "DS lead p50"} value={mmss(dsLeadP50)} accent="#f59e0b" />
      </div>
      <div style={{ fontSize: 12, color: "var(--text-mute)", margin: "2px 2px 8px" }}>
        {ko(lang)
          ? "정답 = tos_handover_label.comp_ts(실제 유휴). 예측이 완료를 지나야 채워짐 — 갓 적재분은 정확도 0(수집 중). DS 예측의 gps_would_fire=false는 GPS단독이면 놓쳤을 건 → ΔRecall이 ② TOS 보정의 순이득."
          : "Ground truth = comp_ts. Accuracy fills once predictions age past completion. ΔRecall = recall the TOS hook added over GPS-alone."}
      </div>
      <div className="learn-list">
        <div style={{ display: "grid", gridTemplateColumns: siGrid, gap: 8, padding: "3px 6px", fontWeight: 600, color: "var(--text-dim)", fontSize: 12 }}>
          <span>{ko(lang) ? "작업" : "job"}</span><span>{ko(lang) ? "신호" : "signal"}</span><span>{ko(lang) ? "예측" : "pred"}</span><span>{ko(lang) ? "적중" : "match"}</span><span>{ko(lang) ? "정밀도" : "prec"}</span><span>{ko(lang) ? "리드p50" : "lead"}</span>
        </div>
        {(si?.by_source ?? []).map((s, i) => (
          <div key={i} style={{ display: "grid", gridTemplateColumns: siGrid, gap: 8, padding: "3px 6px" }}>
            <span className="mono">{s.jobtype}</span>
            <span className="mono" style={{ color: s.source === "tos_actv" ? "#a78bfa" : s.source === "gps_rtg" ? "#34d399" : s.source === "qc_plc" ? "#0ea5e9" : s.source === "both" ? "#22d3ee" : "#94a3b8" }}>{s.source}</span>
            <span className="mono">{s.predictions}</span>
            <span className="mono">{s.matched}</span>
            <span className="mono">{s.precision_pct != null ? `${s.precision_pct.toFixed(0)}%` : "—"}</span>
            <span className="mono">{mmss(s.lead_p50_s)}</span>
          </div>
        ))}
        {(si?.by_source?.length ?? 0) === 0 && <div className="cyc-empty">{ko(lang) ? "예측 수집 중" : "collecting predictions"}</div>}
      </div>
      <div className="learn-charts" style={{ marginTop: 10 }}>
        <div className="cyc-tp">
          <div className="cyc-sec-h">{ko(lang) ? "GPS 단독 vs TOS 보정 — 재현율" : "GPS-only vs TOS-corrected — recall"}</div>
          <div className="cyc-tp-box" style={{ height: "auto", display: "flex", flexDirection: "column", gap: 6, padding: 10 }}>
            {(si?.by_jobtype ?? []).map((j) => (
              <div key={j.jobtype} style={{ fontSize: 13 }}>
                <span className="mono" style={{ fontWeight: 600 }}>{j.jobtype}</span>{" "}
                {ko(lang) ? "정답" : "truth"} {j.truth_idles} · {ko(lang) ? "재현율" : "recall"} <b style={{ color: "#34d399" }}>{j.recall_pct != null ? `${j.recall_pct.toFixed(0)}%` : "—"}</b>{" "}
                ({ko(lang) ? "GPS단독" : "GPS"} {j.recall_gps_pct != null ? `${j.recall_gps_pct.toFixed(0)}%` : "—"} → <b style={{ color: "#a78bfa" }}>TOS +{j.recall_pct != null && j.recall_gps_pct != null ? (j.recall_pct - j.recall_gps_pct).toFixed(0) : "—"}%p</b>)
              </div>
            ))}
            {(si?.by_jobtype?.length ?? 0) === 0 && <div className="cyc-empty">{ko(lang) ? "정답 매칭 대기 (예측이 완료를 지나면 채워짐)" : "awaiting label matches"}</div>}
          </div>
        </div>
        <div className="cyc-tp">
          <div className="cyc-sec-h">{ko(lang) ? "개선 — DS 재현율 (↑)" : "Improving — DS recall (↑)"}</div>
          <div className="cyc-tp-box">{siRecallSeries.length > 1 ? <LineChart values={siRecallSeries} color="#34d399" axes /> : <div className="cyc-empty">{ko(lang) ? "스냅샷 수집 중" : "collecting"}</div>}</div>
        </div>
      </div>
    </div>
  );
}
