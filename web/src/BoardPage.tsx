// 배차 추천 보드 (P1, 2026-08-10) — 관제/배차 담당자용 "실행 화면".
// 분석 화면(Stage2Page)과 목적이 다르다: 여기는 "지금 이 트럭을 여기로"만 담는다.
// 원천은 /api/dispatch/board 하나 — 최신 틱 추천(급한 순) + 풀 요약 + 신선도 + 채택률.
// 신선도 규율: 틱 나이 150초 초과면 전체 회색 + 정지 배너(낡은 추천은 적용 금지).
import { useEffect, useState } from "react";
import { api, DispatchBoard } from "./api";
import type { Lang } from "./i18n";
import { mytTime, pct } from "./timefmt";

const STALE_S = 150; // 60초 틱 × 2.5 — 이 나이를 넘긴 추천은 적용하면 안 된다

/** 마감 카운트다운: 남은 초 → "3:12" / 지났으면 "지연 4:05" */
function countdown(deadlineIso: string | null, nowMs: number, ko: boolean): { text: string; cls: string } {
  if (!deadlineIso) return { text: "–", cls: "lo" };
  const s = Math.round((new Date(deadlineIso).getTime() - nowMs) / 1000);
  const a = Math.abs(s);
  const t = `${Math.floor(a / 60)}:${String(a % 60).padStart(2, "0")}`;
  if (s < 0) return { text: `${ko ? "지연 " : "late "}${t}`, cls: "bad" };
  if (s < 300) return { text: t, cls: "warn" };
  return { text: t, cls: "ok" };
}

/** 채택률 추이 스파크라인 — 시간당 1점(mig 0144). 실선=상자 채택, 점선=트럭까지 일치. */
function AdoptionTrend({ data }: { data: { box_pct: number | null; ytno_match_pct: number | null }[] }) {
  const w = 100, h = 36;
  const line = (pick: (p: { box_pct: number | null; ytno_match_pct: number | null }) => number | null) =>
    data
      .map((p, i) => {
        const v = pick(p);
        return v == null ? null : `${(i / Math.max(1, data.length - 1)) * w},${h - 3 - (Math.min(v, 100) / 100) * (h - 6)}`;
      })
      .filter(Boolean)
      .join(" ");
  return (
    <svg viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" style={{ width: "100%", height: 36, display: "block" }}>
      <polyline points={line((p) => p.box_pct)} fill="none" stroke="#34d399" strokeWidth="1.4" vectorEffect="non-scaling-stroke" strokeLinejoin="round" />
      <polyline points={line((p) => p.ytno_match_pct)} fill="none" stroke="#a78bfa" strokeWidth="1.2" strokeDasharray="3 2" vectorEffect="non-scaling-stroke" strokeLinejoin="round" />
    </svg>
  );
}

function Stat({ label, val, unit, cls }: { label: string; val: string; unit?: string; cls?: string }) {
  return (
    <div className="stat-card">
      <div className="label">{label}</div>
      <div className={`val${cls ? ` ${cls}` : ""}`}>{val}{unit ? <span className="unit">{unit}</span> : null}</div>
    </div>
  );
}

// ── 배차 깔때기 ────────────────────────────────────────────────────────────────
// "추천이 왜 이 수뿐인가"를 단계 계수로 보여준다. 미터(단일 색)는 **직전 단계 대비 비율** —
// 절대폭으로 그리면 494 vs 8 이라 뒷단계가 안 보인다. 숫자는 텍스트 토큰, 색은 비율만 담는다.
function FunnelStage({ n, label, note, prev, hot, chip, title, ko }: {
  n: number; label: string; note?: string; prev?: number; hot?: boolean; chip?: string; title?: string; ko: boolean;
}) {
  const p = prev != null && prev > 0 ? Math.min(100, Math.round((n / prev) * 100)) : null;
  const pctTxt = p != null ? (ko ? `직전의 ${p}%` : `${p}% of prev`) : null;
  return (
    <div className={`bf-stage${hot ? " bf-hot" : ""}`} title={title}>
      <div className="bf-n">{n.toLocaleString()}{chip ? <span className="bf-chip-bad"> {chip}</span> : null}</div>
      <div className="bf-l">{label}</div>
      {p != null && <div className="bf-track"><div className="bf-fill" style={{ width: `${p}%` }} /></div>}
      <div className="bf-note">{[note, pctTxt].filter(Boolean).join(" · ")}</div>
    </div>
  );
}

function Funnel({ d, ko, stale }: { d: DispatchBoard; ko: boolean; stale: boolean }) {
  const f = d.funnel;
  if (!f) return null;
  const trucks = d.pool?.n_trucks ?? null;
  const held = d.pool?.trucks_held ?? null;
  return (
    <section className={`tcard${stale ? " wp-stale" : ""}`} style={{ marginBottom: 12 }}>
      <div className="tcard-head">
        <h3>{ko ? "배차 깔때기" : "Dispatch funnel"}
          <span className="h3-sub">{ko
            ? "계획 → 지시 발행 → 미배차 → 마감 도래 → 추천 — 추천이 적은 건 작업이 없어서가 아니라 '지금 시작해야 할 일'만 담기 때문"
            : "planned → issued → unassigned → due now → recos — few recos = only what must start now, not a lack of work"}</span></h3>
      </div>
      <div className="tcard-body">
        <div className="bfunnel">
          <FunnelStage ko={ko} n={f.planned_backlog_cont} label={ko ? "계획 잔여 (컨테이너)" : "planned backlog (containers)"}
            note={ko ? "지시 미발행 — TOS가 작업 ~1시간 전 발행" : "orders not yet cut (~1h ahead)"}
            title={ko ? "적부계획에는 있으나 TOS 작업지시가 아직 없는 컨테이너 (큐 잔여 카운터 − 발행분)" : "planned but no TOS order yet"} />
          <div className="bf-arrow">▸</div>
          <FunnelStage ko={ko} n={f.issued} label={ko ? "지시 발행 (상자)" : "issued (boxes)"}
            note={ko ? "마감 계산 완료" : "deadline computed"}
            title={ko ? "TOS 작업지시가 있고 배차 마감이 계산된 상자(트럭 몫 — 트윈은 1로 셈)" : "issued truck-loads with a computed deadline"} />
          <div className="bf-arrow">▸</div>
          <FunnelStage ko={ko} n={f.unassigned} label={ko ? "TOS 미배차" : "unassigned"} prev={f.issued}
            note={ko ? `배차됨 ${(f.issued - f.unassigned).toLocaleString()}` : `${(f.issued - f.unassigned).toLocaleString()} assigned`}
            title={ko ? "우리 배차 대상 — TOS가 아직 트럭을 안 붙인 상자" : "our dispatch universe"} />
          <div className="bf-arrow">▸</div>
          <FunnelStage ko={ko} n={f.due_now} label={ko ? "마감 도래" : "due now"} prev={f.unassigned} hot
            chip={f.overdue_now > 0 ? `🔴 ${f.overdue_now} ${ko ? "경과" : "late"}` : undefined}
            note={ko ? "지금+5분 안 마감" : "deadline ≤ now+5m"}
            title={ko ? "배차 마감(출항 요구 페이스 균등 배분)이 지금+5분 안에 든 미배차 상자 — 매처가 이번 틱에 담는 목록과 같은 잣대" : "deadline within now+5m — the matcher's pool criterion"} />
          <div className="bf-arrow">▸</div>
          <FunnelStage ko={ko} n={d.recos.length} label={ko ? "추천 (이번 틱)" : "recos (tick)"} prev={f.due_now} hot
            note={ko ? "60초마다 재계산" : "recomputed every 60s"}
            title={ko ? "매처 마지막 틱의 트럭↔상자 추천 수" : "matched pairs, last tick"} />
          <div className="bf-sep" />
          <div className="bf-stage bf-side" title={ko ? "후보 트럭이 도래 작업보다 많으면 남긴다 — 조기 배차(크레인 앞 대기)를 만들지 않기 위한 설계" : "surplus trucks are held — no early dispatch by design"}>
            <div className="bf-n">{trucks != null ? trucks.toLocaleString() : "–"}</div>
            <div className="bf-l">{ko ? "후보 트럭" : "candidate trucks"}</div>
            <div className="bf-note">{ko ? `남김 ${held ?? "–"}대 (억지로 채우지 않음)` : `${held ?? "–"} held (no force-fill)`}</div>
          </div>
        </div>
        <div className="lvp-note" style={{ marginTop: 8 }}>{ko
          ? "마감은 출항 요구 페이스 균등 배분이라 '지금 시작해야 할 일'이 현장 처리속도만큼만 매 틱 도래한다. 추천이 실배차로 반영되지 않으면 그 상자는 다음 틱에 다시 담긴다 — 반복 배차는 이미 이 구조 안에 있다."
          : "Deadlines are paced to departure demand, so work comes due at the rate it must start. Unapplied recos re-enter the pool next tick — the loop is already continuous."}</div>
      </div>
    </section>
  );
}

export default function BoardPage({ lang }: { lang: Lang }) {
  const ko = lang === "ko";
  const [d, setD] = useState<DispatchBoard | null>(null);
  const [err, setErr] = useState(false);
  const [nowMs, setNowMs] = useState(Date.now());

  useEffect(() => {
    let live = true;
    const load = () => api.dispatchBoard().then((v) => { if (live) { setD(v); setErr(false); } }).catch(() => { if (live) setErr(true); });
    load();
    const poll = setInterval(load, 15_000);
    const tick = setInterval(() => setNowMs(Date.now()), 1_000); // 카운트다운은 폴링 사이에도 흐른다
    return () => { live = false; clearInterval(poll); clearInterval(tick); };
  }, []);

  // 신선도: 서버가 준 age_s 에 그 응답을 받은 뒤 흐른 시간을 더해 산다
  const [fetchedAt, setFetchedAt] = useState(Date.now());
  useEffect(() => { setFetchedAt(Date.now()); }, [d]);
  const ageS = d?.age_s != null ? d.age_s + Math.round((nowMs - fetchedAt) / 1000) : null;
  const stale = err || ageS == null || ageS > STALE_S;

  const shadow = (d?.mode ?? "shadow") !== "active";
  const ad = d?.adoption ?? null;

  return (
    <div className="content cyc-page">
      <div className="cyc-head">
        <div className="cyc-title">
          <h2>
            {ko ? "배차 추천 보드" : "Dispatch Board"}
            <span className={`pill ${shadow ? "warn" : "good"}`} style={{ marginLeft: 10 }}>
              {shadow ? "SHADOW" : "ACTIVE"}
            </span>
            <span className={`pill ${stale ? "bad" : "good"}`} style={{ marginLeft: 6 }}>
              {stale
                ? (ko ? `⚠ 정지 · ${ageS != null ? `${ageS}초 전` : "?"} 데이터` : `⚠ stale · ${ageS ?? "?"}s old`)
                : (ko ? `LIVE · ${ageS}초 전 틱` : `LIVE · tick ${ageS}s ago`)}
            </span>
          </h2>
          <span className="cyc-title-sub">
            {ko
              ? "지금 보내야 할 트럭 — 마감(출항 요구 페이스)이 급한 순. 회색이면 낡은 추천이니 적용하지 마세요."
              : "Trucks to send now — ordered by dispatch deadline (departure-pace). Grey = stale, do not apply."}
            {d?.generated_at ? ` · ${ko ? "틱" : "tick"} ${mytTime(d.generated_at)} MYT` : ""}
          </span>
        </div>
      </div>

      {d && <Funnel d={d} ko={ko} stale={stale} />}

      <div className="stats-row" style={{ display: "grid", gridTemplateColumns: "repeat(2, 1fr)", gap: 10, marginBottom: 12 }}>
        <Stat
          label={ko ? "채택률(상자·24h)" : "adoption (box·24h)"}
          val={pct(ad?.box_pct)}
        />
        <Stat
          label={ko ? "트럭까지 일치" : "truck match"}
          val={pct(ad?.ytno_match_pct)}
        />
      </div>

      <section className={`tcard${stale ? " wp-stale" : ""}`}>
        <div className="tcard-head">
          <h3>{ko ? "지금 이 트럭을 여기로" : "Send this truck here, now"}</h3>
          <span className="head-sub">
            {ko
              ? `채택률 분모: 24시간 내 추천 상자 ${ad?.boxes_reco ?? "–"}개 중 20분 내 실배차 ${ad?.boxes_dispatched ?? "–"}개`
              : `adoption: ${ad?.boxes_dispatched ?? "–"} of ${ad?.boxes_reco ?? "–"} reco'd boxes dispatched within 20 min`}
          </span>
        </div>
        <div className="tcard-body">
          {!d || d.recos.length === 0 ? (
            <div className="cyc-empty">{err ? (ko ? "연결 오류" : "connection error") : (ko ? "지금 보낼 것이 없습니다 — 마감이 온 작업이 없음" : "nothing due right now")}</div>
          ) : (
            <table className="hist-table board-table">
              <thead>
                <tr>
                  <th>#</th>
                  <th>{ko ? "트럭" : "truck"}</th>
                  <th>{ko ? "컨테이너" : "container"}</th>
                  <th>QC</th>
                  <th>{ko ? "베이" : "bay"}</th>
                  <th>{ko ? "유형" : "type"}</th>
                  <th>{ko ? "출발지" : "from"}</th>
                  <th>{ko ? "보내야 할 때까지" : "send within"}</th>
                  <th>{ko ? "예상 도착" : "ETA"}</th>
                </tr>
              </thead>
              <tbody>
                {d.recos.map((r, i) => {
                  const c = countdown(r.dispatch_deadline_ts, nowMs, ko);
                  return (
                    <tr key={r.ytno}>
                      <td>{i + 1}</td>
                      <td className="board-tt">{r.ytno}</td>
                      <td className="mono">{r.contno ?? "–"}</td>
                      <td>{r.qc ?? "–"}</td>
                      <td className="mono">{r.queuename ?? "–"}</td>
                      <td>
                        <span className={`pill ${r.jobtype === "DS" ? "good" : "warn"}`}>
                          {r.jobtype === "DS" ? (ko ? "양하" : "DS") : r.jobtype === "LD" ? (ko ? "적하" : "LD") : r.jobtype ?? "–"}
                        </span>
                      </td>
                      <td className="mono">{r.jobtype === "LD" ? r.src_block ?? "–" : ko ? "안벽" : "quay"}</td>
                      <td className={c.cls}>{c.text}</td>
                      <td>{r.arrival_s != null ? `${Math.round(r.arrival_s / 60)}${ko ? "분" : "m"}` : "–"}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          )}
        </div>
      </section>

      {(d?.adoption_trend.length ?? 0) >= 2 && (
        <section className="tcard" style={{ marginTop: 12 }}>
          <div className="tcard-head">
            <h3>{ko ? "채택률 추이" : "Adoption trend"}<span className="h3-sub">{ko ? "시간당 1점 · 최근 7일" : "hourly · 7d"}</span></h3>
            <div className="head-sub"><span className="muted">{ko ? "실선 = 상자 채택 · 점선 = 트럭까지 일치" : "solid = box · dashed = truck match"}</span></div>
          </div>
          <div className="tcard-body">
            <AdoptionTrend data={d?.adoption_trend ?? []} />
          </div>
        </section>
      )}

      <p className="ls-note" style={{ marginTop: 10 }}>
        {ko
          ? "SHADOW 모드: 이 추천은 기록만 되며 현장 배차를 바꾸지 않습니다. 적용은 TOS 수동 배차 화면에서 담당자가 합니다. 채택률은 TOS 실배차 기록과 자동 대조한 값입니다 — 낮다는 것은 틀렸다는 뜻이 아니라 TOS와 다른 판단을 하고 있다는 뜻일 수 있습니다."
          : "SHADOW mode: recommendations are recorded only. Apply via the TOS manual dispatch screen. Adoption is auto-scored against actual TOS dispatches — low means we differ from TOS, not necessarily that we're wrong."}
      </p>
    </div>
  );
}
