// TT Dispatch — 우리 배차 로직의 실황판 (2026-08-10 전면 재작성).
// 목적: 현행 로직이 어떻게 돌고 있는지 한눈에 — QC 쪽은 배차가 어디까지 됐고 어디가
// 급한지(매처와 같은 잣대), 차량 쪽은 배차 가능한 차(후보 풀 = 매처 발행값)와 배차된
// 차(작업 중)의 상태. 수치는 전부 백엔드 권위값이고 화면은 로컬 재계산을 하지 않는다.
//   ① 배차 압력 보드 — 선박(급한 순) × QC: 마감 경과·배차 대수·처리속도
//   ② QC 작업 타임라인 — 크레인 순서(timeline_pos) × 적부계획 슬롯(slot_idx) · 상자별 마감
//   ③ 후보 차량 풀 — 매처가 이번 틱에 쓸 수 있는 차량(발행된 비용 기저 그대로)
//   ④ 작업 중 차량 — 매처가 건너뛰는(이미 일 있는) 트럭
// 원천: /api/workpool(TOS 미러 ~90s) + /api/livemap/positions(웹소켓 GPS/PLC, 3s 폴).
import { useEffect, useMemo, useState } from "react";
import { type Lang } from "./i18n";
import { api, type WorkpoolResponse, type WpQc, type WpQueue, type WpMove, type WpBoxDeadline, type Stage2Advisory, type ComparePick } from "./api";

const ko = (lang: Lang) => lang === "ko";

// ── shared live sources ──
type Dev = {
  id: string; cls: string; speed?: number; age_s?: number; jobtype?: string | null;
  dispatch?: string; dispatch_reason?: string; arrival?: string; topos1?: string;
  plc?: { is_loaded: boolean; age_s: number; mph?: number; last_move_age_s?: number };
};
type Snap = {
  connected: boolean; as_of: string | null; dispatch_counts?: Record<string, number>;
  crane_mph_live?: number | null; crane_moves_60m?: number; cranes_working?: number;
  // GPS 침묵(120초+)인데 매처가 풀에 유지하는 후보(무브로그 앵커/드랍 근접). devices에는 안 나온다.
  stage2_held?: { id: string; jobtype?: string; free_in_s: number; anchored: boolean }[];
  stage2_pool_age_s?: number | null;
  devices: Dev[];
};

function usePositions(ms = 3000) {
  const [snap, setSnap] = useState<Snap | null>(null);
  const [err, setErr] = useState(false);
  useEffect(() => {
    let alive = true;
    const poll = async () => {
      try {
        const r = await fetch("/api/livemap/positions");
        if (!r.ok) throw new Error();
        const j: Snap = await r.json();
        if (alive) { setSnap(j); setErr(false); }
      } catch { if (alive) setErr(true); }
    };
    poll();
    const iv = setInterval(poll, ms);
    return () => { alive = false; clearInterval(iv); };
  }, [ms]);
  return { snap, err };
}

function useWorkpool(ms = 15000) {
  const [data, setData] = useState<WorkpoolResponse | null>(null);
  const [err, setErr] = useState(false);
  useEffect(() => {
    let alive = true;
    const poll = () => api.workpool().then((d) => { if (alive) { setData(d); setErr(false); } }).catch(() => { if (alive) setErr(true); });
    poll();
    const iv = setInterval(poll, ms);
    return () => { alive = false; clearInterval(iv); };
  }, [ms]);
  return { data, err };
}

// dispatch-state colors (shared with the live map / vehicle pool)
// ★트럭 작업 4단계 용어(무부하주행→픽업→부하주행→드랍오프, 2026-08-04 지정)로 표기.
const DSP_META: Record<string, { ko: string; en: string; color: string }> = {
  idle: { ko: "유휴 (배차 가능)", en: "Idle (available)", color: "#22c55e" },
  staging: { ko: "배차됨·픽업 대기", en: "Assigned·pickup wait", color: "#0ea5e9" },
  soon_idle: { ko: "드랍오프 중·곧 빔", en: "Drop-off·soon free", color: "#f59e0b" },
  delivering: { ko: "부하주행", en: "Laden haul", color: "#64748b" },
  wait_rtg: { ko: "블록 도착·드랍오프 대기", en: "At block·drop-off wait", color: "#ef4444" },
  empty_travel: { ko: "무부하주행", en: "Empty haul", color: "#94a3b8" },
};

// ETW countdown from the accurate TOS ETW RPC (qc_etw_utc via the tos_etw_gateway). The
// snapshot has a TTL (expires); past it, the value is stale and shown dimmed.
// Relative duration as a colon clock: "23:45" (m:ss), "1:23:45" (h:mm:ss past an hour), "0:08" (<1m).
function fmtRel(sec: number): string {
  const a = Math.abs(Math.round(sec));
  const h = Math.floor(a / 3600), m = Math.floor((a % 3600) / 60), s = a % 60;
  const p = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${p(m)}:${p(s)}` : `${m}:${p(s)}`;
}

// HH:MM clock in the terminal/user TZ.
function clockOf(ts: string | null | undefined, ko: boolean): string | null {
  return ts ? new Date(ts).toLocaleTimeString(ko ? "ko-KR" : "en-US", { timeZone: "Asia/Kuala_Lumpur", hour: "2-digit", minute: "2-digit", hour12: false }) : null;
}
// "MM-DD HH:MM" — departure can fall on a different day, so include the date.
function dayClockOf(ts: string | null | undefined, ko: boolean): string | null {
  if (!ts) return null;
  const d = new Date(ts);
  return `${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")} ${clockOf(ts, ko)}`;
}
// dispatch-deadline format = colon clock with a "지연/late" prefix when past.
function clockDur(sec: number, ko: boolean): string {
  return (sec < 0 ? (ko ? "지연 " : "late ") : "") + fmtRel(sec);
}
// remaining time as an unambiguous DURATION (not a clock): "5시간 42분" / "23분" / "지연 4분".
function relDurOf(sec: number, ko: boolean): string {
  const neg = sec < 0;
  const a = Math.abs(Math.round(sec));
  const h = Math.floor(a / 3600), mn = Math.floor((a % 3600) / 60);
  const t = h > 0 ? `${h}${ko ? "시간 " : "h "}${mn}${ko ? "분" : "m"}` : a >= 60 ? `${mn}${ko ? "분" : "m"}` : `${a}${ko ? "초" : "s"}`;
  return (neg ? (ko ? "지연 " : "late ") : "") + t;
}

function etwLabel(etw: string | null | undefined, expires: string | null | undefined, lang: Lang): { text: string; cls: string } | null {
  if (!etw) return null;
  const sec = Math.round((Date.parse(etw) - Date.now()) / 1000);
  const stale = expires != null && Date.parse(expires) < Date.now();
  const t = fmtRel(sec);
  if (stale) return { text: ko(lang) ? `${t} (만료)` : `${t} (stale)`, cls: "lo" };
  if (sec < -30) return { text: ko(lang) ? `지연 ${t}` : `overdue ${t}`, cls: "bad" };
  if (sec < 90) return { text: ko(lang) ? `곧 ${t}` : t, cls: "bad" };
  if (sec < 600) return { text: t, cls: "warn" };
  return { text: t, cls: "ok" };
}

const kindChip = (jt: string | null) => (jt === "DS" ? "dsc" : jt === "LD" ? "lod" : "shf");
const kindLabel = (jt: string | null) => (jt === "DS" ? "DSC" : jt === "LD" ? "LOD" : "SHF");

// ───────────────────────── live vehicle pool ─────────────────────────
type LiveTT = { id: string; cls: string; dispatch?: string; jobtype?: string; topos1?: string; dispatch_reason?: string; swappable?: boolean; dest_remaining_m?: number; nearest_rtg_m?: number; free_in_s?: number; free_in_hi_s?: number; in_pool?: boolean };

// "곧 빔 ~N분 (최대 M분)" — 백엔드 free_in_s. 후보 상태(soon_idle/wait_rtg)에서는 매처가
// 마지막 틱에 실제로 쓴 비용 기저(base)를 그대로 실어준 값이고(≤60s 지연), 그 외 상태나
// 매처 미발행 구간만 상수표 폴백이다. hi(p90)는 상수표 경로에서만 있다.
function freeInLabel(d: LiveTT, lang: Lang): string | null {
  if (d.free_in_s == null) return null;
  const m = Math.round(d.free_in_s / 60);
  const hi = d.free_in_hi_s != null ? Math.round(d.free_in_hi_s / 60) : null;
  return ko(lang) ? `~${m}분${hi ? ` (최대 ${hi})` : ""}` : `~${m}m${hi ? ` (max ${hi})` : ""}`;
}

// localized "why" for a candidate TT (built from structured fields, not the
// backend's Korean dispatch_reason — so EN mode shows no Korean).
function soonWhy(d: LiveTT, lang: Lang): string {
  const k = ko(lang);
  if (d.dispatch === "idle") return k ? "지금 배차 가능" : "dispatchable now";
  if (d.dispatch === "wait_rtg") {
    const m = d.nearest_rtg_m != null ? ` ${Math.round(d.nearest_rtg_m)}m` : "";
    return k ? `블록 도착 · RTG 대기${m}` : `at block · waiting RTG${m}`;
  }
  if (d.nearest_rtg_m != null) {
    const m = Math.round(d.nearest_rtg_m);
    return k ? `블록 RTG 근접 ${m}m` : `block RTG ${m}m`;
  }
  return k ? "안벽 핸드오버 · PLC" : "quay handover · PLC";
}

// ── 후보 차량 = 매처가 마지막 틱에 실제로 풀에 넣은 트럭 ─────────────────────────────────
// ★2026-08-19 이후 풀은 GPS 상태 라벨로 정해지지 않는다: 현장이 pull 구조(트럭이 비면 요청)라
// 매처는 ①원천 크레인 로그로 '이미 빈 트럭'을 전부(라벨이 delivering/staging이어도, GPS가 잠잠해도)
// ②예측 자유까지 15분 안인 배차 중 트럭을 담는다. 그래서 프론트가 상태로 후보를 재구성하면 틀린다 —
// 백엔드가 device.in_pool로 소속을 그대로 알려준다(매처 미발행/낡음이면 undefined).
// GPS 침묵으로 devices에 아예 없는 후보는 종전대로 snap.stage2_held로 따로 받아 합산한다.
// (옛 상태 목록은 in_pool이 없는 낡은 API를 만났을 때의 폴백으로만 남긴다.)
const CANDIDATE_STATES = ["idle", "soon_idle", "wait_rtg"] as const;
// 정렬 기준 = 매처의 간선 비용 기저(base): 지금 자유면 0, 아니면 자유까지 예측 초.
// free_in_s는 후보 상태에서 매처가 마지막 틱에 쓴 base 그대로다(백엔드 stage2_pool 발행,
// ≤60s 지연) — 양하는 그 트립의 무브로그 앵커 우선, 정차면 정차 중앙값, 이동 중이면 학습값,
// 매처 미발행 구간만 상수표 폴백. 그래서 이 정렬은 매처의 순서와 같은 숫자로 선다.
// 비용 기저(free_in_s)가 있으면 그것이 매처가 쓴 값이다. 없을 때만 GPS 라벨로 떨어진다.
// ★2026-08-21: 원천 크레인 로그로 '이미 빈 것'이 확인된 트럭은 GPS 라벨이 delivering/staging 이어도 base 0 이다 —
// 라벨을 먼저 보면 그 트럭이 "곧 자유"로 밀려 카드가 자기 정의("비용 기저 0초")와 어긋난다.
const freeInOf = (d: LiveTT): number => d.free_in_s ?? (d.dispatch === "idle" ? 0 : 9e9);
// ⚠ 후보를 두 갈래로만 센다: '지금 유휴' vs '곧 자유'. 곧유휴/RTG대기를 갈라 세우지 않는다 —
// ADR 0002(유휴 리드타임은 예측하지 않는다, 채택 2026-07-15)로 상태별 개별 예측을 중단했다.
// 시간-투-프리의 출처는 위 주석(앵커→정차 중앙값→학습값→상수)이 현행이다. 화면이 세 갈래로
// 나누면 시스템이 실제로 하지 않는 구분을 하는 것처럼 읽힌다. 상태는 행 앞 점(보조 정보)으로만 남긴다.
const isIdleNow = (d: LiveTT) => freeInOf(d) <= 0;
// localized dispatch-state label for tooltips
function dspTitle(dispatch: string | undefined, lang: Lang): string | undefined {
  if (!dispatch || !DSP_META[dispatch]) return undefined;
  return ko(lang) ? DSP_META[dispatch].ko : DSP_META[dispatch].en;
}

// where the assigned truck physically is / what it's doing now — phrased per job direction
// (LD picks up at a block and drops at the QC; DS receives at the QC and drops at the yard).
function ttWhere(tt: Dev | undefined, jobtype: string | null, lang: Lang): string | null {
  if (!tt?.dispatch) return null;
  const k = ko(lang), d = tt.dispatch, arrived = tt.arrival === "ARRIVED";
  if (d === "soon_idle" || (arrived && (d === "delivering" || d === "staging"))) {
    return jobtype === "LD" ? (k ? "QC 밑·적재 중" : "at QC · loading") : (k ? "야드 도착·RTG 인계" : "at yard · RTG handover");
  }
  if (d === "wait_rtg") return k ? "야드 도착·RTG 대기" : "at yard · waiting RTG";
  if (d === "delivering") return jobtype === "LD" ? (k ? "블록→QC 운반 중" : "block→QC carrying") : (k ? "QC→야드 운반 중" : "QC→yard carrying");
  if (d === "empty_travel") return jobtype === "LD" ? (k ? "블록으로 공차 이동" : "→ block (empty)") : (k ? "QC로 공차 이동" : "→ QC (empty)");
  if (d === "staging") return k ? "배차·대기" : "staging";
  if (d === "idle") return k ? "유휴" : "idle";
  return ko(lang) ? DSP_META[d]?.ko ?? null : DSP_META[d]?.en ?? null;
}

function LiveDispatchPool({ lang, snap, err }: { lang: Lang; snap: Snap | null; err: boolean }) {
  const k = ko(lang);
  const tts = ((snap?.devices ?? []) as LiveTT[]).filter((d) => d.cls === "TT");
  // GPS 침묵(120초+)인데 매처가 풀에 유지 중인 후보 — devices에 없는 트럭이라 별도 채널로 온다.
  const heldKind = new Map<string, "anchored" | "held">();
  const heldRows: LiveTT[] = (snap?.stage2_held ?? [])
    .filter((h) => !tts.some((d) => d.id === h.id))
    .map((h) => {
      heldKind.set(h.id, h.anchored ? "anchored" : "held");
      return { id: h.id, cls: "TT", dispatch: "soon_idle", jobtype: h.jobtype, free_in_s: h.free_in_s } as LiveTT;
    });
  // 후보 = 보이는 후보 상태 + 침묵 유지. 자유가 빠른 순(= 매처 비용 기저 순)으로 한 줄로 세운다.
  const cands = tts
    .filter((d) => d.in_pool ?? (CANDIDATE_STATES as readonly string[]).includes(d.dispatch ?? ""))
    .concat(heldRows)
    .sort((a, b) => freeInOf(a) - freeInOf(b) || a.id.localeCompare(b.id));
  const idleN = cands.filter(isIdleNow).length;
  const soonN = cands.length - idleN;  // 곧 자유 = 곧유휴 + RTG대기 + 침묵 유지 (한 갈래로 센다·위 주석 참조)
  const busyN = tts.length - (cands.length - heldRows.length); // 매처가 이번 틱에 건너뛴 차량(보이는 것만) = 자유까지 15분 넘게 남은 트럭
  const empties = tts.filter((d) => d.dispatch === "empty_travel");
  // swap pool: empty trucks still far enough from their pickup, EXCLUDING yard moves (MI/MO)
  // — only vessel work (DS/LD) is swappable. Distance threshold is operator-adjustable.
  const [swapMinM, setSwapMinM] = useState(500);
  const isYardMove = (d: LiveTT) => ["MI", "MO"].includes((d.jobtype ?? "").toUpperCase());
  const swap = empties
    .filter((d) => !isYardMove(d) && (d.dest_remaining_m ?? 0) >= swapMinM)
    .sort((a, b) => (b.dest_remaining_m ?? 1e9) - (a.dest_remaining_m ?? 1e9));
  const swapExcluded = empties.filter((d) => !isYardMove(d)).length - swap.length;
  const ageS = snap?.as_of ? Math.max(0, Math.round((Date.now() - Date.parse(snap.as_of)) / 1000)) : null;
  // truly live = backend reports connected AND the snapshot is fresh (guards a stale "LIVE" pill that
  // keeps the last connected:true snapshot after the feed dies).
  const liveFresh = !!snap?.connected && (ageS == null || ageS <= 120);

  return (
    <section className="tcard lvp">
      <div className="tcard-head">
        <h3>{k ? "후보 차량 풀" : "Candidate TT Pool"}
          <span className="h3-sub">{k ? "websocket GPS/PLC · 지금 배차 대상이 되는 차량(공급)" : "websocket GPS/PLC · vehicles the matcher can dispatch (supply)"}</span></h3>
        <div className="head-sub">
          <span className={`pill ${liveFresh ? "good" : "bad"}`}><span className="dot" />{!snap && !err ? "…" : liveFresh ? "LIVE" : (k ? "정지" : "STALE")}</span>
          <span className="muted mono" style={ageS != null && ageS > 120 ? { color: "#fca5a5", fontWeight: 700 } : undefined}>{ageS != null ? `⟳ ${ageS}s` : ""}</span>
        </div>
      </div>
      <div className="tcard-body">
        {/* 후보 총계를 먼저 크게 — 매처가 이번 틱에 쓸 수 있는 차량 수가 이 카드의 헤드라인이다. */}
        <div className="lvp-stats lvp-stats4">
          <div className="lvp-stat lvp-stat-hero" title={k ? `매처가 이번 틱 풀에 넣은 차량 = 이미 빈 트럭(원천 크레인 로그로 확인) + 15분 안에 빌 트럭 (GPS 침묵 ${heldRows.length}대 포함)` : `vehicles in the matcher's pool this tick = confirmed free (crane log) + freeing within 15 min (incl. ${heldRows.length} GPS-silent)`}>
            <div className="lvp-n">{cands.length}</div>
            <div className="lvp-l">{k ? "후보 차량" : "Candidates"}</div>
          </div>
          <div className="lvp-stat" style={{ borderTopColor: DSP_META.idle.color }} title={k ? "대기 없이 지금 보낼 수 있다(비용 기저 0초)" : "dispatchable with no wait (cost base 0s)"}>
            <div className="lvp-n">{idleN}</div>
            <div className="lvp-l">{k ? "지금 유휴" : "Idle now"}</div>
          </div>
          <div className="lvp-stat" style={{ borderTopColor: DSP_META.soon_idle.color }} title={k ? `아직 작업 중이지만 곧 자유 — 자유까지 예측 시간이 비용에 더해진다. 안벽 핸드오버·RTG 대기를 갈라 세지 않는다(ADR 0002). 시간 출처: 양하는 그 트립의 무브로그 앵커 우선, 정차면 정차 중앙값, 이동 중이면 학습값. GPS 침묵이어도 무브로그가 '일하는 중'이라 하면 풀에 유지된다(지금 ${heldRows.length}대)` : `still working but soon free — predicted time-to-free is added to the cost. Quay handover vs RTG wait are NOT counted separately (ADR 0002). Time source: DS uses this trip's move-log anchor first, stationary median when stopped, learned values while moving. GPS-silent trucks stay in the pool while the move-log says they're working (${heldRows.length} now)`}>
            <div className="lvp-n">{soonN}</div>
            <div className="lvp-l">{k ? "곧 자유" : "Soon free"}</div>
          </div>
          <div className="lvp-stat lvp-stat-mute" title={k ? "운반 중·배차 대기·공차 — 매처가 건너뛰는 차량" : "delivering / staging / empty — skipped by the matcher"}>
            <div className="lvp-n">{busyN}</div>
            <div className="lvp-l">{k ? "작업 중 (제외)" : "Busy (skipped)"}</div>
          </div>
        </div>
        <div className="lvp-cols">
          <div className="lvp-col">
            <div className="lvp-col-h">{k ? "후보 — 자유가 빠른 순" : "Candidates — soonest free first"}<span className="lvp-cn">{cands.length}</span></div>
            <div className="lvp-sub">{k
              ? "매처가 마지막 틱(60초 주기)에 실제로 쓴 값 그대로 — 지금 자유 = 0초, 나머지는 자유까지 예측 시간을 비용에 더함"
              : "the exact numbers the matcher used last tick (60s cadence) — free now = 0s; others add predicted time-to-free to the cost"}</div>
            <div className="lvp-list lvp-list-tall">
              {cands.length === 0 && <div className="lvp-empty">{k ? "없음" : "none"}</div>}
              {cands.map((d) => {
                const meta = DSP_META[d.dispatch ?? ""] ?? null;
                const now = d.dispatch === "idle";
                const hk = heldKind.get(d.id); // GPS 침묵 유지 트럭 (stage2_held)
                return (
                  <div className="lvp-row" key={d.id}>
                    <span className="sw" style={{ background: meta?.color ?? "var(--text-mute)", opacity: hk ? 0.45 : undefined }} title={hk ? (k ? "GPS 침묵 — 매처가 무브로그 근거로 풀에 유지" : "GPS silent — held in the pool on move-log evidence") : dspTitle(d.dispatch, lang)} />
                    <span className="lvp-id mono">{d.id}</span>
                    {d.jobtype && <span className={`lvp-job type-${d.jobtype.toLowerCase()}`}>{d.jobtype}</span>}
                    {d.topos1 && <span className="lvp-dest mono">→{d.topos1}</span>}
                    {now
                      ? <span className="lvp-freein lvp-now">{k ? "지금" : "now"}</span>
                      : freeInLabel(d, lang) && <span className="lvp-freein" title={k ? "매처가 마지막 틱에 쓴 자유까지 시간(양하는 무브로그 앵커 우선·정차는 정차 중앙값·60초 주기 갱신)" : "time-to-free the matcher used last tick (DS: move-log anchor first; stopped: stationary median; refreshed every 60s)"}>{freeInLabel(d, lang)}</span>}
                    <span className="lvp-why">{hk
                      ? (freeInOf(d) <= 0
                          ? (k ? "GPS 침묵 · 원천 로그로 이미 빈 것 확인" : "GPS silent · confirmed free by crane log")
                          : hk === "anchored"
                            ? (k ? "GPS 침묵 · 무브로그 앵커로 유지" : "GPS silent · held by move-log anchor")
                            : (k ? "GPS 침묵 · 드랍 근접 대기로 유지" : "GPS silent · held waiting at its drop"))
                      : soonWhy(d, lang)}</span>
                  </div>
                );
              })}
            </div>
          </div>
          <div className="lvp-col">
            <div className="lvp-col-h"><span className="sw" style={{ background: DSP_META.empty_travel.color }} />{k ? "스왑 후보 (공차)" : "Swap candidates (empty)"}<span className="lvp-cn">{swap.length}</span></div>
            <div className="lvp-sub">{k
              ? `참고용 — 위 후보와 별개다(매처 풀에 안 들어감). 픽업까지 ≥${swapMinM}m · MI/MO 제외 · 기준미달 ${swapExcluded} 제외`
              : `reference only — NOT in the matcher pool. ≥${swapMinM}m to pickup · MI/MO excluded · ${swapExcluded} below threshold`}</div>
            <div className="lvp-swapctl">
              <span className="lvp-swapctl-l">{k ? "기준 거리" : "min dist"}</span>
              <input type="range" min={100} max={1500} step={50} value={swapMinM} onChange={(e) => setSwapMinM(Number(e.target.value))} />
              <span className="lvp-swapctl-v mono">{swapMinM}m</span>
            </div>
            <div className="lvp-list lvp-list-tall">
              {swap.length === 0 && <div className="lvp-empty">{k ? "없음" : "none"}</div>}
              {swap.map((d) => (
                <div className="lvp-row" key={d.id}>
                  <span className="lvp-id mono">{d.id}</span>
                  {d.jobtype && <span className={`lvp-job type-${d.jobtype.toLowerCase()}`}>{d.jobtype}</span>}
                  {d.topos1 && <span className="lvp-dest mono">→{d.topos1}</span>}
                  <span className="lvp-why">{d.dest_remaining_m != null ? (k ? `잔여 ${Math.round(d.dest_remaining_m)}m` : `${Math.round(d.dest_remaining_m)}m left`) : (k ? "목적지 학습 중" : "dest learning")}</span>
                </div>
              ))}
            </div>
          </div>
        </div>
        <div className="lvp-note">{k
          ? "후보 = 지금 유휴 + 곧 자유. 운반 중·배차 대기·공차는 이미 일이 있어 제외한다. 자유까지 시간은 매처가 마지막 틱(60초 주기)에 실제로 쓴 값이다 — 양하는 그 트립의 크레인 픽업부터 내려 세는 무브로그 앵커를 먼저 쓰고, 없으면 정차 중앙값(작업유형별) → 학습값 → 상수 순서로 폴백한다. 개별 트럭의 남은 시간을 정밀하게 맞히는 일은 관측 신호의 근본 한계로 중단했다(대기를 만드는 크레인 큐가 안 보이고, 트럭은 멈추면 단말이 침묵한다). 다만 GPS가 120초 넘게 침묵해도 무브로그가 '아직 일하는 중'이라 하면 매처는 그 트럭을 풀에 유지하며, 그런 트럭은 여기 'GPS 침묵'으로 표시된다(지도에는 없음)."
          : "Candidates = idle now + soon free. Delivering / staging / empty trucks already have work and are skipped. Time-to-free is the value the matcher actually used on its last tick (60s cadence) — DS counts down from this trip's own crane pickup (move-log anchor) when available, else stationary median (per job type) → learned values → constants. Predicting an individual truck's remaining time precisely was discontinued as an observability limit (the crane queue that creates the wait is invisible, and devices go silent while stopped). Trucks silent for 120s+ are still HELD in the pool while the move-log says they're working — they appear here flagged 'GPS silent' (absent from the map)."}</div>
      </div>
    </section>
  );
}

// ═══════════════════════ QC 작업 실황 (2026-08-10 전면 재작성) ═══════════════════════
// 축 = 현행 배차 로직 그대로:
//   · 순서   = 크레인 타임라인(timeline_pos: 활성 선박 블록 → 마감 이른 블록, 블록 안 seq)
//              × 구역 안 적부계획 순번(slot_idx, mig 0128 — TOS ITV 배차기와 같은 기준)
//   · 긴급도 = 매처와 같은 티어(버킷 여유 = (완료기한−now)−처리시간; <0 늦음 · <30분 빠듯)
//   · 마감   = 상자별 출항 요구 페이스 균등 배분(dispatch_deadline_ts, 백엔드 단일 계산)
// 다선박 크레인이 기본(실측 49중 33)이라 화면을 (선박 × QC) 칸으로 가른다 — 진행률·트럭
// 수·여유가 다른 배와 섞이지 않는다(옛 카드의 결함). ETW 는 순서 권위가 아니라 참고 칩.

type WpBox = WpBoxDeadline;
// 매처의 DEP_TIGHT_S(livemap.rs)와 동일 — 화면 신호등이 매처 티어와 같은 잣대가 되게 한다.
const TIGHT_S = 1800;

function bucketSlackS(q: WpQueue, nowMs: number): number | null {
  if (!q.deadline_ts || q.proc_s == null) return null;
  return Math.round((Date.parse(q.deadline_ts) - nowMs) / 1000) - q.proc_s;
}
const tierLight = (s: number | null) => (s == null ? "⚪" : s < 0 ? "🔴" : s < TIGHT_S ? "🟡" : "🟢");

// (선박 × QC) 칸 — 이 선박의 큐·상자·트럭만 담는다.
type QcView = {
  qc: WpQc; vessel: string; active: boolean;      // active = 크레인이 지금 이 배를 작업 중
  queues: WpQueue[];                              // 이 선박 것만, 크레인 타임라인 순
  boxesByQueue: Map<string, WpBox[]>;             // queuename → 적부계획 슬롯 순
  moveByCont: Map<string, WpMove>;                // 상자 → 작업지시 행(트럭·ETW·ACTV)
  trucked: number; minSlack: number | null; overdueN: number;
  comp: number; tot: number; mph: number | null;
};
type VesselView = {
  vessel: string; dep: string | null;
  comp: number; tot: number; minSlack: number | null; overdueN: number;
  qcs: QcView[];
};

function buildVesselViews(wp: WorkpoolResponse | null, snap: Snap | null, nowMs: number): VesselView[] {
  if (!wp) return [];
  const mphByQc = new Map<string, number>();
  for (const d of snap?.devices ?? []) if (d.plc?.mph != null && d.plc.mph > 0) mphByQc.set(d.id, d.plc.mph);
  const boxesByKey = new Map<string, WpBox[]>();
  for (const b of wp.box_deadlines ?? []) {
    const key = `${b.vessel}|${b.qc}|${b.queuename}`;
    const a = boxesByKey.get(key);
    if (a) a.push(b); else boxesByKey.set(key, [b]);
  }
  for (const a of boxesByKey.values()) a.sort((x, y) => (x.slot_idx ?? 9e9) - (y.slot_idx ?? 9e9) || x.contno.localeCompare(y.contno));

  const views: VesselView[] = [];
  const byVessel = new Map<string, VesselView>();
  for (const q of wp.qcs) {
    const moveByCont = new Map<string, WpMove>();
    for (const m of q.moves) if (m.contno) moveByCont.set(m.contno, m);
    for (const vessel of new Set(q.queues.map((b) => b.vessel))) {
      const queues = q.queues
        .filter((b) => b.vessel === vessel)
        .sort((a, b) => (a.timeline_pos ?? 9e9) - (b.timeline_pos ?? 9e9) || (a.seq ?? 9e9) - (b.seq ?? 9e9));
      const comp = queues.reduce((a, b) => a + b.done, 0);
      const tot = queues.reduce((a, b) => a + b.total, 0);
      const boxesByQueue = new Map<string, WpBox[]>();
      let overdueN = 0;
      for (const b of queues) {
        const arr = boxesByKey.get(`${vessel}|${q.qc}|${b.queuename}`) ?? [];
        if (arr.length) boxesByQueue.set(b.queuename, arr);
        for (const x of arr) if (!x.tos_assigned && x.dispatch_deadline_ts && Date.parse(x.dispatch_deadline_ts) < nowMs) overdueN++;
      }
      if (tot - comp <= 0 && boxesByQueue.size === 0) continue; // 남은 일도 발행 상자도 없음
      const slacks = queues.map((b) => bucketSlackS(b, nowMs)).filter((s): s is number => s != null);
      const trucked = new Set(
        q.moves
          .filter((m) => m.vessel === vessel && m.ytno && m.ytno.trim() && !(m.jobtype === "DS" && m.actv_ts))
          .map((m) => (m.ytno as string).trim()),
      ).size;
      const qv: QcView = {
        qc: q, vessel, active: (q.vessels[0] ?? null) === vessel,
        queues, boxesByQueue, moveByCont, trucked,
        minSlack: slacks.length ? Math.min(...slacks) : null,
        overdueN, comp, tot, mph: mphByQc.get(q.qc) ?? null,
      };
      let vv = byVessel.get(vessel);
      if (!vv) {
        vv = { vessel, dep: null, comp: 0, tot: 0, minSlack: null, overdueN: 0, qcs: [] };
        byVessel.set(vessel, vv);
        views.push(vv);
      }
      vv.qcs.push(qv);
      vv.comp += comp; vv.tot += tot; vv.overdueN += overdueN;
      if (qv.minSlack != null && (vv.minSlack == null || qv.minSlack < vv.minSlack)) vv.minSlack = qv.minSlack;
      if (qv.active && q.estdep_ts) vv.dep = q.estdep_ts;
    }
  }
  for (const vv of views) vv.qcs.sort((a, b) => Number(b.active) - Number(a.active) || a.qc.qc.localeCompare(b.qc.qc, undefined, { numeric: true }));
  // 급한 배 먼저 — 페이지 전체가 같은 순서를 쓴다(보드에서 본 순서 = 아래 상세 순서).
  views.sort((a, b) => (a.minSlack ?? 9e9) - (b.minSlack ?? 9e9) || a.vessel.localeCompare(b.vessel));
  return views;
}

function useStage2(ms = 15000) {
  const [advisory, setAdvisory] = useState<Stage2Advisory[]>([]);
  const [picks, setPicks] = useState<ComparePick[]>([]);
  useEffect(() => {
    let alive = true;
    const poll = () => {
      api.stage2Advisory().then((d) => { if (alive) setAdvisory(d); }).catch(() => {});
      api.stage2ComparePicks().then((d) => { if (alive) setPicks(d); }).catch(() => {});
    };
    poll();
    const iv = setInterval(poll, ms);
    return () => { alive = false; clearInterval(iv); };
  }, [ms]);
  return { advisory, picks };
}

// workpool 미러 신선도 — 낡으면 카드 전체를 FROZEN 으로 크게 표시(자체 신호·GPS와 독립).
function useWpAge(wp: WorkpoolResponse | null, k: boolean) {
  const ageS = wp?.as_of ? Math.max(0, Math.round((Date.now() - Date.parse(wp.as_of)) / 1000)) : null;
  const stale = ageS != null && ageS > 300;
  const txt = ageS == null ? "" : ageS >= 86400 ? `${Math.floor(ageS / 86400)}${k ? "일" : "d"}` : ageS >= 3600 ? `${Math.floor(ageS / 3600)}${k ? "시간" : "h"}` : ageS >= 60 ? `${Math.floor(ageS / 60)}${k ? "분" : "m"}` : `${ageS}${k ? "초" : "s"}`;
  return { ageS, stale, txt };
}

// ───────────────────────── ① 배차 압력 보드 ─────────────────────────
// "어디가 급한가"를 한 눈에: 선박(급한 순) × QC 칸. 큰 숫자 = 마감 경과·미배차 상자
// (= 매처가 이번 틱에 담는 바로 그 목록). 신호등 = 매처 티어와 같은 잣대.
function PressureBoard({ lang, wp, views }: { lang: Lang; wp: WorkpoolResponse | null; views: VesselView[] }) {
  const k = ko(lang);
  const { ageS, stale, txt } = useWpAge(wp, k);
  const totOverdue = views.reduce((a, v) => a + v.overdueN, 0);
  const totTrucked = views.reduce((a, v) => a + v.qcs.reduce((s, x) => s + x.trucked, 0), 0);
  const jump = (id: string) => document.getElementById(id)?.scrollIntoView({ behavior: "smooth", block: "center" });
  return (
    <section className={`tcard${stale ? " wp-stale" : ""}`}>
      <div className="tcard-head">
        <h3>{k ? "배차 압력" : "Dispatch Pressure"}
          <span className="h3-sub">{k ? "선박(급한 순) × QC — 🔴 마감 경과(미배차) · 배차 트럭 · 실시간 처리속도" : "vessels (most urgent first) × QC — 🔴 overdue unassigned · trucks · live rate"}</span></h3>
        <div className="head-sub">
          {stale && <span className="pill bad" title={wp?.as_of ?? ""}>⚠ {k ? `정지 · ${txt} 전 (라이브 아님)` : `FROZEN · ${txt} old`}</span>}
          {totOverdue > 0 && <span className="pill bad">🔴 {k ? `마감 경과 ${totOverdue}` : `${totOverdue} overdue`}</span>}
          <span className="muted">{k ? `배차 ${totTrucked}대` : `${totTrucked} trucks`}</span>
          <span className="muted mono">{ageS != null ? `⟳ ${stale ? txt : `${ageS}s`}` : ""}</span>
        </div>
      </div>
      <div className="tcard-body">
        {views.length === 0 && <div className="lvp-empty">{k ? "작업 없음" : "no work"}</div>}
        <div className="qca-cols">
          {views.map((v) => {
            const vpct = v.tot > 0 ? Math.round((v.comp / v.tot) * 100) : 0;
            return (
              <div className="qca-vgroup" key={v.vessel}>
                <div className="qc-vgroup-h">
                  <span className="vsl">{v.vessel}</span>
                  <span className="qca-light">{tierLight(v.minSlack)}</span>
                  {v.dep && <span className="vgroup-dep" title={k ? "출항 예정 (괄호=남은 시간)" : "departure (paren = left)"}>🏁 <span className="mono">{dayClockOf(v.dep, k)}</span> <span className="qc-dep-left">({relDurOf((Date.parse(v.dep) - Date.now()) / 1000, k)})</span></span>}
                  <span className="qc-vgroup-n">{v.qcs.length} QC</span>
                </div>
                <div className="qc-vbar" title={k ? "이 선박 큐만 합산한 진행(완료/전체 컨테이너, TOS 카운터). 완료 큐는 ~6시간 뒤 목록에서 내려가 오래 접안한 배는 낮게 보일 수 있다" : "this vessel's queues only (done/total containers, TOS counters). Finished queues drop out after ~6h"}>
                  <div className="qc-vbar-txt"><span>{k ? "진행" : "prog"} {vpct}%</span><span className="mono">{v.comp.toLocaleString()} / {v.tot.toLocaleString()}</span></div>
                  <div className="qc-vbar-track"><div className="fill" style={{ width: `${vpct}%` }} /></div>
                </div>
                <div className="qca-grid">
                  {v.qcs.map((x) => (
                    <div className="qca-cell clickable" key={x.qc.qc} onClick={() => jump(`qccol-${v.vessel}-${x.qc.qc}`)}
                      title={`${x.qc.qc} · ${v.vessel}${x.active ? "" : (k ? " · 이 배는 이 크레인의 다음 순서" : " · next vessel for this crane")} — ${k ? "클릭=상세로" : "click for detail"}`}>
                      <div className="qca-qc">{x.qc.qc} <span className="qca-light">{tierLight(x.minSlack)}</span>{!x.active && <span className="muted"> {k ? "대기" : "next"}</span>}</div>
                      <div className="qca-n" style={{ color: x.overdueN > 0 ? "#ef4444" : "#22c55e" }}>{x.overdueN}<small>{k ? "경과" : "od"}</small></div>
                      <div className="qca-vsl">{k ? `배차 ${x.trucked}대` : `${x.trucked} trk`}{x.mph != null ? ` · ⚡${x.mph}/h` : ""}</div>
                    </div>
                  ))}
                </div>
              </div>
            );
          })}
        </div>
        <div className="lvp-note">{k
          ? "마감 경과 = 배차 마감(출항 요구 페이스 균등 배분)이 지났는데 TOS가 아직 트럭을 안 붙인 상자 수 — 매처가 이번 틱에 담는 바로 그 목록이다. 신호등은 매처와 같은 잣대(가장 급한 베이의 여유: 🔴 음수 = 지금 속도로는 완료기한을 못 지킴 · 🟡 30분 미만 · ⚪ 출항 정보 없음)."
          : "Overdue = boxes past their dispatch deadline (even departure-pace allocation) that TOS hasn't assigned yet — exactly the list the matcher pools this tick. Lights use the matcher's own tiers (most-urgent bay slack: 🔴 negative · 🟡 <30min · ⚪ no schedule)."}</div>
      </div>
    </section>
  );
}

// ───────────────────────── ② QC 작업 타임라인 ─────────────────────────
type OurPick = { ytno: string; arrival_s: number | null; kind: "live" | "cmp"; agree?: boolean | null; delta_s?: number | null };

function QcTimeline({ lang, wp, snap, views, advisory, picks }: {
  lang: Lang; wp: WorkpoolResponse | null; snap: Snap | null; views: VesselView[];
  advisory: Stage2Advisory[]; picks: ComparePick[];
}) {
  const k = ko(lang);
  const { ageS, stale, txt } = useWpAge(wp, k);
  const [showOurs, setShowOurs] = useState(true);
  const [showAllUn, setShowAllUn] = useState(false);
  // ⚠ 조회용 Map 들은 전부 useMemo — 페이지가 1초 틱으로 재렌더되므로, 여기서 매 렌더마다
  // 재구축하면 그게 곧 초당 CPU/GC 부하가 된다(2026-08-10 브라우저 과부하 사고의 절반).
  const { ttState, craneFresh } = useMemo(() => {
    const ttState = new Map<string, Dev>();
    const craneFresh = new Map<string, boolean>();
    for (const d of snap?.devices ?? []) {
      if (d.cls === "TT") ttState.set(d.id, d);
      else if (d.plc) craneFresh.set(d.id, (d.plc.age_s ?? 999) <= 120);
    }
    return { ttState, craneFresh };
  }, [snap]);
  // 🤖 추천 신선도: advisory 는 매처 '마지막 틱' 행 — 매처가 멈추면 낡은 추천이 남으므로 나이를 재고 180초 넘으면 붙이지 않는다.
  const advTs = advisory[0]?.ts ? Date.parse(advisory[0].ts as string) : null;
  const advAgeS = advTs != null ? Math.max(0, Math.round((Date.now() - advTs) / 1000)) : null;
  const advFresh = advisory.length > 0 && (advAgeS == null || advAgeS <= 180);
  const { advByCont, advLoose } = useMemo(() => {
    const advByCont = new Map<string, Stage2Advisory>();
    const advLoose = new Map<string, Stage2Advisory[]>(); // contno 없는 옛 형식 폴백(QC별)
    if (advFresh) for (const r of advisory) {
      if (r.contno) advByCont.set(r.contno, r);
      else if (r.qc) { const a = advLoose.get(r.qc) ?? []; a.push(r); advLoose.set(r.qc, a); }
    }
    return { advByCont, advLoose };
  }, [advisory, advFresh]);
  const picksByKey = useMemo(() => {
    const m = new Map<string, ComparePick>();
    for (const p of picks) m.set(`${p.qc}|${p.queuename}|${p.tos_ytno}`, p);
    return m;
  }, [picks]);
  return (
    <section className={`tcard${stale ? " wp-stale" : ""}`}>
      <div className="tcard-head">
        <h3>{k ? "QC 작업 타임라인" : "QC Work Timeline"}
          <span className="h3-sub">{k ? "크레인이 일할 순서(적부계획 축) · 상자별 배차 마감 · 배차 상태 · 🤖 우리 추천" : "crane order (stow-plan axis) · per-box dispatch deadline · assignment · 🤖 our pick"}</span></h3>
        <div className="head-sub">
          {stale && <span className="pill bad" title={wp?.as_of ?? ""}>⚠ {k ? `정지 · ${txt} 전 데이터` : `FROZEN · ${txt} old`}</span>}
          {showOurs && advAgeS != null && !advFresh && <span className="pill bad" title={k ? "매처 마지막 추천이 3분 넘게 낡음 — 🤖 표시는 생략" : "matcher advisory stale >3min — 🤖 hidden"}>🤖 {k ? `추천 정지 ${Math.floor(advAgeS / 60)}분` : `stale ${Math.floor(advAgeS / 60)}m`}</span>}
          <label className="qc-pastsel" title={k ? "미배차 상자를 구역당 3개까지만 접어 보일지, 발행분 전부 보일지" : "show 3 unassigned per bay, or all issued"}>
            <input type="checkbox" checked={showAllUn} onChange={(e) => setShowAllUn(e.target.checked)} /> {k ? "미배차 전부" : "all unassigned"}
          </label>
          <label className="qc-pastsel" title={k ? "모든 상자에 🤖 우리 배차 표시(TOS 배정 행엔 '우리라면 누구', 같으면 ✓)" : "🤖 our dispatch on every box (✓ if same as TOS)"}>
            <input type="checkbox" checked={showOurs} onChange={(e) => setShowOurs(e.target.checked)} /> 🤖
          </label>
          <span className="muted mono" style={stale ? { color: "#fca5a5", fontWeight: 700 } : undefined}>{ageS != null ? `⟳ ${stale ? txt : `${ageS}s`}` : ""}</span>
        </div>
      </div>
      <div className="tcard-body">
        {views.length === 0 && <div className="lvp-empty">{k ? "작업 없음" : "no work"}</div>}
        {views.map((v) => (
          <div className="qc-vgroup" key={v.vessel}>
            <div className="qc-vgroup-h">
              <span className="vsl">{v.vessel}</span>
              <span className="qca-light">{tierLight(v.minSlack)}</span>
              {v.dep && <span className="vgroup-dep">🏁 {k ? "출항" : "dep"} <span className="mono">{dayClockOf(v.dep, k)}</span> <span className="qc-dep-left">({relDurOf((Date.parse(v.dep) - Date.now()) / 1000, k)} {k ? "남음" : "left"})</span></span>}
              <span className="qc-vgroup-n">{v.qcs.length} QC</span>
            </div>
            <div className="qc-panel">
              {v.qcs.map((x) => (
                <QcColV2 key={`${v.vessel}-${x.qc.qc}`} x={x} lang={lang} ttState={ttState}
                  working={craneFresh.get(x.qc.qc) ?? false} showOurs={showOurs} showAllUn={showAllUn}
                  advByCont={advByCont} advLoose={advLoose} picksByKey={picksByKey} />
              ))}
            </div>
          </div>
        ))}
        <div className="lvp-note">{k
          ? "순서는 적부계획(TOS의 ITV 배차기가 쓰는 그 순번) 축이고, 상자별 마감은 출항 요구 페이스 균등 배분(백엔드 단일 계산)이다. '발행 n / 남은 m'에서 발행은 TOS가 작업지시를 만든 상자(작업 ~1시간 전에야 만든다) — 나머지는 아직 지시가 없을 뿐 계획에는 있다. 크레인이 이미 처리한 양하 상자(트럭이 야드로 운반 중)는 여기가 아니라 오른쪽 차량 카드에 있다. '다음≈'는 계획 순서 기준 근사다 — 크레인은 계획을 88% 따르지만 이웃 상자끼리는 맞바꾸기도 한다."
          : "Order = the stow-plan axis (the same rank TOS's own ITV dispatcher sorts by); per-box deadlines = even departure-pace allocation (backend-computed). In 'issued n / left m', issued = boxes TOS has cut orders for (~1h ahead) — the rest are planned but not yet issued. DS boxes the crane already handled (truck en route to yard) live in the fleet card, not here. '다음≈' is a plan-order approximation — cranes follow the plan 88% but may swap neighbours."}</div>
      </div>
    </section>
  );
}

function QcColV2({ x, lang, ttState, working, showOurs, showAllUn, advByCont, advLoose, picksByKey }: {
  x: QcView; lang: Lang; ttState: Map<string, Dev>; working: boolean; showOurs: boolean; showAllUn: boolean;
  advByCont: Map<string, Stage2Advisory>; advLoose: Map<string, Stage2Advisory[]>; picksByKey: Map<string, ComparePick>;
}) {
  const k = ko(lang);
  const pct = x.tot > 0 ? Math.round((x.comp / x.tot) * 100) : 0;
  // '다음≈' = 타임라인상 처음으로 남은 일이 있는 **발행된** 베이의 첫 슬롯(발행 전 베이는 접힘).
  const firstRemQ = x.queues.find((q) => q.remaining > 0 && x.boxesByQueue.has(q.queuename))?.queuename ?? null;
  // 🤖 배정: contno 정확 일치 우선, 없으면(옛 형식) QC별 같은 방향 첫-적합 폴백.
  const loosePool = (advLoose.get(x.qc.qc) ?? []).slice();
  const pickFor = (b: WpBox): OurPick | null => {
    if (!showOurs) return null;
    if (b.tos_assigned) {
      const m = x.moveByCont.get(b.contno) ?? b.contnos.map((c) => x.moveByCont.get(c)).find(Boolean);
      const yt = m?.ytno?.trim();
      if (!yt) return null;
      const p = picksByKey.get(`${x.qc.qc}|${b.queuename}|${yt}`);
      return p?.our_ytno ? { ytno: p.our_ytno, arrival_s: p.our_arrival_s, kind: "cmp", agree: p.agree, delta_s: p.delta_s } : null;
    }
    const exact = advByCont.get(b.contno) ?? b.contnos.map((c) => advByCont.get(c)).find(Boolean);
    if (exact) return { ytno: exact.ytno, arrival_s: exact.arrival_s, kind: "live" };
    const i = loosePool.findIndex((r) => (r.jobtype ?? null) === b.jobtype);
    if (i >= 0) { const r = loosePool.splice(i, 1)[0]; return { ytno: r.ytno, arrival_s: r.arrival_s, kind: "live" }; }
    return null;
  };
  const fmtEta = (s: number) => `${Math.floor(s / 60)}:${String(Math.round(s % 60)).padStart(2, "0")}`;
  return (
    <div className="qc-col" id={`qccol-${x.vessel}-${x.qc.qc}`}>
      <div className="qc-head">
        <span className={`id ${working ? "busy" : "idle"}`}><span className="dot" />{x.qc.qc}
          <span className="qc-vessel">{x.vessel}{x.active ? "" : (k ? " · 다음 순서" : " · next")}</span></span>
        {x.mph != null
          ? <span className="mph" title={k ? "PLC 실시간 처리량 (최근 1시간 무브)" : "live PLC throughput (moves/h)"}>⚡<span className="v">{x.mph}</span>/h</span>
          : <span className="mph">{k ? "남은" : "left"} <span className="v">{x.tot - x.comp}</span></span>}
      </div>
      <div className="qc-progress"><span>{x.trucked} {k ? "배차중" : "trucked"}{working ? (k ? " · PLC 가동" : " · PLC live") : ""}</span><span className="mono">{x.comp.toLocaleString()} / {x.tot.toLocaleString()}</span></div>
      <div className="qc-progress-bar"><div className="fill" style={{ width: `${pct}%` }} /></div>
      {x.minSlack != null && (() => {
        const s = x.minSlack;
        const a = Math.abs(Math.round(s / 60));
        const hm = `${Math.floor(a / 60)}h ${a % 60}m`;
        return (
          <div className="qc-deadline" title={k ? "이 배 몫 중 가장 급한 베이의 여유 = (그 베이 완료기한 − 지금) − 그 베이 처리시간. 매처의 티어 산식 그대로" : "most-urgent bay slack = (bay finish-by − now) − bay work; the matcher's own tier formula"}>
            <span className={`qc-slack ${s < 0 ? "late" : s < TIGHT_S ? "tight" : "ok"}`}>{tierLight(s)} {hm} {s < 0 ? (k ? "부족" : "short") : (k ? "여유" : "slack")}</span>
          </div>
        );
      })()}
      <div className="qc-seqlabel">{k ? "작업 (계획 순서)" : "work (plan order)"}</div>
      {/* 상자(지시)가 발행된 베이만 블록으로 그린다. 발행 전 베이는 맨 아래 한 줄 요약 —
          실측 큐 블록 490중 447개가 상자 없는 미래 베이였고, 그 헤더·푸터 DOM이 스크롤을
          끌고 내려갔다(2026-08-10). 정보는 아래 요약(개수·잔여·최악 여유)으로 보존한다. */}
      {x.queues.filter((q) => x.boxesByQueue.has(q.queuename)).map((q) => {
        const boxes = x.boxesByQueue.get(q.queuename) ?? [];
        const bslack = bucketSlackS(q, Date.now());
        const jt = boxes[0]?.jobtype ?? (q.disload === "L" ? "LD" : q.disload === "D" ? "DS" : null);
        let hiddenUn = 0, shownUn = 0;
        const shown: WpBox[] = [];
        for (const b of boxes) {
          if (b.tos_assigned) { shown.push(b); continue; }
          if (showAllUn || shownUn < 3) { shown.push(b); shownUn++; } else hiddenUn++;
        }
        return (
          <div key={q.queuename}>
            <div className="qc-qdiv">
              <span className="mono">{q.queuename}</span>
              {jt && <span className="qc-qdiv-t">{jt === "DS" ? (k ? "양하" : "DSC") : (k ? "적하" : "LOD")}</span>}
              <span className="muted mono" style={{ marginLeft: "auto" }}>{q.done}/{q.total}</span>
              {bslack != null && <span className={`jetw ${bslack < 0 ? "bad" : bslack < TIGHT_S ? "warn" : "ok"}`} title={k ? "이 베이의 여유(완료기한 − 지금 − 처리시간)" : "bay slack"}>{tierLight(bslack)} {relDurOf(bslack, k)}</span>}
              {q.work_eta_ts && q.remaining > 0 && (() => {
                const s = (Date.parse(q.work_eta_ts!) - Date.now()) / 1000;
                return <span className="jetw lo" title={k ? "크레인이 이 베이에 도달하는 예측 시각(보정 없음·앞선 작업 합산)" : "predicted crane arrival at this bay"}>⛏ {s <= 0 ? (k ? "진행 중" : "now") : `~${relDurOf(s, k)}`}</span>;
              })()}
            </div>
            {shown.map((b) => {
              const m = x.moveByCont.get(b.contno) ?? b.contnos.map((c) => x.moveByCont.get(c)).find(Boolean);
              const yt = b.tos_assigned ? m?.ytno?.trim() : undefined;
              const tt = yt ? ttState.get(yt) : undefined;
              const dot = tt?.dispatch ? DSP_META[tt.dispatch]?.color : undefined;
              const isNext = x.active && q.queuename === firstRemQ && b === shown[0];
              const dlMs = b.dispatch_deadline_ts ? Date.parse(b.dispatch_deadline_ts) : null;
              const our = pickFor(b);
              return (
                <div className={`qc-task ${isNext ? "now" : "queued"}`} key={b.contno}>
                  <span className="seq" title={k ? "구역 안 계획 순번(적부계획)" : "plan slot in bay"}>{isNext ? (k ? "다음≈" : "next≈") : b.slot_idx != null ? `#${b.slot_idx + 1}` : "▸"}</span>
                  <div className="body">
                    <div className="top">
                      <span className={`type-${kindChip(b.jobtype)}`}>{kindLabel(b.jobtype)}</span>
                      <span className="cont" title={b.contnos.length > 1 ? `${k ? "트윈" : "twin"}: ${b.contnos.join(" + ")}` : b.contno}>{b.contno}{b.contnos.length > 1 ? " ×2" : ""}</span>
                    </div>
                    <div className="bot">
                      {dlMs != null && (() => {
                        const sec = Math.round((dlMs - Date.now()) / 1000);
                        const cls = sec < 120 ? "bad" : sec < 1800 ? "warn" : "ok";
                        return <span className={`jetw ${cls}`} title={k ? "배차 마감까지 남은 시간 — 출항 페이스 기준 이 상자를 트럭에 맡겨야 하는 시각(백엔드 계산). 빨강=지금" : "time to dispatch deadline (departure-pace, backend-computed); red = now"}>🏁 {clockDur(sec, k)}</span>;
                      })()}
                      {(() => { const e = m ? etwLabel(m.etw_accurate, m.etw_expires, lang) : null; return e && <span className={`jetw ${e.cls}`} title={k ? "TOS 작업예정(ETW) — 참고용 일정 추정(순서 권위 아님)" : "TOS ETW — schedule estimate (not the order authority)"}>⏱ {e.text}</span>; })()}
                    </div>
                  </div>
                  <div className="assign">
                    <span className="chips">
                      {yt
                        ? <span className="tt" title={dspTitle(tt?.dispatch, lang)}>{dot && <span className="dot" style={{ background: dot, marginRight: 4 }} />}{yt}</span>
                        : <span className="tt-none">{k ? "미배차" : "Unassigned"}</span>}
                      {our && (() => {
                        const agree = our.kind === "cmp" && our.agree === true;
                        const detail = our.kind === "cmp"
                          ? (agree ? "✓" : our.delta_s != null ? (our.delta_s > 0 ? `▲${fmtEta(our.delta_s)}` : `▼${fmtEta(-our.delta_s)}`) : "")
                          : (our.arrival_s != null ? fmtEta(our.arrival_s) : "");
                        const title = our.kind === "cmp"
                          ? (agree ? (k ? `우리 배차: ${our.ytno} (TOS와 동일)` : `ours: ${our.ytno} (same as TOS)`) : (k ? `우리 배차: ${our.ytno}${our.delta_s != null ? (our.delta_s > 0 ? ` · TOS보다 ${fmtEta(our.delta_s)} 빠름` : ` · ${fmtEta(-our.delta_s)} 느림`) : ""}` : `ours: ${our.ytno}`))
                          : (k ? `우리 배차: ${our.ytno}${our.arrival_s != null ? ` · 픽업 도착 ${fmtEta(our.arrival_s)}` : ""}` : `our dispatch: ${our.ytno}`);
                        return <span className={`tt-ours${agree ? " agree" : ""}`} title={title}>🤖 {our.ytno}{detail ? <span className="d">{detail}</span> : null}</span>;
                      })()}
                    </span>
                    {(() => { const w = ttWhere(tt, b.jobtype, lang); return w && <span className="tt-status" style={{ color: dot }}>{w}</span>; })()}
                  </div>
                </div>
              );
            })}
            <div className="qc-bay-more">{k
              ? `지시 발행 ${boxes.length} · 남은 ${q.remaining}${hiddenUn > 0 ? ` · 접힘 ${hiddenUn}` : ""}`
              : `issued ${boxes.length} · left ${q.remaining}${hiddenUn > 0 ? ` · folded ${hiddenUn}` : ""}`}</div>
          </div>
        );
      })}
      {(() => {
        // 발행 전 베이 요약 — 계획에는 있으나 TOS 지시가 아직 없는 뒤쪽 베이들.
        const rest = x.queues.filter((q) => q.remaining > 0 && !x.boxesByQueue.has(q.queuename));
        if (rest.length === 0) return null;
        const restRem = rest.reduce((a, b) => a + b.remaining, 0);
        const slacks = rest.map((b) => bucketSlackS(b, Date.now())).filter((s): s is number => s != null);
        const restMin = slacks.length ? Math.min(...slacks) : null;
        return (
          <div className="qc-bay-more" title={k ? "지시가 아직 발행되지 않은 베이(계획에는 있음). 신호등·여유 = 그중 가장 급한 베이 기준" : "bays with no issued orders yet (planned). Light/slack = worst among them"}>
            {tierLight(restMin)} {k ? `이후 ${rest.length}베이 · 남은 ${restRem}` : `+${rest.length} bays · ${restRem} left`}
            {restMin != null ? ` · ${relDurOf(restMin, k)} ${restMin < 0 ? (k ? "부족" : "short") : (k ? "여유" : "slack")}` : ""}
          </div>
        );
      })()}
    </div>
  );
}

// ───────────────────────── ③ 작업 중 차량 ─────────────────────────
// 매처가 건너뛰는(이미 일이 있는) 트럭들 — 공차 이동 · 운반 · 배차 대기. 후보 카드의 반대편.
function FleetCard({ lang, snap }: { lang: Lang; snap: Snap | null }) {
  const k = ko(lang);
  const ORDER: Record<string, number> = { empty_travel: 0, delivering: 1, staging: 2 };
  const fleet = ((snap?.devices ?? []) as Dev[])
    .filter((d) => d.cls === "TT" && d.dispatch != null && d.dispatch in ORDER)
    .sort((a, b) => ORDER[a.dispatch as string] - ORDER[b.dispatch as string] || a.id.localeCompare(b.id));
  const n = (s: string) => fleet.filter((d) => d.dispatch === s).length;
  return (
    <section className="tcard lvp">
      <div className="tcard-head">
        <h3>{k ? "작업 중 차량" : "Dispatched Fleet"}
          <span className="h3-sub">{k ? "이미 일이 있어 매처가 건너뛰는 트럭 (공차 이동·운반·대기)" : "trucks the matcher skips — already working (empty run · carrying · staging)"}</span></h3>
        <div className="head-sub"><span className="muted">{fleet.length}{k ? "대" : ""}</span></div>
      </div>
      <div className="tcard-body">
        <div className="lvp-stats">
          {(["empty_travel", "delivering", "staging"] as const).map((s) => (
            <div className="lvp-stat" key={s} style={{ borderTopColor: DSP_META[s].color }}>
              <div className="lvp-n">{n(s)}</div>
              <div className="lvp-l">{k ? DSP_META[s].ko : DSP_META[s].en}</div>
            </div>
          ))}
        </div>
        <div className="lvp-list">
          {fleet.length === 0 && <div className="lvp-empty">{k ? "없음" : "none"}</div>}
          {fleet.map((d) => (
            <div className="lvp-row" key={d.id}>
              <span className="sw" style={{ background: DSP_META[d.dispatch as string]?.color ?? "var(--text-mute)" }} title={dspTitle(d.dispatch, lang)} />
              <span className="lvp-id mono">{d.id}</span>
              {d.jobtype && <span className={`lvp-job type-${d.jobtype.toLowerCase()}`}>{d.jobtype}</span>}
              {d.topos1 && <span className="lvp-dest mono">→{d.topos1}</span>}
              <span className="lvp-why">{ttWhere(d, d.jobtype ?? null, lang) ?? dspTitle(d.dispatch, lang)}</span>
            </div>
          ))}
        </div>
        <div className="lvp-note">{k
          ? "핸드오버 중(곧 자유) 트럭은 위 후보 카드에 있다 — 매처는 그 트럭들을 이미 다음 배차 대상으로 계산한다."
          : "Trucks at handover (soon free) are in the candidate card above — the matcher already counts them for the next dispatch."}</div>
      </div>
    </section>
  );
}

// ───────────────────────── 맨 위로 ─────────────────────────
// 타임라인이 길어 어디서든 최상단 복귀 — 600px 넘게 내려가면 우하단에 나타난다.
// 스크롤 리스너는 passive + rAF 스로틀(같은 값이면 setState 가 바로 bail-out)이라
// 스크롤 성능 수리(313a0f0)를 되물리지 않는다.
function ScrollTopButton({ lang }: { lang: Lang }) {
  const [show, setShow] = useState(false);
  useEffect(() => {
    let raf = 0;
    const onScroll = () => {
      if (raf) return;
      raf = requestAnimationFrame(() => { raf = 0; setShow(window.scrollY > 600); });
    };
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => { window.removeEventListener("scroll", onScroll); if (raf) cancelAnimationFrame(raf); };
  }, []);
  if (!show) return null;
  return (
    <button className="scrolltop" title={ko(lang) ? "맨 위로" : "back to top"} aria-label={ko(lang) ? "맨 위로" : "back to top"}
      onClick={() => window.scrollTo({ top: 0, behavior: "smooth" })}>↑</button>
  );
}

// ───────────────────────── 페이지 ─────────────────────────
export default function TtPage({ lang }: { lang: Lang }) {
  const { snap, err } = usePositions();
  const { data: wp } = useWorkpool();
  const { advisory, picks } = useStage2();
  // 5초 틱 — 카운트다운(마감·출항·ETW)이 15초 폴 사이에도 흐르게 한다. 1초 틱은 트리 전체
  // 재조정을 매초 돌려 스크롤 페인트와 겹쳤다(2026-08-10 증상) — 5초면 체감 라이브는 같다.
  // 뷰 조립은 틱이 아니라 데이터 갱신(wp 15s·snap 3s)에만 다시 한다(useMemo).
  const [, setTick] = useState(0);
  useEffect(() => { const iv = setInterval(() => setTick((t) => t + 1), 5000); return () => clearInterval(iv); }, []);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const views = useMemo(() => buildVesselViews(wp, snap, Date.now()), [wp, snap]);
  return (
    <div className="content tt-page tt-2col">
      <div className="tt-col tt-col-qc">
        <PressureBoard lang={lang} wp={wp} views={views} />
        <QcTimeline lang={lang} wp={wp} snap={snap} views={views} advisory={advisory} picks={picks} />
      </div>
      <div className="tt-col tt-col-tt">
        <LiveDispatchPool lang={lang} snap={snap} err={err} />
        <FleetCard lang={lang} snap={snap} />
      </div>
      <ScrollTopButton lang={lang} />
    </div>
  );
}

