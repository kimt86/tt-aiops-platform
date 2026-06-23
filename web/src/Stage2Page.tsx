// 2단계 매칭 — 그림자 검증 탭. 어느 트럭을 어느 작업에 보낼지 추천(운영 무간섭)하고,
// 최근 30분 요약 지표 + 현재 틱 권고 매칭을 라이브로 보여준다.
import { useEffect, useState } from "react";
import { type Lang } from "./i18n";
import { api, type Stage2Shadow } from "./api";

const ko = (l: Lang) => l === "ko";
const pct = (v: number | null | undefined) => (v == null ? "—" : `${v.toFixed(0)}%`);
const mmss = (s: number | null | undefined) => (s == null ? "—" : `${Math.floor(s / 60)}:${String(Math.round(s % 60)).padStart(2, "0")}`);
const signMin = (s: number | null | undefined) => (s == null ? "—" : `${s >= 0 ? "+" : ""}${(s / 60).toFixed(1)}`);

function Chip({ label, value, accent }: { label: string; value: string; accent?: string }) {
  return (
    <div className="ls-chip">
      <div className="ls-chip-l">{label}</div>
      <div className="ls-chip-v" style={accent ? { color: accent } : undefined}>{value}</div>
    </div>
  );
}

export default function Stage2Page({ lang }: { lang: Lang }) {
  const k = ko(lang);
  const [d, setD] = useState<Stage2Shadow | null>(null);
  const [err, setErr] = useState(false);
  useEffect(() => {
    let alive = true;
    const poll = () => api.stage2Shadow().then((r) => { if (alive) { setD(r); setErr(false); } }).catch(() => alive && setErr(true));
    poll();
    const iv = setInterval(poll, 15000);
    return () => { alive = false; clearInterval(iv); };
  }, []);
  const s = d?.summary;
  const ie = d?.inefficiency;
  const rows = d?.latest ?? [];
  return (
    <div className="content cyc-page">
      <div className="cyc-head">
        <div className="cyc-title">
          <h2>{k ? "2단계 매칭 — 그림자 검증" : "Stage-2 Matching — Shadow"}</h2>
          <span className="cyc-title-sub">
            {k ? "어느 트럭을 어느 작업에 보낼지 추천 · 최근 30분 요약 + 현재 권고 (15초 갱신)" : "recommended vehicle→work matches · 30-min summary + current (15s)"}
            {err && <span className="cyc-err"> · {k ? "연결 오류" : "offline"}</span>}
          </span>
        </div>
      </div>

      <div className="ls-chips" style={{ marginBottom: 12 }}>
        <Chip label={k ? "현재 권고 매칭" : "current matches"} value={String(rows.length)} accent="#f472b6" />
        <Chip label={k ? "thrash (재배정율)" : "thrash"} value={pct(s?.switched_pct)} accent={(s?.switched_pct ?? 0) < 10 ? "#34d399" : "#f59e0b"} />
        <Chip label={k ? "마감 충족" : "feasible"} value={pct(s?.feasible_pct)} />
        <Chip label={k ? "중앙 도착" : "median arrival"} value={s?.median_arrival_s != null ? `${(s.median_arrival_s / 60).toFixed(1)}${k ? "분" : "m"}` : "—"} />
        <Chip label={k ? "OD 정확층(L2)" : "OD L2"} value={pct(s?.l2_pct)} accent="#60a5fa" />
        <Chip label={k ? "30분 매칭" : "30m matches"} value={s ? s.matches_30m.toLocaleString() : "—"} />
      </div>

      <div className="ls-note">
        {k
          ? "그림자 모드 — 실제 배차는 바꾸지 않고 권고만 기록·검증합니다. thrash = 직전 틱 대비 작업이 바뀐 차량 비율(낮을수록 안정; 전환 페널티로 억제). 마감 충족은 현재 작업-ETA 입력 한계로 보수적(로깅 전용, 매칭 구동엔 무관)."
          : "Shadow only — recommendations logged, live dispatch untouched. thrash = vehicles whose work changed vs last tick (lower=stable). feasibility conservative (work-ETA input limits)."}
      </div>

      {ie && ie.starve_ticks > 0 && (
        <div className="ls-card" style={{ borderTopColor: "#f59e0b", padding: "12px 16px", marginBottom: 12 }}>
          <div style={{ fontWeight: 700, marginBottom: 8 }}>{k ? "🎯 권고 vs TOS — 근방 유휴 비효율 (최근 30분)" : "🎯 Stage-2 vs TOS — wasted nearby idle (30m)"}</div>
          <div className="ls-chips">
            <Chip label={k ? "크레인 멈춤 (QC·분)" : "crane stuck (QC·min)"} value={(ie.starve_ticks * 0.5).toFixed(0)} accent="#ef4444" />
            <Chip label={k ? "근방에 빈 트럭 있었음" : "free truck nearby"} value={pct(ie.with_free_pct)} accent="#f59e0b" />
            <Chip label={k ? "평균 근방 유휴트럭" : "avg free nearby"} value={ie.avg_free != null ? `${ie.avg_free.toFixed(1)}${k ? "대" : ""}` : "—"} />
            <Chip label={k ? "영향 QC" : "QCs"} value={String(ie.qcs)} />
          </div>
          <div className="ls-note" style={{ marginTop: 8 }}>
            {k
              ? "크레인이 트럭이 없어 멈춰 있던 시간 중, 근방(~600m)에 빈 트럭이 있었던 비율입니다. 트럭이 부족해서가 아니라 가까운 빈 트럭을 안 보낸 것 — Stage-2 최적 매칭이 줄일 수 있는 비효율입니다."
              : "Of the time a crane sat stuck waiting for a truck, how often a free truck was within ~600m — not a shortage but a dispatch gap Stage-2 would close."}
          </div>
        </div>
      )}

      <table className="hist-table" style={{ marginTop: 12 }}>
        <thead>
          <tr>
            <th>{k ? "차량" : "vehicle"}</th>
            <th>{k ? "상태" : "state"}</th>
            <th>{k ? "작업 (QC·큐)" : "work (QC·queue)"}</th>
            <th>{k ? "유형" : "job"}</th>
            <th>{k ? "픽업블록" : "pickup"}</th>
            <th>{k ? "도착" : "arrival"}</th>
            <th>{k ? "마감여유(분)" : "slack(min)"}</th>
            <th>OD</th>
            <th>{k ? "전환" : "switch"}</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((m) => (
            <tr key={m.ytno}>
              <td className="mono">{m.ytno}</td>
              <td>{m.veh_state}</td>
              <td className="mono">{m.qc} <span style={{ color: "var(--text-mute)" }}>{m.queuename}</span></td>
              <td>{m.jobtype}</td>
              <td className="mono">{m.src_block ?? "—"}</td>
              <td className="mono">{mmss(m.arrival_s)}</td>
              <td className="mono" style={{ color: (m.deadline_slack_s ?? 0) < 0 ? "#ef4444" : "#34d399" }}>{signMin(m.deadline_slack_s)}</td>
              <td style={{ color: m.cost_tier === "L2" ? "#60a5fa" : "var(--text-mute)" }}>{m.cost_tier}</td>
              <td>{m.switched ? "↔" : ""}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {rows.length === 0 && <div className="cyc-empty">{k ? "권고 매칭 없음 (후보 차량/작업 대기 중)" : "no matches yet"}</div>}
    </div>
  );
}
