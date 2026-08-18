// AI Dispatch Engine — Health Monitor (REAL data from the Stage-2 shadow matcher).
// Liveness + recommendation volume/stability/feasibility/optimisation gain, arrival-time
// distribution, hourly stability trend, and the latest recommended decisions. Shadow mode —
// recommendations are logged & scored, they do NOT drive live dispatch.
import { useEffect, useState } from "react";
import { type Lang } from "./i18n";
import { api, type HealthDispatch } from "./api";
import { mytTime } from "./timefmt";

const ko = (l: Lang) => l === "ko";
const pct = (v: number | null | undefined) => (v == null ? "—" : v.toFixed(v < 10 ? 1 : 0));
const mmss = (s: number | null | undefined) => (s == null ? "—" : `${Math.floor(s / 60)}:${String(Math.round(s % 60)).padStart(2, "0")}`);


function Hist({ data }: { data: { label: string; n: number }[] }) {
  const max = Math.max(1, ...data.map((h) => h.n));
  return (
    <div className="hp-hist">
      {data.map((h) => (
        <div className="hp-hist-col" key={h.label}>
          <div className="hp-hist-bar"><div className="hp-hist-fill" style={{ height: `${(h.n / max) * 100}%` }} /></div>
          <div className="hp-hist-lbl mono">{h.label}</div>
        </div>
      ))}
    </div>
  );
}

function Trend({ data }: { data: { thrash_pct: number | null }[] }) {
  const w = 100, h = 100;
  const vals = data.map((p) => p.thrash_pct).filter((v): v is number => v != null);
  const hi = Math.max(15, ...vals) * 1.1;
  const y = (v: number) => h - (v / hi) * (h - 8) - 4;
  const pts = data
    .map((p, i) => (p.thrash_pct == null ? null : `${(i / Math.max(1, data.length - 1)) * w},${y(p.thrash_pct)}`))
    .filter(Boolean)
    .join(" ");
  return (
    <svg className="hp-trend" viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none">
      <polyline points={pts} fill="none" stroke="#34d399" strokeWidth="1.4" vectorEffect="non-scaling-stroke" strokeLinejoin="round" />
    </svg>
  );
}

export default function HealthPage({ lang }: { lang: Lang }) {
  const k = ko(lang);
  const t = (ka: string, e: string) => (k ? ka : e);
  const [d, setD] = useState<HealthDispatch | null>(null);
  const [err, setErr] = useState(false);
  useEffect(() => {
    let alive = true;
    const poll = () => api.healthDispatch().then((r) => { if (alive) { setD(r); setErr(false); } }).catch(() => alive && setErr(true));
    poll();
    const iv = setInterval(poll, 15000);
    return () => { alive = false; clearInterval(iv); };
  }, []);

  const cards = [
    {
      label: t("권고 신선도", "Freshness"),
      val: d?.last_tick_age_s != null ? String(d.last_tick_age_s) : "—",
      unit: "s",
      note: t("정상 ≤180s · 작업목록 착지마다", "OK ≤180s · on each work-list landing"),
      tone: d?.up ? "good" : "warn",
    },
    {
      label: t("현재 권고", "Current matches"),
      val: d ? String(d.matches_latest) : "—",
      unit: t("건", ""),
      note: t(`최근 1시간 ${d?.ticks_1h ?? 0}틱`, `${d?.ticks_1h ?? 0} ticks / 1h`),
      tone: "good",
    },
    {
      label: t("안정성 (재배정율)", "Stability (thrash)"),
      val: pct(d?.thrash_pct),
      unit: "%",
      note: t("낮을수록 안정", "lower = stable"),
      tone: (d?.thrash_pct ?? 0) < 12 ? "good" : "warn",
    },
    {
      // 크레인 기준 축(mig 0116) — 옛 feasible_pct 는 적하에서 야드 도착만 세던 폐기 축이라 쓰지 않는다.
      label: t("마감 충족", "Feasible"),
      val: pct(d?.feasible_crane_pct),
      unit: "%",
      note: t("크레인 기준 — 권고 트럭이 크레인 시각까지 도착", "crane-basis: truck reaches crane in time"),
      tone: "good",
    },
    {
      label: t("최적화 이득", "Optimisation gain"),
      val: pct(d?.savings_pct),
      unit: "%",
      note: t("단순배정 대비 도착시간 절감", "vs greedy, arrival saved"),
      tone: "good",
    },
  ];

  return (
    <div className="content hp-root">
      {/* engine header */}
      <div className="hp-eng">
        <div>
          <div className="hp-eng-title">{t("AI 배차 엔진 — 헬스 모니터", "AI Dispatch Engine — Health Monitor")}</div>
          <div className="hp-eng-sub">
            {t("그림자 모드 (권고만 기록·검증, 실제 배차 무간섭)", "Shadow mode — recommendations logged, live dispatch untouched")}
            {" · "}{t("작업목록 착지마다", "on each work-list landing")}
            {d?.last_tick_age_s != null && ` · ${t("마지막 권고", "last")} ${d.last_tick_age_s}s ${t("전", "ago")}`}
            {err && <span className="hp-eng-sub" style={{ color: "#ef4444" }}> · {t("연결 오류", "offline")}</span>}
          </div>
        </div>
        <div className="hp-eng-pills">
          <span className={`pill ${d?.up ? "good" : "warn"}`}>{d?.up ? t("엔진 정상", "Engine OK") : t("권고 지연", "Stale")}</span>
          <span className="pill">{t("그림자", "Shadow")}</span>
        </div>
      </div>

      {/* 5 stat cards */}
      <div className="grid hp-stats">
        {cards.map((s) => (
          <div className="stat-card" key={s.label}>
            <div className="label">{s.label}</div>
            <div className="val">{s.val}<span className="unit">{s.unit}</span></div>
            <div className={`delta ${s.tone}`}>{s.note}</div>
          </div>
        ))}
      </div>

      {/* arrival-time dist + stability trend */}
      <div className="grid hp-2col">
        <section className="tcard">
          <div className="tcard-head"><h3>{t("권고 도착시간 분포", "Recommended arrival-time dist.")}<span className="h3-sub">{t("최근 1시간 · 분", "last 1H · min")}</span></h3></div>
          <div className="tcard-body">
            <Hist data={d?.arrival_hist ?? []} />
            <div className="hp-pcts">
              {[
                [t("중앙(p50)", "p50"), d?.arr_p50_s != null ? `${(d.arr_p50_s / 60).toFixed(1)}분` : "—", ""],
                ["p90", d?.arr_p90_s != null ? `${(d.arr_p90_s / 60).toFixed(1)}분` : "—", ""],
                [t("도로망 라우팅(R)", "routed (R)"), `${pct(d?.routed_pct)}%`, "good"],
              ].map(([key, v, c], i) => (
                <div key={i}><div className="hp-pct-k">{key}</div><div className={`hp-pct-v mono ${c}`}>{v}</div></div>
              ))}
            </div>
          </div>
        </section>
        <section className="tcard">
          <div className="tcard-head"><h3>{t("안정성(재배정율) 추이", "Stability (thrash) trend")}<span className="h3-sub">24H</span></h3><div className="head-sub"><span className="muted">{t("낮을수록 안정", "lower = stable")}</span></div></div>
          <div className="tcard-body">
            <Trend data={d?.trend ?? []} />
            <div className="hp-pcts">
              <div><div className="hp-pct-k">{t("현재 재배정율", "now")}</div><div className="hp-pct-v mono good">{pct(d?.thrash_pct)}%</div></div>
              <div><div className="hp-pct-k">{t("시간 구간", "hours")}</div><div className="hp-pct-v mono">{d?.trend.length ?? 0}</div></div>
              <div><div className="hp-pct-k">{t("최적 이득", "gain")}</div><div className="hp-pct-v mono good">{pct(d?.savings_pct)}%</div></div>
            </div>
          </div>
        </section>
      </div>

      {/* recent decision log (real recommendations) */}
      <section className="tcard">
        <div className="tcard-head"><h3>{t("최근 권고 결정", "Recent recommendations")}<span className="h3-sub">{d?.decisions.length ?? 0}</span></h3><div className="head-sub"><span className="muted">{t("최신 틱 · 15초 갱신", "latest tick · 15s")}</span></div></div>
        <div className="tcard-body" style={{ padding: 0 }}>
          <table className="dec-table">
            <thead>
              <tr>
                <th>{t("시각", "Time")}</th><th>{t("차량", "Vehicle")}</th><th>{t("작업 (QC·큐)", "Work")}</th><th>{t("유형", "Job")}</th>
                <th>{t("도착", "Arrival")}</th><th>{t("마감여유", "Slack")}</th><th>OD</th><th>{t("전환", "Switch")}</th><th>{t("결과", "Result")}</th>
              </tr>
            </thead>
            <tbody>
              {(d?.decisions ?? []).map((x, i) => (
                <tr key={i}>
                  <td className="mono">{mytTime(x.ts)}</td>
                  <td className="mono">{x.ytno}</td>
                  <td className="mono">{x.qc} <span style={{ color: "var(--text-mute)" }}>{x.queuename}</span></td>
                  <td>{x.jobtype === "DS" ? t("양하", "DS") : x.jobtype === "LD" ? t("적하", "LD") : x.jobtype}</td>
                  <td className="mono">{mmss(x.arrival_s)}</td>
                  <td className="mono" style={{ color: (x.deadline_slack_s ?? 0) < 0 ? "#ef4444" : "#34d399" }}>{x.deadline_slack_s != null ? `${x.deadline_slack_s >= 0 ? "+" : ""}${(x.deadline_slack_s / 60).toFixed(1)}` : "—"}</td>
                  <td style={{ color: x.cost_tier === "R" ? "#60a5fa" : "var(--text-mute)" }}>{x.cost_tier}</td>
                  <td>{x.switched ? "↔" : ""}</td>
                  <td>{x.feasible ? <span className="ok">{t("제때 ✓", "in time ✓")}</span> : <span className="fb">{t("늦음", "late")}</span>}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {(d?.decisions ?? []).length === 0 && <div className="cyc-empty">{t("권고 없음 (후보 대기 중)", "no recommendations yet")}</div>}
        </div>
      </section>
    </div>
  );
}
