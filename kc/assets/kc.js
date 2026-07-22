/* WP-TT 지식센터 — 공용 셸(사이드바·토프바·TOC·검색·이전/다음).
   각 페이지는 <main class="kc-article" data-page="section/slug"> 만 담고, 이 스크립트가 셸을 주입한다.
   홈은 <body data-home><div class="kc-home">…</div>. 나브·제목의 단일 출처는 아래 NAV. */

const NAV = [
  { id: "start", label: "시작 & 기획", items: [
    { page: "start/overview",       title: "지식센터 개요" },
    { page: "start/summary",        title: "5분 요약" },
    { page: "start/contributing",   title: "글쓰기 규칙" },
    { page: "start/prd",            title: "PRD" },
    { page: "start/roadmap",        title: "로드맵" },
    { page: "start/launch-plan",    title: "정식 전환 계획" },
  ]},
  { id: "dashboard", label: "대시보드 가이드", items: [
    { page: "dashboard/index",       title: "대시보드 완전 해설" },
    { page: "dashboard/kpi",         title: "KPI 페이지" },
    { page: "dashboard/dispatch",    title: "TT Dispatch & 라이브맵" },
    { page: "dashboard/cycle",       title: "Cycle 페이지" },
    { page: "dashboard/learning",    title: "러닝 센터" },
    { page: "dashboard/stage2-match", title: "MATCH (2단계 매칭)" },
  ]},
  { id: "data", label: "데이터 & 지표", items: [
    { page: "data/tos-extraction",     title: "TOS 데이터 추출 전수", group: "TOS" },
    { page: "data/tos-db-reference",   title: "TOS DB 레퍼런스", group: "TOS" },
    { page: "data/tos-verification",   title: "TOS 가정 검증", group: "TOS" },
    { page: "data/websocket-data",     title: "실시간 피드 & 활용", group: "실시간 피드" },
    { page: "data/websocket-fields",   title: "websocket 필드 레퍼런스", group: "실시간 피드" },
    { page: "data/websocket-coverage", title: "신호 품질·커버리지 감사", group: "실시간 피드" },
    { page: "data/feed-semantics",     title: "피드 의미론 실측", group: "실시간 피드" },
    { page: "data/kpi-computation",    title: "KPI 산출", group: "KPI · 사이클" },
    { page: "data/kpi-accuracy",       title: "websocket로 KPI 정확도", group: "KPI · 사이클" },
    { page: "data/cycle-decomposition", title: "사이클 분해 (현행)", group: "KPI · 사이클" },
    { page: "data/cycle-gps-quality",  title: "GPS 싸이클 해석 품질·한계", group: "KPI · 사이클" },
    { page: "data/cycle-detection",    title: "사이클 감지 로직", group: "KPI · 사이클" },
    { page: "data/dispatch-pools",     title: "차량풀 · 작업풀 갱신", group: "KPI · 사이클" },
  ]},
  { id: "dispatch", label: "배차 (2단계)", items: [
    { page: "dispatch/index",           title: "배차 로직 — 2단계" },
    { page: "dispatch/amr-allocation",  title: "요구사항 (R1–R6)" },
    { page: "dispatch/service2-overview", title: "WP AI Service 2 개요" },
    { page: "dispatch/stage2-design",   title: "2단계 매칭 설계" },
    { page: "dispatch/stage1-journey",  title: "1단계 여정" },
    { page: "dispatch/stage2-journey",  title: "2단계 여정" },
    { page: "dispatch/stage2-rollout",  title: "롤아웃 계획" },
    { page: "dispatch/simulator-spec",  title: "배차 시뮬레이터" },
    { page: "dispatch/research-log",    title: "배차 조사 기록 (연구 일지)" },
    { page: "dispatch/leadtime-adr",    title: "ADR — 유휴 리드타임 예측불가" },
  ]},
  { id: "learn", label: "학습 · 예측 & 리서치", items: [
    { page: "learn/learning-center",   title: "학습 센터 기획" },
    { page: "learn/travel-time",       title: "이동시간 모델 (명세 + 이력)" },
    { page: "learn/tt-prediction",     title: "2단계 예측 모형" },
    { page: "learn/tt-dispatch-problem", title: "스마트 TT 배차 (문제 정의)" },
    { page: "learn/soon-idle-tos",     title: "곧-유휴 감지 (TOS)" },
    { page: "learn/qc-workpoint",      title: "QC 작업지점" },
    { page: "learn/rtg-work-cycle",    title: "RTG 작업 사이클" },
    { page: "learn/cycle-v2-shadow",   title: "사이클 v2 그림자 검증" },
  ]},
  { id: "reference", label: "레퍼런스", items: [
    { page: "reference/glossary",          title: "용어집 & FAQ" },
    { page: "reference/references",        title: "참고자료" },
    { page: "reference/capacity-planning", title: "클라우드 용량 산정" },
  ]},
];

const FLAT = NAV.flatMap((s) => s.items.map((it) => ({ ...it, sec: s.label })));
const href = (page) => `/kc/${page}.html`;
const byPage = (p) => FLAT.find((it) => it.page === p);

function h(tag, attrs, ...kids) {
  const e = document.createElement(tag);
  if (attrs) for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") e.className = v; else if (k === "html") e.innerHTML = v;
    else if (v != null) e.setAttribute(k, v);
  }
  for (const k of kids.flat()) if (k != null) e.append(k.nodeType ? k : document.createTextNode(k));
  return e;
}
const slug = (s) => s.trim().toLowerCase().replace(/[^\w가-힣]+/g, "-").replace(/^-+|-+$/g, "") || "s";

document.addEventListener("DOMContentLoaded", () => {
  const body = document.body;
  const isHome = body.hasAttribute("data-home");
  const article = body.querySelector(".kc-article");
  const homeContent = body.querySelector(".kc-home");
  const page = article?.dataset.page || "";
  const cur = byPage(page);

  // ── sidebar ──
  const search = h("input", { type: "search", placeholder: "문서 검색…", "aria-label": "검색" });
  const navEl = h("nav", { class: "kc-nav" });
  for (const s of NAV) {
    navEl.append(h("div", { class: "sec" }, s.label));
    for (const it of s.items) {
      const a = h("a", { href: href(it.page), "data-title": it.title }, it.title);
      if (it.page === page) a.classList.add("active");
      navEl.append(a);
    }
  }
  const noHit = h("div", { class: "hit-none", style: "display:none" }, "일치하는 문서 없음");
  navEl.append(noHit);
  search.addEventListener("input", () => {
    const q = search.value.trim().toLowerCase(); let hits = 0;
    navEl.querySelectorAll("a").forEach((a) => {
      const on = !q || a.dataset.title.toLowerCase().includes(q);
      a.style.display = on ? "" : "none"; if (on) hits++;
    });
    navEl.querySelectorAll(".sec").forEach((sec) => {
      let n = sec.nextElementSibling, any = false;
      while (n && !n.classList.contains("sec") && !n.classList.contains("hit-none")) {
        if (n.tagName === "A" && n.style.display !== "none") any = true; n = n.nextElementSibling;
      }
      sec.style.display = any ? "" : "none";
    });
    noHit.style.display = hits ? "none" : "block";
  });

  const side = h("aside", { class: "kc-side" },
    h("a", { class: "kc-brand", href: "/kc/", style: "text-decoration:none" },
      h("span", { class: "mark" }, "TT"),
      h("span", null, h("span", { class: "bt" }, "지식센터"), h("span", { class: "bs" }, "TT AiOps Platform"))),
    h("div", { class: "kc-search" }, search),
    navEl);

  // ── topbar ──
  const hamb = h("button", { class: "kc-hamb", "aria-label": "메뉴" }, "☰");
  const crumb = isHome
    ? h("span", { class: "kc-crumb" }, h("b", null, "지식센터"))
    : h("span", { class: "kc-crumb" }, cur ? cur.sec : "문서", " › ", h("b", null, cur ? cur.title : (document.title || "")));
  const srcChip = article?.dataset.source
    ? h("span", { class: "kc-src", title: "원본 소스" }, article.dataset.source) : null;
  const top = h("div", { class: "kc-top" }, hamb, crumb, h("span", { class: "sp" }), srcChip);

  // ── body/content ──
  let mainInner;
  if (isHome) {
    mainInner = homeContent || h("div", { class: "kc-home" });
  } else {
    // TOC from article headings
    const toc = h("nav", { class: "kc-toc" });
    const heads = article ? [...article.querySelectorAll("h2, h3")] : [];
    if (heads.length) {
      toc.append(h("div", { class: "h" }, "이 페이지"));
      for (const hd of heads) {
        if (!hd.id) hd.id = slug(hd.textContent);
        toc.append(h("a", { href: "#" + hd.id, class: hd.tagName === "H3" ? "h3" : "" }, hd.textContent));
      }
    }
    // prev/next
    const idx = FLAT.findIndex((it) => it.page === page);
    const prev = idx > 0 ? FLAT[idx - 1] : null, next = idx >= 0 && idx < FLAT.length - 1 ? FLAT[idx + 1] : null;
    if (article && (prev || next)) {
      const pn = h("nav", { class: "kc-pagenav" });
      pn.append(prev ? h("a", { class: "prev", href: href(prev.page) }, h("div", { class: "d" }, "← 이전"), h("div", { class: "t" }, prev.title)) : h("span", { style: "flex:1" }));
      pn.append(next ? h("a", { class: "next", href: href(next.page) }, h("div", { class: "d" }, "다음 →"), h("div", { class: "t" }, next.title)) : h("span", { style: "flex:1" }));
      article.append(pn);
    }
    mainInner = h("div", { class: "kc-body" }, article, heads.length ? toc : h("div"));
    // scrollspy
    if (heads.length && "IntersectionObserver" in window) {
      const map = new Map(heads.map((hd) => [hd.id, toc.querySelector(`a[href="#${CSS.escape(hd.id)}"]`)]));
      const io = new IntersectionObserver((ents) => {
        ents.forEach((en) => { if (en.isIntersecting) {
          toc.querySelectorAll("a").forEach((a) => a.classList.remove("active"));
          map.get(en.target.id)?.classList.add("active");
        }});
      }, { rootMargin: "-64px 0px -70% 0px" });
      heads.forEach((hd) => io.observe(hd));
    }
  }

  const main = h("main", { class: "kc-main" }, top, mainInner);
  const scrim = h("div", { class: "kc-scrim" });
  const shell = h("div", { class: "kc-shell" }, side, main, scrim);
  hamb.addEventListener("click", () => shell.classList.toggle("open"));
  scrim.addEventListener("click", () => shell.classList.remove("open"));

  body.innerHTML = "";
  body.append(shell);
});
