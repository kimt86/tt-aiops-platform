// Vehicle detail panel shown when a truck is clicked on the live map — mirrors
// wp-tt-data-center's LiveVehicleDetail (bottom-right fixed aside, sectioned rows),
// trimmed to the fields our live GPS feed actually carries (job/vessel/route/fuel/...).
import { type Lang } from "./i18n";

// the live device object served by /api/livemap/positions
export type SelVeh = {
  id: string;
  cls: string;
  lat: number;
  lon: number;
  speed: number;
  engine: number;
  age_s: number;
  jobtype?: string;
  vslname?: string;
  container1?: string;
  container2?: string;
  cur_loc?: string;
  topos1?: string;
  arrival?: string;
  dispatch?: "idle" | "staging" | "empty_travel" | "delivering" | "soon_idle" | "approaching" | "wait_rtg";
  dispatch_reason?: string;
  nearest_rtg_m?: number;
  fuel?: number;
  accuracy?: number;
  userid?: string;
  batt?: string;
  nett?: string;
  dtime?: string;
  distance?: number;
  plc?: {
    is_loaded: boolean;
    load_t?: number;
    lock?: boolean | null;
    land?: boolean | null;
    hpos?: number;
    tpos?: number;
    age_s: number;
  };
};

// dispatch state → label + colors (matches the live-map rings)
const DSP: Record<string, { ko: string; en: string; color: string; bg: string }> = {
  idle: { ko: "유휴 (배차 가능)", en: "Idle (available)", color: "#16a34a", bg: "rgba(34,197,94,0.12)" },
  staging: { ko: "배차됨 · 대기", en: "Assigned · staging", color: "#0284c7", bg: "rgba(14,165,233,0.12)" },
  soon_idle: { ko: "곧유휴 · 임박", en: "Imminent", color: "#d97706", bg: "rgba(245,158,11,0.14)" },
  approaching: { ko: "접근 · RTG 활성", en: "Approaching", color: "#ca8a04", bg: "rgba(252,211,77,0.16)" },
  wait_rtg: { ko: "도착 · RTG 대기", en: "Arrived · waiting RTG", color: "#dc2626", bg: "rgba(239,68,68,0.12)" },
  delivering: { ko: "적재 이동 중", en: "Delivering", color: "#475569", bg: "rgba(100,116,139,0.12)" },
  empty_travel: { ko: "공차 주행 중", en: "Empty traveling", color: "#475569", bg: "rgba(100,116,139,0.12)" },
};

// localized dispatch detail, built from structured fields (not the backend's Korean
// dispatch_reason). The state label already conveys the gist; this adds RTG distance.
function dispatchWhy(v: SelVeh, ko: boolean): string | null {
  if (v.dispatch === "approaching") {
    return ko ? "RTG 활성 (큐 진입 · ~12분, 물리 집기 아님)" : "RTG active (queued ~12m, not the lift)";
  }
  if (v.dispatch === "soon_idle") {
    if (v.nearest_rtg_m != null) {
      const m = Math.round(v.nearest_rtg_m);
      return ko ? `블록 RTG 근접 ${m}m` : `block RTG ${m}m`;
    }
    return ko ? "안벽 핸드오버 · PLC" : "quay handover · PLC";
  }
  if (v.dispatch === "wait_rtg") {
    if (v.nearest_rtg_m != null) {
      const m = Math.round(v.nearest_rtg_m);
      return ko ? `RTG 대기 (최근접 ${m}m)` : `waiting RTG (nearest ${m}m)`;
    }
    return ko ? "RTG 미관측" : "no RTG nearby";
  }
  return null; // idle/empty_travel/delivering — the state label is enough
}

const EQUIP_KO: Record<string, string> = { TT: "야드트럭", RTG: "야드크레인", C: "안벽크레인", TC: "트랜스퍼크레인" };
function equipLabel(cls: string, ko: boolean): string {
  return ko ? (EQUIP_KO[cls] ?? cls) : cls;
}

type St = "moving" | "idle" | "off";
function stateOf(spd: number, eng: number): St {
  if (spd > 0) return "moving";
  if (eng === 1) return "idle";
  return "off";
}
const ST_COLOR: Record<St, string> = { moving: "#22c55e", idle: "#f59e0b", off: "#64748b" };
const ST_TXT: Record<St, { ko: string; en: string }> = {
  moving: { ko: "이동 중", en: "Moving" },
  idle: { ko: "대기 (시동 ON)", en: "Idle (engine on)" },
  off: { ko: "정지", en: "Stopped" },
};
// jobtype kept as the raw DS/LD/MO/MI code (matches the reference decision), with a hint.
const JOB_HINT: Record<string, { ko: string; en: string }> = {
  DS: { ko: "양하", en: "Discharge" },
  LD: { ko: "적하", en: "Load" },
  MO: { ko: "이적(out)", en: "Move out" },
  MI: { ko: "이적(in)", en: "Move in" },
};

export function LiveVehicleDetail({ v, lang, onClose }: { v: SelVeh; lang: Lang; onClose: () => void }) {
  const ko = lang === "ko";
  const st = stateOf(v.speed, v.engine);
  const hasWork = v.jobtype || v.vslname || v.container1 || v.container2 || v.cur_loc || v.topos1;
  const dsp = v.dispatch ? DSP[v.dispatch] : null;

  return (
    <aside className="lvd-root">
      <header className="lvd-header">
        <div className="lvd-id-row">
          <span className="lvd-id mono">{v.id}</span>
          <span className="lvd-eq">{equipLabel(v.cls, ko)}</span>
          <button className="lvd-close" onClick={onClose} aria-label={ko ? "닫기" : "Close"} title={ko ? "닫기" : "Close"}>×</button>
        </div>
        <div className="lvd-state" style={{ color: ST_COLOR[st] }}>● {ko ? ST_TXT[st].ko : ST_TXT[st].en}</div>
        {dsp && (
          <div className="lvd-dispatch" style={{ color: dsp.color, borderColor: dsp.color, background: dsp.bg }}>
            {ko ? dsp.ko : dsp.en}{dispatchWhy(v, ko) ? <span className="lvd-dsp-why"> · {dispatchWhy(v, ko)}</span> : null}
          </div>
        )}
        <div className="lvd-rel mono">{relTime(v.age_s, ko)}</div>
      </header>

      {hasWork && (
        <Section title={ko ? "작업" : "Work"}>
          {v.jobtype && (
            <Row label="jobtype">
              <span className="lvd-jobtype">{v.jobtype}</span>
              {JOB_HINT[v.jobtype] && <span className="lvd-tag">{ko ? JOB_HINT[v.jobtype].ko : JOB_HINT[v.jobtype].en}</span>}
            </Row>
          )}
          {v.vslname && <Row label={ko ? "선박" : "vessel"}><span className="lvd-mono">{v.vslname}</span></Row>}
          {(v.container1 || v.container2) && (
            <Row label={ko ? "적재" : "load"}>
              {v.container1 && <span className="lvd-mono">{v.container1}</span>}
              {v.container2 && (<><span className="lvd-arrow"> + </span><span className="lvd-mono">{v.container2}</span></>)}
            </Row>
          )}
          {(v.cur_loc || v.topos1) && (
            <Row label={ko ? "경로" : "route"}>
              <span className="lvd-flow mono">
                <span>{v.cur_loc ?? "—"}</span>
                <span className="lvd-arrow"> → </span>
                <span>{v.topos1 ?? "—"}</span>
              </span>
            </Row>
          )}
        </Section>
      )}

      <Section title={ko ? "차량 상태" : "Vehicle"}>
        <Row label={ko ? "속도" : "speed"}><span className="lvd-mono">{Math.round(v.speed)} km/h</span></Row>
        {v.fuel != null && <Row label={ko ? "연료" : "fuel"}><FuelBar pct={v.fuel} /></Row>}
        {v.accuracy != null && <Row label={ko ? "GPS 정확도" : "gps acc"}><span className="lvd-mono">{v.accuracy.toFixed(0)} m</span></Row>}
        {v.batt && <Row label={ko ? "배터리" : "battery"}><span className="lvd-mono">{v.batt}</span></Row>}
        {v.nett && <Row label={ko ? "통신" : "network"}><span className="lvd-mono">{v.nett}</span></Row>}
        {v.userid && <Row label={ko ? "운전자" : "driver"}><span className="lvd-mono">{v.userid}</span></Row>}
      </Section>

      {v.plc && (
        <Section title={ko ? "PLC 작업 상태 (ctab)" : "PLC state (ctab)"}>
          <Row label={ko ? "적재" : "load"}>
            <span className={`lvd-tag ${v.plc.is_loaded ? "lvd-tag-loaded" : "lvd-tag-empty"}`}>
              {v.plc.is_loaded ? (ko ? "컨테이너 적재" : "loaded") : (ko ? "빈 후크" : "empty hook")}
            </span>
            {v.plc.load_t != null && <span className="lvd-muted"> · {v.plc.load_t.toFixed(1)} t</span>}
          </Row>
          {v.plc.lock != null && (
            <Row label={ko ? "후크 잠금" : "twistlock"}>
              <span className={v.plc.lock ? "lvd-tag lvd-tag-on" : "lvd-tag"}>{v.plc.lock ? (ko ? "잠김" : "locked") : (ko ? "해제" : "unlocked")}</span>
            </Row>
          )}
          {v.plc.land != null && (
            <Row label={ko ? "안착" : "landed"}>
              <span className={v.plc.land ? "lvd-tag lvd-tag-on" : "lvd-tag"}>{v.plc.land ? (ko ? "안착" : "landed") : (ko ? "해제" : "clear")}</span>
            </Row>
          )}
          {(v.plc.hpos != null || v.plc.tpos != null) && (
            <Row label={ko ? "호이스트/트롤리" : "hoist/trolley"}>
              <span className="lvd-mono">{v.plc.hpos?.toFixed(1) ?? "—"} / {v.plc.tpos?.toFixed(1) ?? "—"}</span>
            </Row>
          )}
          <Row label={ko ? "PLC 신선도" : "plc age"}>
            <span className={`lvd-mono${v.plc.age_s > 30 ? " lvd-stale" : ""}`}>{v.plc.age_s}{ko ? "초 전" : "s ago"}</span>
          </Row>
        </Section>
      )}

      <Section title={ko ? "위치" : "Position"}>
        <Row label="lat/lon"><span className="lvd-mono">{v.lat.toFixed(5)}, {v.lon.toFixed(5)}</span></Row>
        {v.dtime && <Row label={ko ? "기기시각" : "dev time"}><span className="lvd-mono">{v.dtime}</span></Row>}
        {v.distance != null && v.distance > 0 && <Row label={ko ? "이동거리" : "trip"}><span className="lvd-mono">{fmtDist(v.distance)}</span></Row>}
      </Section>
    </aside>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="lvd-section">
      <header className="lvd-section-h">{title}</header>
      <div className="lvd-section-body">{children}</div>
    </section>
  );
}
function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="lvd-row">
      <span className="lvd-label">{label}</span>
      <span className="lvd-value">{children}</span>
    </div>
  );
}

function FuelBar({ pct }: { pct: number }) {
  const clamped = Math.max(0, Math.min(100, pct));
  const color = pct < 25 ? "#ef4444" : pct < 50 ? "#f59e0b" : "#22c55e";
  return (
    <span className="lvd-fuel">
      <span className="lvd-fuel-track"><span className="lvd-fuel-fill" style={{ width: `${clamped}%`, background: color }} /></span>
      <span className="lvd-fuel-pct" style={{ color }}>{pct.toFixed(0)}%</span>
    </span>
  );
}

function relTime(s: number, ko: boolean): string {
  s = Math.max(0, Math.round(s));
  if (s < 60) return ko ? `${s}초 전 수신` : `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return ko ? `${m}분 전 수신` : `${m}m ago`;
  return ko ? `${Math.floor(m / 60)}시간 전 수신` : `${Math.floor(m / 60)}h ago`;
}
function fmtDist(m: number): string {
  return m < 1000 ? `${m.toFixed(0)} m` : `${(m / 1000).toFixed(2)} km`;
}
