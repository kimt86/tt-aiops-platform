// WS DATA HEALTH — monitors the live GPS feed pipeline:
//   GPS source → SSH tunnel → API ingest → in-memory store → /api → dashboard.
// Polls /api/livemap/health (~1.5s). Structure mirrors wp-tt-data-center's data-health
// page: status banner, hero tiles (connection / freshness / throughput), a pipeline
// diagram with per-node status dots, and supporting tiles (ingestion / fleet / quality).
import { useEffect, useRef, useState } from "react";
import { type Lang } from "./i18n";

type Health = {
  color: "green" | "amber" | "red";
  state_word: string;
  cause: string;
  connected: boolean;
  connected_for_s: number | null;
  last_msg_age_s: number | null;
  last_message_at: string | null;
  messages_total: number;
  reconnects: number;
  last_error: string | null;
  uptime_s: number;
  rate_per_min: number;
  sparkline: number[];
  fresh: number;
  stale: number;
  lost: number;
  total_devices: number;
  by_class: Record<string, number>;
  engine_on: number;
  with_job: number;
  avg_accuracy_m: number | null;
  fresh_under_s: number;
  stale_after_s: number;
};

const COL: Record<string, string> = { green: "#22c55e", amber: "#f59e0b", red: "#ef4444" };
const WORD = (c: string, ko: boolean) => (c === "green" ? (ko ? "정상" : "Healthy") : c === "amber" ? (ko ? "주의" : "Warning") : (ko ? "장애" : "Down"));

export default function FeedHealthPage({ lang }: { lang: Lang }) {
  const ko = lang === "ko";
  const [h, setH] = useState<Health | null>(null);
  const [err, setErr] = useState(false);
  const timer = useRef<number | null>(null);

  useEffect(() => {
    let alive = true;
    const poll = async () => {
      try {
        const r = await fetch("/api/livemap/health");
        if (!r.ok) throw new Error(String(r.status));
        const j: Health = await r.json();
        if (alive) { setH(j); setErr(false); }
      } catch { if (alive) setErr(true); }
    };
    poll();
    timer.current = window.setInterval(poll, 1500);
    return () => { alive = false; if (timer.current) clearInterval(timer.current); };
  }, []);

  if (!h) {
    return <div className="fh-root"><div className="fh-loading">{err ? (ko ? "헬스 응답 없음…" : "no health response…") : (ko ? "불러오는 중…" : "loading…")}</div></div>;
  }

  const active = h.fresh + h.stale;
  // pipeline node colors
  const srcOk = h.connected && (h.last_msg_age_s ?? 999) < 60;
  const nodes: { id: string; label: string; detail: string; sub?: string; color: string }[] = [
    { id: "src", label: ko ? "GPS 소스" : "GPS source", detail: "172.21.30.72:9986", sub: srcOk ? "wpt_gps" : (ko ? "무신호" : "no signal"), color: srcOk ? "green" : "red" },
    { id: "tunnel", label: ko ? "SSH 터널" : "SSH tunnel", detail: "azure-wp-poc", sub: h.connected ? (ko ? "경유" : "relayed") : (ko ? "끊김" : "down"), color: h.connected ? "green" : "red" },
    { id: "ingest", label: ko ? "API 수집" : "API ingest", detail: `${h.rate_per_min}/min`, sub: h.connected ? (ko ? "연결됨" : "connected") : (ko ? "재연결 중" : "reconnecting"), color: h.connected ? "green" : "amber" },
    { id: "store", label: ko ? "위치 저장소" : "store", detail: `${active} ${ko ? "대" : "dev"}`, sub: `${h.messages_total.toLocaleString()} msg`, color: active > 0 ? "green" : "amber" },
    { id: "api", label: "/api", detail: ko ? "positions" : "positions", sub: ko ? "서빙" : "serving", color: "green" },
    { id: "ui", label: ko ? "대시보드" : "dashboard", detail: ko ? "라이브 맵" : "live map", sub: ko ? "폴 2.5초" : "poll 2.5s", color: "green" },
  ];

  return (
    <div className="fh-root">
      {/* banner */}
      <div className="fh-banner" style={{ borderColor: COL[h.color], background: `${COL[h.color]}1a` }}>
        <span className="fh-banner-dot" style={{ background: COL[h.color], boxShadow: `0 0 10px ${COL[h.color]}` }} />
        <strong style={{ color: COL[h.color] }}>{WORD(h.color, ko)}</strong>
        <span className="fh-banner-cause">: {h.cause}</span>
        <span className="fh-spacer" />
        {err && <span className="fh-banner-stale">{ko ? "폴 실패(최근값)" : "poll failed (last)"}</span>}
        <span className="fh-banner-up mono">{ko ? "수집 가동" : "ingest up"} {fmtDur(h.uptime_s, ko)}</span>
      </div>

      {/* hero tiles */}
      <div className="fh-hero">
        <div className="fh-tile">
          <div className="fh-tile-h">{ko ? "연결" : "Connection"}</div>
          <div className="fh-big" style={{ color: h.connected ? COL.green : COL.red }}>{h.connected ? (ko ? "연결됨" : "LIVE") : (ko ? "끊김" : "DOWN")}</div>
          <div className="fh-tile-rows">
            <Kv k={ko ? "연결 지속" : "connected for"} v={h.connected_for_s != null ? fmtDur(h.connected_for_s, ko) : "—"} />
            <Kv k={ko ? "재연결" : "reconnects"} v={String(h.reconnects)} />
            <Kv k={ko ? "최근 수신" : "last msg"} v={h.last_msg_age_s != null ? `${h.last_msg_age_s}s ${ko ? "전" : "ago"}` : "—"} mono />
          </div>
        </div>

        <div className="fh-tile">
          <div className="fh-tile-h">{ko ? "신선도" : "Freshness"}</div>
          <div className="fh-fresh">
            <FreshBadge n={h.fresh} color={COL.green} label={`<${h.fresh_under_s}s`} sub={ko ? "신선" : "fresh"} />
            <FreshBadge n={h.stale} color={COL.amber} label={`<${h.stale_after_s}s`} sub={ko ? "지연" : "stale"} />
            <FreshBadge n={h.lost} color={COL.red} label={`>${h.stale_after_s}s`} sub={ko ? "유실" : "lost"} />
          </div>
          <div className="fh-tile-rows">
            <Kv k={ko ? "활성 장비" : "active"} v={`${active}`} />
            <Kv k={ko ? "평균 GPS 정확도" : "avg gps acc"} v={h.avg_accuracy_m != null ? `${h.avg_accuracy_m} m` : "—"} mono />
          </div>
        </div>

        <div className="fh-tile">
          <div className="fh-tile-h">{ko ? "처리량" : "Throughput"}</div>
          <div className="fh-big">{h.rate_per_min.toLocaleString()}<span className="fh-big-unit">/min</span></div>
          <Spark values={h.sparkline} />
          <div className="fh-tile-rows">
            <Kv k={ko ? "누적 메시지" : "total msgs"} v={h.messages_total.toLocaleString()} mono />
          </div>
        </div>
      </div>

      {/* pipeline */}
      <div className="fh-pipe">
        {nodes.map((n, i) => (
          <div key={n.id} className="fh-pipe-wrap">
            <div className="fh-node">
              <div className="fh-node-h"><span className="fh-dot" style={{ background: COL[n.color] }} /><span className="fh-node-label">{n.label}</span></div>
              <div className="fh-node-detail mono">{n.detail}</div>
              {n.sub && <div className="fh-node-sub">{n.sub}</div>}
            </div>
            {i < nodes.length - 1 && <span className="fh-edge">→</span>}
          </div>
        ))}
      </div>

      {/* supporting tiles */}
      <div className="fh-support">
        <div className="fh-tile">
          <div className="fh-tile-h">{ko ? "수집 (Ingestion)" : "Ingestion"}</div>
          <Kv k={ko ? "WS 연결" : "ws"} v={h.connected ? (ko ? "연결됨" : "connected") : (ko ? "점검 필요" : "check")} vColor={h.connected ? COL.green : COL.red} />
          <Kv k={ko ? "재연결 횟수" : "reconnects"} v={String(h.reconnects)} />
          <Kv k={ko ? "최근 수신" : "last msg"} v={h.last_msg_age_s != null ? `${h.last_msg_age_s}s ${ko ? "전" : "ago"}` : "—"} mono />
          <Kv k={ko ? "마지막 오류" : "last error"} v={h.last_error ?? "—"} mono />
        </div>

        <div className="fh-tile">
          <div className="fh-tile-h">{ko ? "함대 구성" : "Fleet"}</div>
          <div className="fh-fleet">
            {Object.entries(h.by_class).sort((a, b) => b[1] - a[1]).map(([cls, n]) => (
              <div key={cls} className="fh-fleet-row">
                <span className="fh-fleet-cls">{clsLabel(cls, ko)}</span>
                <span className="fh-fleet-bar"><span className="fh-fleet-fill" style={{ width: `${pct(n, active)}%` }} /></span>
                <span className="fh-fleet-n mono">{n}</span>
              </div>
            ))}
            {Object.keys(h.by_class).length === 0 && <div className="fh-muted">—</div>}
          </div>
        </div>

        <div className="fh-tile">
          <div className="fh-tile-h">{ko ? "데이터 품질" : "Data quality"}</div>
          <Kv k={ko ? "시동 ON" : "engine on"} v={`${h.engine_on} / ${active}`} />
          <Kv k={ko ? "작업 배정" : "with job"} v={`${h.with_job} / ${active}`} />
          <Kv k={ko ? "평균 GPS 정확도" : "avg gps acc"} v={h.avg_accuracy_m != null ? `${h.avg_accuracy_m} m` : "—"} mono />
          <Kv k={ko ? "최근 수신 시각" : "last at"} v={h.last_message_at ? new Date(h.last_message_at).toLocaleTimeString("en-GB", { timeZone: "Asia/Kuala_Lumpur", hour12: false }) : "—"} mono />
        </div>
      </div>
    </div>
  );
}

function Kv({ k, v, mono, vColor }: { k: string; v: string; mono?: boolean; vColor?: string }) {
  return (
    <div className="fh-kv">
      <span className="fh-kv-k">{k}</span>
      <span className={`fh-kv-v${mono ? " mono" : ""}`} style={vColor ? { color: vColor } : undefined}>{v}</span>
    </div>
  );
}
function FreshBadge({ n, color, label, sub }: { n: number; color: string; label: string; sub: string }) {
  return (
    <div className="fh-fresh-badge">
      <div className="fh-fresh-n" style={{ color }}>{n}</div>
      <div className="fh-fresh-sub">{sub}</div>
      <div className="fh-fresh-lab mono">{label}</div>
    </div>
  );
}
function Spark({ values }: { values: number[] }) {
  const w = 240, hh = 36, max = Math.max(1, ...values);
  const n = values.length;
  const bw = w / n;
  return (
    <svg className="fh-spark" viewBox={`0 0 ${w} ${hh}`} preserveAspectRatio="none">
      {values.map((val, i) => {
        const bh = (val / max) * (hh - 2);
        return <rect key={i} x={i * bw + 0.5} y={hh - bh} width={Math.max(0.5, bw - 1)} height={bh} fill="var(--teal)" opacity={0.55 + 0.45 * (val / max)} />;
      })}
    </svg>
  );
}

const CLS_KO: Record<string, string> = { TT: "야드트럭 TT", RTG: "야드크레인 RTG", C: "안벽크레인 QC", TC: "TC", ES: "ES", M: "M", Z: "Z", RS: "RS", CR: "CR", PPM: "PPM" };
function clsLabel(cls: string, ko: boolean): string { return ko ? (CLS_KO[cls] ?? cls) : cls; }
function pct(n: number, total: number): number { return total > 0 ? Math.round((n / total) * 100) : 0; }
function fmtDur(s: number, ko: boolean): string {
  s = Math.max(0, Math.round(s));
  if (s < 60) return ko ? `${s}초` : `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return ko ? `${m}분` : `${m}m`;
  const hr = Math.floor(m / 60);
  if (hr < 24) return ko ? `${hr}시간 ${m % 60}분` : `${hr}h ${m % 60}m`;
  return ko ? `${Math.floor(hr / 24)}일 ${hr % 24}시간` : `${Math.floor(hr / 24)}d ${hr % 24}h`;
}
