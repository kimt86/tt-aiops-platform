// 공용 시간·수치 포맷 — 터미널 현지(MYT, UTC+8) 표시 전용.
// 파일마다 중복 정의되던 것을 통합(P3). ⚠ 옛 이름 kstTime 은 이름만 KST 였고 실제로는
// MYT 를 그렸다 — 여기서는 mytTime 하나로 통일한다.

/** ISO → "HH:MM:SS" (MYT) */
export const mytTime = (iso: string | null | undefined): string =>
  iso ? new Date(iso).toLocaleTimeString("en-GB", { timeZone: "Asia/Kuala_Lumpur", hour12: false }) : "–";

/** ISO → "HH:MM" (MYT) */
export const mytHm = (iso: string | null | undefined): string =>
  iso
    ? new Date(iso).toLocaleTimeString("en-GB", {
        timeZone: "Asia/Kuala_Lumpur", hour12: false, hour: "2-digit", minute: "2-digit",
      })
    : "–";

export const pct = (v: number | null | undefined): string => (v == null ? "–" : `${v.toFixed(1)}%`);

/** 초 → "m:ss" */
export const mmss = (s: number | null | undefined): string => {
  if (s == null) return "–";
  const a = Math.abs(Math.round(s));
  return `${s < 0 ? "-" : ""}${Math.floor(a / 60)}:${String(a % 60).padStart(2, "0")}`;
};

/** 초 → 부호 있는 분 ("+3.2분" 꼴은 페이지 몫, 여기선 숫자만) */
export const signMin = (s: number | null | undefined): string =>
  s == null ? "–" : `${s >= 0 ? "+" : ""}${(s / 60).toFixed(1)}`;
