// VS TOS — 상자 단위 시스템 비교가 헤드라인이다 (2026-08-10 재작성).
// 같은 상자(contno)에 대해 우리 추천(트럭·시점)과 TOS 실배차(트럭·시점)를 직접 짝짓는다.
// 시점 격차는 정오 판정이 아니라 계기 — 우리 마감은 출항 요구 페이스 규범(pool_mode=3)이라
// TOS 배차시각과 다르다는 것 자체는 틀림이 아니다. 그 아래: TOS 매칭의 재배열 여지(상한),
// 시점 실측, 권고 엔진 상세. 전부 그림자(운영 무간섭).
import { useEffect, useState } from "react";
import { type Lang } from "./i18n";
import { api, type Stage2Shadow, type BoxCompare } from "./api";
import { mytTime } from "./timefmt";

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

// one breakdown dimension as diverging bars (green right = optimum beats TOS, red left = we're worse).
// NOTE: this decomposes the improvement CEILING, not a realized saving — see the section note.
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
  const [box, setBox] = useState<BoxCompare | null>(null);
  const [fair, setFair] = useState<import("./api").FairCompareOut | null>(null);
  const [bd, setBd] = useState<import("./api").FairBreakdown | null>(null);
  const [err, setErr] = useState(false);
  useEffect(() => {
    let alive = true;
    const poll = () => {
      api.stage2Shadow().then((r) => { if (alive) { setD(r); setErr(false); } }).catch(() => alive && setErr(true));
      api.stage2BoxCompare().then((r) => { if (alive) setBox(r); }).catch(() => {});
      api.stage2FairCompare().then((r) => { if (alive) setFair(r); }).catch(() => {});
      api.stage2FairBreakdown().then((r) => { if (alive) setBd(r); }).catch(() => {});
    };
    poll();
    const iv = setInterval(poll, 15000);
    return () => { alive = false; clearInterval(iv); };
  }, []);
  // 신선도(P2): 매처 마지막 틱(latest_ts)이 150초를 넘으면 낡은 매칭이다 — 회색 처리로
  // "라이브처럼 보이는 죽은 표"를 막는다(TtPage wpStale 패턴 이식).
  const [nowMs, setNowMs] = useState(Date.now());
  useEffect(() => { const iv = setInterval(() => setNowMs(Date.now()), 5000); return () => clearInterval(iv); }, []);
  const tickAgeS = d?.latest_ts ? Math.max(0, Math.round((nowMs - new Date(d.latest_ts).getTime()) / 1000)) : null;
  const engineStale = err || tickAgeS == null || tickAgeS > 150;
  const s = d?.summary;
  const ie = d?.inefficiency;
  const sv = d?.solver;
  const rows = d?.latest ?? [];
  const jobKo = (j: string | null) => (j === "DS" ? (k ? "양하" : "DS") : j === "LD" ? (k ? "적하" : "LD") : j ?? "");

  return (
    <div className="content cyc-page">
      <div className="cyc-head">
        <div className="cyc-title">
          <h2>{k ? "배차 비교 — 우리 시스템 vs TOS" : "Dispatch — our system vs TOS"}</h2>
          <span className="cyc-title-sub">
            {k
              ? "같은 상자에 대한 우리 추천(트럭·시점)과 TOS 실배차(트럭·시점)의 직접 비교. 그림자(운영 무간섭)."
              : "per-container: our recommendation (truck + timing) vs TOS's actual dispatch. Shadow."}
            {err && <span className="cyc-err"> · {k ? "연결 오류" : "offline"}</span>}
          </span>
        </div>
      </div>

      {/* ── HEADLINE: 상자 단위 시스템 비교 ── */}
      {box && (
        <div className="ls-note" style={{ borderLeft: "3px solid #60a5fa", background: "rgba(96,165,250,0.08)", padding: "10px 12px", marginBottom: 10 }}>
          <div style={{ fontWeight: 700, fontSize: 13, marginBottom: 3 }}>
            {k
              ? `📦 같은 상자, 두 시스템 — 최근 24시간 ${box.boxes_joined.toLocaleString()}상자 짝지음`
              : `📦 Same container, two systems — ${box.boxes_joined.toLocaleString()} boxes paired (24h)`}
          </div>
          {k ? (
            <>
              {`분모: 우리가 추천한 상자 ${box.boxes_reco.toLocaleString()}개 중 TOS도 최초 추천 앞뒤 3시간 안에 실배차한 ${box.boxes_joined.toLocaleString()}개. `}
              {box.boxes_joined > 0 && (
                <>
                  {`트럭까지 같은 선택 ${pct(box.truck_match_pct)}. TOS 배차는 우리 최초 추천보다 중앙 ${signMin(box.gap_p50_s)}분(p25 ${signMin(box.gap_p25_s)} / p75 ${signMin(box.gap_p75_s)}), `}
                  {`${pct(box.tos_after_pct)}가 우리 추천 이후였고 ${pct(box.margin_in_pct)}가 우리 마감선 안쪽이었습니다.`}
                </>
              )}
            </>
          ) : (
            <>
              {`Denominator: of ${box.boxes_reco.toLocaleString()} boxes we recommended, ${box.boxes_joined.toLocaleString()} were also dispatched by TOS within ±3h. `}
              {box.boxes_joined > 0 && `Same truck ${pct(box.truck_match_pct)}. TOS dispatched median ${signMin(box.gap_p50_s)}min after our first recommendation; ${pct(box.margin_in_pct)} inside our deadline.`}
            </>
          )}
          <div style={{ marginTop: 5, fontSize: 11, color: "var(--text-mute)" }}>
            {k
              ? "⚠ 시점 격차는 정오 판정이 아니라 계기입니다 — 우리 마감은 크레인 예측이 아니라 출항 요구 페이스(규범)라, TOS와 다른 것 자체는 틀림이 아닙니다. 상자 단위 추천 기록은 2026-08-10부터라 표본이 차오르는 중입니다."
              : "⚠ Timing gaps are a gauge, not a verdict — our deadline is a departure-pace norm, not a crane-time prediction. Per-container records start 2026-08-10; the sample is still filling."}
          </div>
        </div>
      )}

      {box && box.by_job.length > 0 && (
        <div className="ls-chips" style={{ marginBottom: 8 }}>
          {box.by_job.map((j) => (
            <Chip key={j.jobtype ?? "?"}
                  label={k ? `${jobKo(j.jobtype)} 시점격차 중앙 (${j.n}상자)` : `${j.jobtype} gap median (${j.n})`}
                  value={`${signMin(j.gap_p50_s)}${k ? "분" : "m"}`}
                  accent="#60a5fa" />
          ))}
          <Chip label={k ? "트럭 일치" : "same truck"} value={pct(box?.truck_match_pct)} accent="#a78bfa" />
          <Chip label={k ? "우리 마감선 안쪽 배차" : "inside our deadline"} value={pct(box?.margin_in_pct)} accent="#34d399" />
        </div>
      )}

      {box && box.recent.length > 0 && (
        <>
          <table className="hist-table" style={{ marginTop: 4 }}>
            <thead>
              <tr>
                <th>{k ? "TOS 배차시각" : "TOS dispatch"}</th>
                <th>{k ? "상자" : "Box"}</th>
                <th>QC</th>
                <th>{k ? "유형" : "Job"}</th>
                <th>{k ? "우리 트럭 (최초 추천)" : "Our truck (first)"}</th>
                <th>{k ? "TOS 트럭" : "TOS truck"}</th>
                <th title={k ? "TOS 배차시각 − 우리 최초 추천시각 (+ = TOS가 뒤)" : "TOS dispatch − our first reco"}>{k ? "시점격차" : "Gap"}</th>
                <th title={k ? "우리 마감선 − TOS 배차시각 (+ = 마감 안쪽)" : "our deadline − TOS dispatch"}>{k ? "마감여유" : "Margin"}</th>
              </tr>
            </thead>
            <tbody>
              {box.recent.map((r) => (
                <tr key={r.contno + r.first_ts}>
                  <td className="mono">{r.dispatch_ts ? mytTime(r.dispatch_ts) : "—"}</td>
                  <td className="mono">{r.contno}</td>
                  <td className="mono">{r.qc ?? "—"}</td>
                  <td>{jobKo(r.jobtype)}</td>
                  <td className="mono" style={r.truck_match ? { color: "#34d399" } : undefined}>{r.our_ytno ?? "—"}{r.truck_match ? " ✓" : ""}</td>
                  <td className="mono">{r.tos_ytno ?? "—"}</td>
                  <td className="mono">{signMin(r.gap_s)}{k ? "분" : "m"}</td>
                  <td className="mono" style={{ color: (r.margin_s ?? 0) >= 0 ? "#34d399" : "#f59e0b" }}>{signMin(r.margin_s)}{k ? "분" : "m"}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <div className="ls-note" style={{ marginBottom: 12 }}>
            {k
              ? "ⓘ 최근 20상자. ✓ = 우리가 배차 전에 그 트럭을 추천한 적 있음. 트럭 일치가 낮은 것은 로직이 다르니 자연스럽습니다 — 같은 트럭을 고르는 게 목표가 아닙니다(흉내내기가 목표였다면 TOS를 넘어설 수 없습니다)."
              : "ⓘ Latest 20 boxes. ✓ = we recommended that truck before the dispatch. Low truck agreement is expected — copying TOS is not the goal."}
          </div>
        </>
      )}
      {box && box.boxes_joined === 0 && (
        <div className="cyc-empty" style={{ marginBottom: 12 }}>{k ? "짝지어진 상자 누적 중 — 상자 단위 추천 기록(2026-08-10~)이 차오르면 여기에 나타납니다." : "pairing accumulating (per-container records start 2026-08-10)"}</div>
      )}

      {/* ── 시점 실측 — 판정 없는 계기 ── */}
      {box && box.timing.length > 0 && (
        <div className="ls-card" style={{ borderTopColor: "#60a5fa", padding: "12px 16px", marginBottom: 14 }}>
          <div style={{ fontWeight: 700, marginBottom: 8 }}>{k ? "⏱ 시점 실측 — TOS 배차 → QC 처리까지 (최근 24h, 전 무브)" : "⏱ Timing (measured) — TOS dispatch → QC handling (24h)"}</div>
          <div className="ls-chips">
            {box.timing.map((t) => (
              <Chip key={t.jobtype ?? "?"}
                    label={k ? `${jobKo(t.jobtype)} 실현 리드 p50·p90 (${t.n.toLocaleString()}무브)` : `${t.jobtype} realized lead p50·p90`}
                    value={`${mmss(t.realized_lead_p50_s)} · ${mmss(t.realized_lead_p90_s)}`} />
            ))}
            {box.timing.map((t) => (
              <Chip key={(t.jobtype ?? "?") + "-m"}
                    label={k ? `${jobKo(t.jobtype)} 그중 무부하주행(모형)` : `${t.jobtype} empty-run (model)`}
                    value={mmss(t.modeled_travel_s)} accent="#a3a3a3" />
            ))}
          </div>
          <div className="ls-note" style={{ marginTop: 8 }}>
            {k
              ? "실현 리드 = 배차부터 QC가 이 상자를 처리 완료할 때까지(양하=QC 픽업, 적하=QC 드랍오프 — 4단계 용어). 리드에서 무부하주행(모형)을 뺀 나머지가 픽업·대기·부하주행 몫입니다. 배차가 필요보다 이르면 그만큼 트럭이 잡혀 있습니다 — 다만 '필요'의 잣대로 학습 리드를 쓰면 TOS 실현치로 TOS를 채점하는 동어반복이라, 여기서는 판정 없이 실측만 보여줍니다."
              : "Realized lead = dispatch until the QC finishes handling this box. Lead minus modeled empty-run = pickup/waiting/laden share. We deliberately do not grade TOS against the learned lead — that would score TOS against its own realized average (a tautology). Measurement only, no verdict."}
          </div>
        </div>
      )}

      {/* ── 재배열 여지 (fair-compare) — 우리 성과가 아니라 기회의 크기 ── */}
      {fair && fair.pairs_total > 0 && (
        <div className="ls-note" style={{ borderLeft: "3px solid #34d399", background: "rgba(52,211,153,0.08)", padding: "10px 12px", marginBottom: 12 }}>
          <div style={{ fontWeight: 700, fontSize: 13, marginBottom: 3 }}>
            {k
              ? `⚖️ TOS 매칭의 재배열 여지(상한) — 빈 차 이동 ${fair.avg_savings_pct != null ? Math.round(fair.avg_savings_pct) : "—"}%`
              : `⚖️ Re-matching headroom in TOS's assignments (ceiling): ${fair.avg_savings_pct != null ? Math.round(fair.avg_savings_pct) : "—"}% of empty travel`}
          </div>
          {k
            ? `같은 순간 TOS가 배차한 트럭들(최근 ~4시간 ${fair.pairs_total.toLocaleString()}쌍)을 1:1로 다시 최적 매칭하면 빈 차 이동이 이만큼 줄어듭니다. ` +
              `⚠ 우리 시스템의 성과가 아니라 기회의 크기입니다 — TOS의 배정도 가능한 순열 중 하나라 이 값은 정의상 음수가 나오지 않습니다. ` +
              (fair.avg_tos_capture_pct != null
                ? `무작위 대비 TOS는 이미 여지의 ${Math.round(fair.avg_tos_capture_pct)}%를 잡고 있습니다 — 남은 ${100 - Math.round(fair.avg_tos_capture_pct)}%가 실제로 노려볼 몫입니다. `
                : `무작위 대조군 수집 중(${fair.rand_n}/48). `) +
              `같은 트럭 선택 ${fair.same_pct != null ? Math.round(fair.same_pct) : "—"}%.`
            : `Optimally re-matching the trucks TOS itself dispatched (${fair.pairs_total.toLocaleString()} pairs, ~4h) cuts empty travel by this much. ` +
              `⚠ A ceiling, not our system's result — TOS's assignment is itself a feasible permutation, so this can never be negative. ` +
              (fair.avg_tos_capture_pct != null
                ? `TOS already captures ${Math.round(fair.avg_tos_capture_pct)}% of the random-to-optimal range.`
                : `Random baseline filling (${fair.rand_n}/48).`)}
        </div>
      )}

      {bd && bd.pairs > 0 && (
        <div style={{ marginBottom: 14 }}>
          <div className="area-divider"><span>{k ? "재배열 여지의 출처와 신뢰성 (최근 24h)" : "Where the headroom comes from (24h)"}</span></div>
          <div className="ls-chips" style={{ marginBottom: 8 }}>
            <Chip label={k ? "표본 (짝)" : "pairs"} value={bd.pairs.toLocaleString()} accent="#60a5fa" />
            <Chip label={k ? "우리가 더 나쁨" : "we're worse"} value={pct(bd.worse_pct)} accent={(bd.worse_pct ?? 0) > 20 ? "#f59e0b" : "#34d399"} />
            <Chip label={k ? "TOS와 동일 선택" : "same as TOS"} value={pct(bd.same_pct)} accent="#a3a3a3" />
            <Chip label={k ? "짝당 절감 (중앙)" : "median save/pair"} value={bd.median_save_s != null ? mmss(bd.median_save_s) : "—"} accent="#34d399" />
          </div>
          <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit,minmax(250px,1fr))", gap: 10 }}>
            <BdGroup title={k ? "거리대별 — 이득의 출처" : "by distance — source of the win"} rows={bd.by_dist} />
            <BdGroup title={k ? "작업유형별" : "by jobtype"} rows={bd.by_job.map((r) => ({ ...r, key: jobKo(r.key) }))} />
            <BdGroup title={k ? "시간대별 (MYT·배차시각)" : "by hour (MYT, dispatch time)"} rows={bd.by_hour} />
            <BdGroup title={k ? "크레인별 (상위 표본)" : "by crane (top sample)"} rows={bd.by_crane} />
          </div>
          <div className="ls-note" style={{ marginTop: 8 }}>
            {k
              ? "ⓘ 여지는 거의 전부 원거리에서 나옵니다 — TOS가 트럭을 긴 공차주행 보낸 경우를 더 가까운 트럭으로 교체할 때. 단거리는 TOS가 이미 좋아 재배열이 살짝 나쁠 수 있고, 그게 '우리가 더 나쁨' 꼬리입니다. 짝별 1:1·같은 순간 풀·같은 비용 잣대(우리 도로망 모형)로 측정. 막대: 초록=여지, 빨강=재배열이 더 나쁨(±50% 상한)."
              : "ⓘ Headroom comes almost entirely from far hauls (replacing TOS's long empty drives with a closer truck). Same-instant pool, per-pair 1:1, one cost yardstick (our road-graph model)."}
          </div>
        </div>
      )}

      {/* ── secondary: live recommendation engine detail ── */}
      <div className="area-divider" style={{ marginTop: 22 }}><span>{k ? "권고 엔진 상세 (그림자 매처)" : "Recommendation engine (shadow)"}</span>
        <span className={`pill ${engineStale ? "bad" : "good"}`} style={{ marginLeft: 8 }}>
          {engineStale
            ? (k ? `⚠ 정지 · ${tickAgeS != null ? `${tickAgeS}초 전` : "?"} 틱` : `⚠ stale · ${tickAgeS ?? "?"}s`)
            : (k ? `LIVE · ${tickAgeS}초 전 틱` : `LIVE · ${tickAgeS}s`)}
        </span>
      </div>

      <div style={engineStale ? { opacity: 0.5, filter: "grayscale(0.5)" } : undefined}>
      <div className="ls-chips" style={{ marginBottom: 12 }}>
        <Chip label={k ? "현재 권고 매칭" : "current matches"} value={String(rows.length)} accent="#f472b6" />
        <Chip label={k ? "thrash (재배정율)" : "thrash"} value={pct(s?.switched_pct)} accent={(s?.switched_pct ?? 0) < 10 ? "#34d399" : "#f59e0b"} />
        {/* 크레인 기준 축(mig 0116) — 옛 feasible_pct 는 폐기 축이라 표시하지 않는다 */}
        <Chip label={k ? "마감 충족(크레인 기준)" : "feasible (crane)"} value={pct(s?.feasible_crane_pct)} />
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
              ? "크레인이 트럭이 없어 멈춰 있던 시간 중, 근방(~600m)에 빈 트럭이 있었던 비율입니다. 트럭이 부족해서가 아니라 가까운 빈 트럭을 안 보낸 것 — 최적 매칭이 줄일 수 있는 비효율입니다. 이 값은 현장 실측 계기이며, 후보 선정(굶주림 신호)에는 쓰지 않습니다(2026-08-06 설계 결정)."
              : "Of the time a crane sat stuck waiting for a truck, how often a free truck was within ~600m — a dispatch gap optimal matching would close. Field gauge only; not an input to candidate selection (by design, 2026-08-06)."}
          </div>
        </div>
      )}

      {sv && sv.ticks > 0 && (
        <div className="ls-card" style={{ borderTopColor: "#a78bfa", padding: "12px 16px", marginBottom: 12 }}>
          <div style={{ fontWeight: 700, marginBottom: 8 }}>{k ? "⚙️ 매칭 내부 대조 — 최적 매칭 vs 단순 배정 (최근 30분)" : "⚙️ Internal baseline — optimal vs naive assignment (30m)"}</div>
          <div className="ls-chips">
            <Chip label={k ? "총 도착시간 절감" : "arrival saved"} value={sv.savings_pct != null ? `${sv.savings_pct.toFixed(0)}%` : "—"} accent="#34d399" />
            <Chip label={k ? "마감 누락 (단순→최적)" : "misses (naive→opt)"} value={`${sv.greedy_miss ?? "—"} → ${sv.optimal_miss ?? "—"}`} accent="#a78bfa" />
            <Chip label={k ? "비교 틱" : "ticks"} value={String(sv.ticks)} />
          </div>
          <div className="ls-note" style={{ marginTop: 8 }}>
            {k
              ? "추천은 항상 최적 매칭입니다. '단순 배정'은 추천이 아니라 측정용 기준선 — 같은 풀을 가까운 순으로만 짝지으면 얼마나 나빠지는지를 보는 내부 대조군입니다."
              : "Recommendations are always the optimal matching; the naive assignment is a measurement baseline, never a recommendation."}
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
            <th title={k ? "크레인이 이 컨테이너를 다루는 시각 대비 트럭 도착 여유 — 배차 마감(출항 요구 페이스)과는 다른 축" : "arrival slack vs the crane-need time — a different axis from the dispatch deadline"}>{k ? "크레인여유(분)" : "crane slack(m)"}</th>
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

      {/* ── 이 비교가 말할 수 없는 것 — 정직성 패널 ── */}
      <div className="ls-note" style={{ marginTop: 18, borderLeft: "3px solid #6b7280" }}>
        <div style={{ fontWeight: 700, marginBottom: 4 }}>{k ? "이 비교가 말할 수 없는 것" : "What this page cannot claim"}</div>
        {k ? (
          <ul style={{ margin: 0, paddingLeft: 18, fontSize: 12, lineHeight: 1.7 }}>
            <li>우리 쪽은 그림자 반사실입니다 — 우리가 실제로 보냈다면 이후 트럭 위치가 달라져 연쇄 효과가 생기는데, 이 페이지의 비교는 그 사슬을 반영하지 못합니다.</li>
            <li>비용 잣대는 양쪽 다 우리 도로망 모형(같은 눈금)입니다. 모형 오차 위에서 최적화하면 추정 여지가 참값보다 부풀 수 있어, 무작위 대조군으로 일부 보정합니다.</li>
            <li>짝지음 자체가 "TOS도 배차한 상자"에 조건부입니다 — TOS가 늦게 보냈거나 안 보낸 작업에서의 차이는 여기 안 보입니다.</li>
            <li>트럭 일치·채택률은 품질 지표가 아니라 소비 준비 계기입니다(채택률 시계열은 배차 보드에). TOS를 잘 흉내내는 것이 목표라면 TOS를 넘어설 수 없습니다.</li>
          </ul>
        ) : (
          <ul style={{ margin: 0, paddingLeft: 18, fontSize: 12, lineHeight: 1.7 }}>
            <li>Our side is a shadow counterfactual — chain effects of actually dispatching differently are not modeled.</li>
            <li>Both sides are costed by our road-graph model (one yardstick); optimizing over model noise can inflate estimated headroom, partially calibrated by the random baseline.</li>
            <li>Pairing conditions on TOS having dispatched the box — differences on jobs TOS delayed or skipped are invisible here.</li>
            <li>Truck agreement / adoption are consumption-readiness gauges, not quality metrics.</li>
          </ul>
        )}
      </div>
    </div>
  );
}
