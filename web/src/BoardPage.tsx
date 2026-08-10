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

function Stat({ label, val, unit, cls }: { label: string; val: string; unit?: string; cls?: string }) {
  return (
    <div className="stat-card">
      <div className="label">{label}</div>
      <div className={`val${cls ? ` ${cls}` : ""}`}>{val}{unit ? <span className="unit">{unit}</span> : null}</div>
    </div>
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

      <div className="stats-row" style={{ display: "grid", gridTemplateColumns: "repeat(5, 1fr)", gap: 10, marginBottom: 12 }}>
        <Stat label={ko ? "추천(이번 틱)" : "recos (tick)"} val={String(d?.recos.length ?? "–")} />
        <Stat label={ko ? "남긴 트럭" : "trucks held"} val={String(d?.pool?.trucks_held ?? "–")} unit={ko ? "대" : ""} />
        <Stat label={ko ? "마감 경과 작업" : "overdue works"} val={String(d?.pool?.overdue ?? "–")} />
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

      <p className="ls-note" style={{ marginTop: 10 }}>
        {ko
          ? "SHADOW 모드: 이 추천은 기록만 되며 현장 배차를 바꾸지 않습니다. 적용은 TOS 수동 배차 화면에서 담당자가 합니다. 채택률은 TOS 실배차 기록과 자동 대조한 값입니다."
          : "SHADOW mode: recommendations are recorded only. Apply via the TOS manual dispatch screen. Adoption is auto-scored against actual TOS dispatches."}
      </p>
    </div>
  );
}
