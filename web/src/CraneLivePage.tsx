// Live crane "digital twin" — animates each quay crane's spreader in near-real-time from the PLC
// telemetry (tpos = trolley position, hpos = hoist/height, is_loaded/lock/land), with TOS context
// (current bay, discharge/load, progress, rate). Pick a QC; the spreader moves as the real crane
// moves (rAF-interpolated between ~1.5s polls). Side view: ship (sea) ← crane → truck (quay).
import { useEffect, useRef, useState } from "react";
import { type Lang } from "./i18n";
import { api, type WorkpoolResponse, type WpQc } from "./api";

const ko = (l: Lang) => l === "ko";
const clamp = (v: number, a: number, b: number) => Math.max(a, Math.min(b, v));

type Plc = {
  is_loaded: boolean; age_s: number; mph?: number; last_move_age_s?: number;
  hpos?: number; tpos?: number; lock?: boolean; land?: boolean; load_t?: number;
};
type Dev = { id: string; cls: string; plc?: Plc };
type Snap = { connected: boolean; as_of: string | null; devices: Dev[] };

function useCranes(ms = 1500): Snap | null {
  const [snap, setSnap] = useState<Snap | null>(null);
  useEffect(() => {
    let alive = true;
    const poll = () => fetch("/api/livemap/positions").then((r) => (r.ok ? r.json() : null)).then((d) => { if (alive && d) setSnap(d); }).catch(() => {});
    poll();
    const id = setInterval(poll, ms);
    return () => { alive = false; clearInterval(id); };
  }, [ms]);
  return snap;
}
function useWp(ms = 15000): WorkpoolResponse | null {
  const [wp, setWp] = useState<WorkpoolResponse | null>(null);
  useEffect(() => {
    let alive = true;
    const poll = () => api.workpool().then((d) => { if (alive) setWp(d); }).catch(() => {});
    poll();
    const id = setInterval(poll, ms);
    return () => { alive = false; clearInterval(id); };
  }, [ms]);
  return wp;
}

// ── telemetry → screen mapping. tpos: sea(0)↔land(100) along the boom; hpos: up(0)↔down(~35).
// Best-guess ranges — easy to flip/scale if the live motion looks mirrored.
const TPOS_MIN = 0, TPOS_MAX = 100;
const HPOS_MIN = 0, HPOS_MAX = 36;
const TROLLEY_X0 = 90, TROLLEY_X1 = 650; // sea end → land end
const BOOM_Y = 64, SPREAD_TOP = 96, SPREAD_BOT = 330;
const txToX = (t: number) => TROLLEY_X0 + (clamp(t, TPOS_MIN, TPOS_MAX) - TPOS_MIN) / (TPOS_MAX - TPOS_MIN) * (TROLLEY_X1 - TROLLEY_X0);
const hpToY = (h: number) => SPREAD_TOP + (clamp(h, HPOS_MIN, HPOS_MAX) - HPOS_MIN) / (HPOS_MAX - HPOS_MIN) * (SPREAD_BOT - SPREAD_TOP);

function CraneStage({ plc, q, lang }: { plc?: Plc; q: WpQc; lang: Lang }) {
  const k = ko(lang);
  // current bay / jobtype = the front of remaining work (first not-yet-discharged move)
  const front = q.moves.find((m) => !(m.jobtype === "DS" && m.actv_ts)) ?? q.moves[0];
  const bay = front?.queuename ?? "—";
  const job = front?.jobtype ?? null; // DS = discharge (ship→truck), LD = load (truck→ship)
  const comp = q.queues.reduce((a, b) => a + b.done, 0);
  const tot = q.queues.reduce((a, b) => a + b.total, 0);
  const pct = tot > 0 ? Math.round((comp / tot) * 100) : 0;
  const loaded = plc?.is_loaded ?? false;
  const idleS = plc?.last_move_age_s ?? null;
  const working = idleS != null && idleS < 150;
  const jobColor = job === "LD" ? "#f59e0b" : "#38bdf8";

  // rAF-interpolated spreader position (smooth between polls)
  const targetX = txToX(plc?.tpos ?? 50);
  const targetY = hpToY(plc?.hpos ?? 4);
  const tgt = useRef({ x: targetX, y: targetY });
  tgt.current.x = targetX; tgt.current.y = targetY;
  const cur = useRef({ x: targetX, y: targetY });
  const trolley = useRef<SVGGElement>(null);
  const cable = useRef<SVGLineElement>(null);
  const spreader = useRef<SVGGElement>(null);
  useEffect(() => {
    let raf = 0;
    const tick = () => {
      const c = cur.current, t = tgt.current;
      c.x += (t.x - c.x) * 0.16;
      c.y += (t.y - c.y) * 0.16;
      trolley.current?.setAttribute("transform", `translate(${c.x.toFixed(1)},0)`);
      spreader.current?.setAttribute("transform", `translate(${c.x.toFixed(1)},${c.y.toFixed(1)})`);
      if (cable.current) { cable.current.setAttribute("x1", c.x.toFixed(1)); cable.current.setAttribute("x2", c.x.toFixed(1)); cable.current.setAttribute("y2", c.y.toFixed(1)); }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, []);

  return (
    <div className="cl-stage">
      <div className="cl-stage-head">
        <span className="cl-qc">{q.qc}</span>
        <span className="cl-vsl">{q.vessels[0] ?? "—"}</span>
        <span className="cl-bay mono">{bay}</span>
        {job && <span className="cl-job" style={{ background: jobColor + "22", color: jobColor, borderColor: jobColor + "55" }}>{job === "LD" ? (k ? "적하 (트럭→배)" : "LOAD (truck→ship)") : (k ? "양하 (배→트럭)" : "DISCH (ship→truck)")}</span>}
        <span className={`cl-state ${working ? "on" : "off"}`}>{working ? (k ? "● 작업중" : "● working") : (k ? "○ 대기" : "○ idle")}</span>
        <span className="cl-mph">⚡ {plc?.mph ?? 0}/h</span>
        <span className="cl-prog">{k ? "진행" : "done"} {pct}% <small>({comp.toLocaleString()}/{tot.toLocaleString()})</small></span>
      </div>
      <svg className="cl-svg" viewBox="0 0 720 400" preserveAspectRatio="xMidYMid meet">
        {/* sea + quay */}
        <rect x="0" y="330" width="430" height="70" fill="#0b2942" />
        <rect x="430" y="330" width="290" height="70" fill="#1a1f29" />
        <line x1="0" y1="330" x2="720" y2="330" stroke="#2b3340" strokeWidth="1.5" />
        {/* ship hull + deck containers (schematic) + hatch line */}
        <path d="M30 250 L360 250 L345 330 L55 330 Z" fill="#16222e" stroke="#2b3a48" strokeWidth="1.5" />
        {Array.from({ length: 7 }).map((_, i) => (
          <rect key={i} x={60 + i * 42} y={228} width={38} height={22} rx={2} fill={i % 2 ? "#243140" : "#2a3a4c"} stroke="#33465a" />
        ))}
        <line x1="60" y1="250" x2="345" y2="250" stroke="#3a4f63" strokeWidth="1" strokeDasharray="4 3" />
        {/* truck on the quay */}
        <g transform="translate(560,300)">
          <rect x="0" y="6" width="78" height="20" rx="3" fill="#2b3340" stroke="#3a4453" />
          <rect x="6" y="-6" width="20" height="14" rx="2" fill="#39414e" />
          <circle cx="18" cy="28" r="5" fill="#11151b" stroke="#444" /><circle cx="64" cy="28" r="5" fill="#11151b" stroke="#444" />
        </g>
        {/* crane structure: legs (gantry on quay rail) + boom over ship and back */}
        <line x1="455" y1="64" x2="455" y2="330" stroke="#46596d" strokeWidth="7" />
        <line x1="610" y1="64" x2="610" y2="330" stroke="#46596d" strokeWidth="7" />
        <line x1="70" y1={BOOM_Y} x2="668" y2={BOOM_Y} stroke="#5b7090" strokeWidth="9" />
        <line x1="455" y1="30" x2="668" y2={BOOM_Y} stroke="#46596d" strokeWidth="4" />
        {/* trolley + cable + spreader (rAF-driven) */}
        <g ref={trolley} transform={`translate(${targetX},0)`}>
          <rect x="-16" y={BOOM_Y - 8} width="32" height="14" rx="2" fill="#7d93a8" />
        </g>
        <line ref={cable} x1={targetX} y1={BOOM_Y + 6} x2={targetX} y2={targetY} stroke="#9fb2c4" strokeWidth="2" />
        <g ref={spreader} transform={`translate(${targetX},${targetY})`}>
          <rect x="-30" y="-4" width="60" height="8" rx="2" fill="#c0ccd8" />
          {loaded && <rect x="-26" y="4" width="52" height="26" rx="2" fill={jobColor} opacity="0.85" stroke="#0008" />}
          {plc?.land && <rect x="-32" y="-6" width="64" height="40" rx="3" fill="none" stroke="#22c55e" strokeWidth="1.5" strokeDasharray="3 2" />}
        </g>
        <text x="78" y="352" className="cl-lbl">{k ? "🌊 배(바다)" : "🌊 ship (sea)"}</text>
        <text x="540" y="352" className="cl-lbl">{k ? "🚚 트럭(안벽)" : "🚚 truck (quay)"}</text>
      </svg>
    </div>
  );
}

export default function CraneLivePage({ lang }: { lang: Lang }) {
  const k = ko(lang);
  const snap = useCranes(1500);
  const wp = useWp(15000);
  const plcById = new Map<string, Plc>();
  for (const d of snap?.devices ?? []) if (d.plc && /^[CMZ]\d/.test(d.id)) plcById.set(d.id, d.plc);
  const qcs = (wp?.qcs ?? [])
    .filter((q) => q.moves.length > 0)
    .sort((a, b) => a.qc.localeCompare(b.qc, undefined, { numeric: true }));
  const [sel, setSel] = useState<string | null>(null);
  const selQc = sel && qcs.some((q) => q.qc === sel) ? sel : qcs[0]?.qc ?? null;
  const q = qcs.find((x) => x.qc === selQc) ?? null;

  return (
    <div className="content crane-live">
      <div className="cl-bar">
        <h2>{k ? "라이브 크레인 작업" : "Live Crane Work"}</h2>
        <span className="cl-sub">{k ? "PLC 텔레메트리(트롤리·스프레더)로 실제 크레인 움직임을 실시간 재현 · TOS 작업 맥락" : "real spreader motion from PLC telemetry + TOS work context"}</span>
        <span className={`cl-conn ${snap?.connected ? "on" : "off"}`}>{snap?.connected ? "● live" : "○ off"}</span>
      </div>
      <div className="cl-selector">
        {qcs.map((x) => {
          const p = plcById.get(x.qc);
          const on = p?.last_move_age_s != null && p.last_move_age_s < 150;
          return (
            <button key={x.qc} className={`cl-chip${x.qc === selQc ? " active" : ""}`} onClick={() => setSel(x.qc)}>
              <span className={`cl-dot ${on ? "on" : "off"}`} />{x.qc}<small>{p?.mph ? `⚡${p.mph}` : ""}</small>
            </button>
          );
        })}
        {qcs.length === 0 && <span className="muted">{k ? "가동 중인 QC 없음" : "no working QC"}</span>}
      </div>
      {q ? <CraneStage plc={plcById.get(q.qc)} q={q} lang={lang} /> : <div className="lvp-empty">{k ? "QC를 선택하세요" : "select a QC"}</div>}
    </div>
  );
}
