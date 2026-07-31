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
const hhmm = (iso: string) => new Date(iso).toLocaleTimeString("en-GB", { timeZone: "Asia/Kuala_Lumpur", hour12: false }); // MYT (terminal local)

function Chip({ label, value, accent }: { label: string; value: string; accent?: string }) {
  return (
    <div className="ls-chip">
      <div className="ls-chip-l">{label}</div>
      <div className="ls-chip-v" style={accent ? { color: accent } : undefined}>{value}</div>
    </div>
  );
}

// one breakdown dimension as diverging bars (green right = optimum beats TOS, red left = we're worse).
// NOTE: this decomposes the improvement CEILING, not a realized saving — see the headline note.
function BdGroup({ title, rows }: { title: string; rows: import("./api").FairBucket[] }) {
  return (
    <div className="ls-card" style={{ padding: 8 }}>
      <div style={{ fontWeight: 600, fontSize: 12, marginBottom: 6 }}>{title}</div>
      {rows.length === 0
        ? <div style={{ fontSize: 11, color: "#888" }}>—</div>
        : rows.map((r) => {
            const sv = r.savings_pct ?? 0;
            const pos = sv >= 0;
            const half = Math.min(Math.abs(sv), 50) / 2; // bar half-width %, capped at ±50%
            return (
              <div key={r.key} title={`여지 ${sv.toFixed(0)}% · ${r.pairs}짝 · 우리가 더 나쁨 ${(r.worse_pct ?? 0).toFixed(0)}%`}
                   style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 3, fontSize: 11 }}>
                <div style={{ width: 86, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{r.key}</div>
                <div style={{ flex: 1, height: 12, background: "#1f2937", borderRadius: 3, position: "relative", overflow: "hidden" }}>
                  <div style={{ position: "absolute", left: pos ? "50%" : `${50 - half}%`, width: `${half}%`, height: "100%", background: pos ? "#34d399" : "#ef4444" }} />
                  <div style={{ position: "absolute", left: "50%", top: 0, bottom: 0, width: 1, background: "#4b5563" }} />
                </div>
                <div style={{ width: 64, textAlign: "right", color: pos ? "#34d399" : "#ef4444" }}>
                  {sv.toFixed(0)}%<span style={{ color: "#9ca3af" }}> ·{r.pairs}</span>
                </div>
              </div>
            );
          })}
    </div>
  );
}

export default function Stage2Page({ lang }: { lang: Lang }) {
  const k = ko(lang);
  const [d, setD] = useState<Stage2Shadow | null>(null);
  const [cmp, setCmp] = useState<DispatchCompare | null>(null);
  const [fair, setFair] = useState<import("./api").FairCompareOut | null>(null);
  const [bd, setBd] = useState<import("./api").FairBreakdown | null>(null);
  const [err, setErr] = useState(false);
  useEffect(() => {
    let alive = true;
    const poll = () => {
      api.stage2Shadow().then((r) => { if (alive) { setD(r); setErr(false); } }).catch(() => alive && setErr(true));
      api.dispatchCompare().then((r) => { if (alive) setCmp(r); }).catch(() => {});
      api.stage2FairCompare().then((r) => { if (alive) setFair(r); }).catch(() => {});
      api.stage2FairBreakdown().then((r) => { if (alive) setBd(r); }).catch(() => {});
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

      {/* ── HEADLINE: the FAIR metric (reservation-respected optimal matching vs TOS) ── */}
      {fair?.latest && (() => {
        const f = fair.latest;
        const sav = fair.avg_savings_pct != null ? Math.round(fair.avg_savings_pct) : Math.round(f.savings_pct);
        return (
          <div className="ls-note" style={{ borderLeft: "3px solid #34d399", background: "rgba(52,211,153,0.08)", padding: "10px 12px", marginBottom: 12 }}>
            <div style={{ fontWeight: 700, fontSize: 13, marginBottom: 3 }}>
              {k ? `⚖️ 개선 여지의 상한 — 빈 차 이동 ${sav}% (같은 풀 최적 재매칭 vs TOS)` : `⚖️ Improvement ceiling: ${sav}% of empty travel (optimal re-match of the same pool vs TOS)`}
            </div>
            {k
              ? `같은 순간 TOS가 배차한 트럭들(${f.n}쌍)을 1:1로 다시 최적 매칭하면 빈 차 이동이 ${sav}% 적습니다. ` +
                `⚠ 이건 절감 실적이 아니라 상한입니다 — TOS의 배정도 가능한 순열 중 하나라 최적해가 그보다 나쁠 수 없고, 따라서 이 값은 정의상 음수가 나오지 않습니다. ` +
                (fair.avg_tos_capture_pct != null
                  ? `무작위로 짝지었을 때 대비 TOS는 이미 개선 여지의 ${Math.round(fair.avg_tos_capture_pct)}%를 잡고 있습니다 — 남은 ${100 - Math.round(fair.avg_tos_capture_pct)}%가 실제로 노려볼 수 있는 몫입니다.`
                  : `무작위 대조군 수집 중(${fair.rand_n}/48) — 이 값이 채워지면 "TOS가 이미 잡은 몫"과 "실제로 남은 몫"을 구분해 보여줍니다.`) +
                ` 같은 트럭 선택 ${Math.round(100 * f.same_n / Math.max(f.n, 1))}%.`
              : `Re-matching TOS's own dispatched trucks (${f.n} pairs) optimally cuts empty travel by ${sav}%. ` +
                `⚠ This is a ceiling, not a realized saving — TOS's assignment is itself a feasible permutation, so the optimum can never be worse and this can never be negative.` +
                (fair.avg_tos_capture_pct != null
                  ? ` Against a random assignment, TOS already captures ${Math.round(fair.avg_tos_capture_pct)}% of the available range.`
                  : ` Random baseline still filling (${fair.rand_n}/48).`)}
          </div>
        );
      })()}

      {/* ── VALUE BREAKDOWN: where the saving comes from + bias check (is the headline trustworthy?) ── */}
      {bd && bd.pairs > 0 && (
        <div style={{ marginBottom: 14 }}>
          <div className="area-divider"><span>{k ? "개선 여지의 출처와 신뢰성 (최근 24h)" : "Where the improvement ceiling comes from (24h)"}</span></div>
          <div className="ls-chips" style={{ marginBottom: 8 }}>
            <Chip label={k ? "표본 (짝)" : "pairs"} value={String(bd.pairs)} accent="#60a5fa" />
            <Chip label={k ? "우리가 더 나쁨" : "we're worse"} value={pct(bd.worse_pct)} accent={(bd.worse_pct ?? 0) > 20 ? "#f59e0b" : "#34d399"} />
            <Chip label={k ? "TOS와 동일 선택" : "same as TOS"} value={pct(bd.same_pct)} accent="#a3a3a3" />
            <Chip label={k ? "짝당 절감 (중앙)" : "median save/pair"} value={bd.median_save_s != null ? mmss(bd.median_save_s) : "—"} accent="#34d399" />
          </div>
          <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit,minmax(250px,1fr))", gap: 10 }}>
            <BdGroup title={k ? "거리대별 — 이득의 출처" : "by distance — source of the win"} rows={bd.by_dist} />
            <BdGroup title={k ? "작업유형별" : "by jobtype"} rows={bd.by_job.map((r) => ({ ...r, key: jobKo(r.key) }))} />
            <BdGroup title={k ? "시간대별 (MYT)" : "by hour (MYT)"} rows={bd.by_hour} />
            <BdGroup title={k ? "크레인별 (상위 표본)" : "by crane (top sample)"} rows={bd.by_crane} />
          </div>
          <div className="ls-note" style={{ marginTop: 8 }}>
            {k
              ? "ⓘ 절감은 거의 전부 원거리에서 나옵니다 — TOS가 트럭을 긴 공차주행 보낸 경우를 우리가 더 가까운 트럭으로 교체. 단거리는 TOS가 이미 좋아 우리가 살짝 나쁠 수 있고, 그게 '우리가 더 나쁨' 비율(꼬리)입니다. 짝별 1:1·같은 순간 유휴 풀로 정직하게 측정. 막대: 초록=우리 절감, 빨강=우리가 더 나쁨(±50% 상한)."
              : "ⓘ savings come almost entirely from far hauls (we replace TOS's long empty drives with a closer truck). On short hauls TOS is already good and we can be marginally worse — that's the 'worse' tail. Honest per-pair 1:1, same-instant pool."}
          </div>
        </div>
      )}

      {/* ── reference: per-work "closest truck" (optimistic — double-books the nearest truck) ── */}
      <div className="area-divider"><span>{k ? "참고 — 각 작업 최근접 (낙관적)" : "Reference — per-work closest (optimistic)"}</span></div>
      <div className="ls-chips" style={{ marginBottom: 10 }}>
        <Chip label={k ? "비교 건수 (24h)" : "compared (24h)"} value={c ? String(c.n) : "—"} accent="#f472b6" />
        <Chip label={k ? "우리가 더 가까움" : "ours closer"} value={pct(c?.ours_faster_pct)} accent="#34d399" />
        <Chip label={k ? "중앙 격차 (우리 빠름)" : "median gap"} value={c?.median_delta_s != null ? `${signMin(c.median_delta_s)}분` : "—"} accent={(c?.median_delta_s ?? 0) >= 0 ? "#34d399" : "#ef4444"} />
        <Chip label={k ? "트럭 선택 불일치" : "different truck"} value={pct(c?.divergence_pct)} accent="#60a5fa" />
      </div>

      {c && c.n > 0 && (
        <div className="ls-note">
          {k
            ? `사유: 우리가 더 가까운 트럭 ${c.ours_closer_n}건 · TOS가 더 가까움 ${c.tos_closer_n}건 · 같은 트럭 ${c.same_n}건. 평균 격차 ${signMin(c.avg_delta_s)}분 / 중앙 ${signMin(c.median_delta_s)}분. ⓘ 도착=트럭의 픽업까지 빈 차 이동. 배차 순간(T1)의 트럭 위치·유휴 풀을 그대로 재현해 같은 작업으로 비교 — 시차 없는 1:1. 큰 격차는 TOS가 안벽 빈 트럭을 먼 야드 픽업 적하에 보낸 실제 경우.`
            : `reasons: ours closer ${c.ours_closer_n} · TOS closer ${c.tos_closer_n} · same truck ${c.same_n}. avg ${signMin(c.avg_delta_s)}min / median ${signMin(c.median_delta_s)}min. ⓘ arrival = empty travel to pickup. reconstructed from the truck pool AT the dispatch instant (T1) — a skew-free 1:1. large gaps = TOS sent a quay-idle truck to a far yard pickup (real).`}
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
        <Chip label={k ? "도로망 라우팅(R)" : "routed (R)"} value={pct(s?.routed_pct)} accent="#60a5fa" />
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
              <td style={{ color: m.cost_tier === "R" ? "#60a5fa" : "var(--text-mute)" }}>{m.cost_tier}</td>
              <td>{m.switched ? "↔" : ""}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {rows.length === 0 && <div className="cyc-empty">{k ? "권고 매칭 없음 (후보 차량/작업 대기 중)" : "no matches yet"}</div>}
    </div>
  );
}
