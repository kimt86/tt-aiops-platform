// Dispatch vs TOS — the headline: for the SAME work, did our recommended truck differ from the one
// TOS actually dispatched, WHY, and what was the performance gap (arrival time)? Below: the live
// recommendation engine detail (shadow). All shadow — recommendations never drive live dispatch.
import { useEffect, useState } from "react";
import { type Lang } from "./i18n";
import { api, type Stage2Shadow, type DispatchCompare } from "./api";

const ko = (l: Lang) => l === "ko";
const pct = (v: number | null | undefined) => (v == null ? "—" : `${v.toFixed(0)}%`);
const mmss = (s: number | null | undefined) => (s == null ? "—" : `${Math.floor(s / 60)}:${String(Math.round(s % 60)).padStart(2, "0")}`);
const signMin = (s: number | null | undefined) => (s == null ? "—" : `${s >= 0 ? "+" : ""}${(s / 60).toFixed(1)}`);
const hhmm = (iso: string) => new Date(iso).toLocaleTimeString("en-GB", { timeZone: "Asia/Seoul", hour12: false });

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
  const [cmp, setCmp] = useState<DispatchCompare | null>(null);
  const [err, setErr] = useState(false);
  useEffect(() => {
    let alive = true;
    const poll = () => {
      api.stage2Shadow().then((r) => { if (alive) { setD(r); setErr(false); } }).catch(() => alive && setErr(true));
      api.dispatchCompare().then((r) => { if (alive) setCmp(r); }).catch(() => {});
    };
    poll();
    const iv = setInterval(poll, 15000);
    return () => { alive = false; clearInterval(iv); };
  }, []);
  const s = d?.summary;
  const ie = d?.inefficiency;
  const sv = d?.solver;
  const rows = d?.latest ?? [];
  const c = cmp?.summary;
  const jobKo = (j: string | null) => (j === "DS" ? (k ? "양하" : "DS") : j === "LD" ? (k ? "적하" : "LD") : j ?? "");

  return (
    <div className="content cyc-page">
      <div className="cyc-head">
        <div className="cyc-title">
          <h2>{k ? "배차 비교 — 우리 권고 vs TOS 실제" : "Dispatch — ours vs TOS"}</h2>
          <span className="cyc-title-sub">
            {k ? "같은 작업에 우리가 권고한 트럭과 TOS가 실제 보낸 트럭 — 누가 더 가까운가(도착시간). 그림자(운영 무간섭)." : "for the same work: our recommended truck vs TOS's actual — who arrives sooner. Shadow."}
            {err && <span className="cyc-err"> · {k ? "연결 오류" : "offline"}</span>}
          </span>
        </div>
      </div>

      {/* ── MAIN: TOS vs ours ── */}
      <div className="ls-chips" style={{ marginBottom: 10 }}>
        <Chip label={k ? "비교 건수 (24h)" : "compared (24h)"} value={c ? String(c.n) : "—"} accent="#f472b6" />
        <Chip label={k ? "우리가 더 가까움" : "ours closer"} value={pct(c?.ours_faster_pct)} accent="#34d399" />
        <Chip label={k ? "중앙 격차 (우리 빠름)" : "median gap"} value={c?.median_delta_s != null ? `${signMin(c.median_delta_s)}분` : "—"} accent={(c?.median_delta_s ?? 0) >= 0 ? "#34d399" : "#ef4444"} />
        <Chip label={k ? "트럭 선택 불일치" : "different truck"} value={pct(c?.divergence_pct)} accent="#60a5fa" />
      </div>

      {c && c.n > 0 && (
        <div className="ls-note">
          {k
            ? `사유: 우리가 더 가까운 트럭 ${c.ours_closer_n}건 · TOS가 더 가까움 ${c.tos_closer_n}건 · 같은 트럭 ${c.same_n}건. 평균 격차 ${signMin(c.avg_delta_s)}분 / 중앙 ${signMin(c.median_delta_s)}분. ⓘ 도착=트럭의 픽업까지 빈 차 이동. 큰 격차는 TOS가 안벽의 빈 트럭을 야드 픽업이 먼 적하에 보낸 실제 경우(버그 아님) — 우리는 픽업 가까운 트럭 선택. 권고-배차 시점 시차가 있어 방향 지표이며 중앙값이 robust.`
            : `reasons: ours closer ${c.ours_closer_n} · TOS closer ${c.tos_closer_n} · same truck ${c.same_n}. avg ${signMin(c.avg_delta_s)}min / median ${signMin(c.median_delta_s)}min. ⓘ arrival = empty travel to pickup. large gaps = TOS sent a quay-idle truck to a far yard-pickup load (real) — we pick a truck near the pickup. directional (recommendation vs dispatch timing); median is robust.`}
        </div>
      )}

      <table className="hist-table" style={{ marginTop: 8 }}>
        <thead>
          <tr>
            <th>{k ? "시각" : "Time"}</th>
            <th>{k ? "작업 (QC·큐)" : "Work"}</th>
            <th>{k ? "유형" : "Job"}</th>
            <th>{k ? "TOS 트럭 (도착)" : "TOS truck (arr)"}</th>
            <th>{k ? "우리 트럭 (도착)" : "Ours (arr)"}</th>
            <th>{k ? "격차" : "Gap"}</th>
            <th>{k ? "사유" : "Why"}</th>
          </tr>
        </thead>
        <tbody>
          {(cmp?.recent ?? []).map((r, i) => (
            <tr key={i}>
              <td className="mono">{hhmm(r.ts)}</td>
              <td className="mono">{r.qc} <span style={{ color: "var(--text-mute)" }}>{r.queuename}</span></td>
              <td>{jobKo(r.jobtype)}</td>
              <td className="mono">{r.tos_ytno} <span style={{ color: "var(--text-mute)" }}>{mmss(r.tos_arrival_s)}</span></td>
              <td className="mono">{r.our_ytno} <span style={{ color: "var(--text-mute)" }}>{mmss(r.our_arrival_s)}</span></td>
              <td className="mono" style={{ color: (r.delta_s ?? 0) > 0 ? "#34d399" : "#ef4444" }}>{signMin(r.delta_s)}{k ? "분" : "m"}</td>
              <td style={{ color: r.reason === "ours_closer" ? "#34d399" : "#f59e0b" }}>{r.reason === "ours_closer" ? (k ? "우리가 가까움" : "ours closer") : r.reason === "tos_closer" ? (k ? "TOS가 가까움" : "TOS closer") : r.reason}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {(cmp?.recent ?? []).length === 0 && <div className="cyc-empty">{k ? "비교 사례 누적 중 (작업이 배차될 때마다 기록)" : "accumulating — logged as works get dispatched"}</div>}

      {/* ── secondary: live recommendation engine detail ── */}
      <div className="area-divider" style={{ marginTop: 22 }}><span>{k ? "권고 엔진 상세 (그림자 매처)" : "Recommendation engine (shadow)"}</span></div>

      <div className="ls-chips" style={{ marginBottom: 12 }}>
        <Chip label={k ? "현재 권고 매칭" : "current matches"} value={String(rows.length)} accent="#f472b6" />
        <Chip label={k ? "thrash (재배정율)" : "thrash"} value={pct(s?.switched_pct)} accent={(s?.switched_pct ?? 0) < 10 ? "#34d399" : "#f59e0b"} />
        <Chip label={k ? "마감 충족" : "feasible"} value={pct(s?.feasible_pct)} />
        <Chip label={k ? "중앙 도착" : "median arrival"} value={s?.median_arrival_s != null ? `${(s.median_arrival_s / 60).toFixed(1)}${k ? "분" : "m"}` : "—"} />
        <Chip label={k ? "OD 정확층(L2)" : "OD L2"} value={pct(s?.l2_pct)} accent="#60a5fa" />
        <Chip label={k ? "30분 매칭" : "30m matches"} value={s ? s.matches_30m.toLocaleString() : "—"} />
      </div>

      {ie && ie.starve_ticks > 0 && (
        <div className="ls-card" style={{ borderTopColor: "#f59e0b", padding: "12px 16px", marginBottom: 12 }}>
          <div style={{ fontWeight: 700, marginBottom: 8 }}>{k ? "🎯 근방 유휴 비효율 (최근 30분)" : "🎯 Wasted nearby idle (30m)"}</div>
          <div className="ls-chips">
            <Chip label={k ? "크레인 멈춤 (QC·분)" : "crane stuck (QC·min)"} value={(ie.starve_ticks * 0.5).toFixed(0)} accent="#ef4444" />
            <Chip label={k ? "근방에 빈 트럭 있었음" : "free truck nearby"} value={pct(ie.with_free_pct)} accent="#f59e0b" />
            <Chip label={k ? "평균 근방 유휴트럭" : "avg free nearby"} value={ie.avg_free != null ? `${ie.avg_free.toFixed(1)}${k ? "대" : ""}` : "—"} />
            <Chip label={k ? "영향 QC" : "QCs"} value={String(ie.qcs)} />
          </div>
          <div className="ls-note" style={{ marginTop: 8 }}>
            {k
              ? "크레인이 트럭이 없어 멈춰 있던 시간 중, 근방(~600m)에 빈 트럭이 있었던 비율입니다. 트럭이 부족해서가 아니라 가까운 빈 트럭을 안 보낸 것 — 최적 매칭이 줄일 수 있는 비효율입니다."
              : "Of the time a crane sat stuck waiting for a truck, how often a free truck was within ~600m — a dispatch gap optimal matching would close."}
          </div>
        </div>
      )}

      {sv && sv.ticks > 0 && (
        <div className="ls-card" style={{ borderTopColor: "#a78bfa", padding: "12px 16px", marginBottom: 12 }}>
          <div style={{ fontWeight: 700, marginBottom: 8 }}>{k ? "⚙️ 최적 솔버 vs 단순 배정 (최근 30분)" : "⚙️ Optimal solver vs greedy (30m)"}</div>
          <div className="ls-chips">
            <Chip label={k ? "총 도착시간 절감" : "arrival saved"} value={sv.savings_pct != null ? `${sv.savings_pct.toFixed(0)}%` : "—"} accent="#34d399" />
            <Chip label={k ? "마감 누락 (단순→최적)" : "misses (greedy→opt)"} value={`${sv.greedy_miss ?? "—"} → ${sv.optimal_miss ?? "—"}`} accent="#a78bfa" />
            <Chip label={k ? "비교 틱" : "ticks"} value={String(sv.ticks)} />
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
              <td>{jobKo(m.jobtype)}</td>
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
