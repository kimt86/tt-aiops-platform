---
title: 참고자료
description: 외부 명세·도구 링크와 내부 코드 위치.
sidebar:
  order: 2
---

## 외부 (도구·표준)

- [Astro Starlight 문서](https://starlight.astro.build/) — 이 지식센터의 프레임워크 (확인일 2026-06-12)
- [Pagefind](https://pagefind.app/) — Starlight 내장 전문 검색 (확인일 2026-06-12)
- [Starlight Asides(콜아웃)](https://starlight.astro.build/guides/authoring-content/#asides) — `:::note/tip/caution/danger`

## 내부 (코드·운영)

- 라이브 GPS 적재 + 사이클 상태머신: `crates/api/src/livemap.rs`
- 사이클 이력 API: `crates/api/src/cycles.rs` (`/api/tt-cycles/*`)
- DB 마이그레이션: `db/migrations/` (최신 `0078`대 — 공차 레그 분해·도로망 검증 로거)
- 도로망 라우터(방향 Dijkstra): `crates/api/src/roadgraph.rs`
- 도로망 재추론 cron: `scripts/reinfer_roadgraph.sh` (GPS→스켈레톤→링크·매시)
- 대시보드 프론트엔드: `web/src/` (CYCLES 페이지 = `CyclesPage.tsx`)
- 지식센터(이 사이트): `docs-site/` — 빌드 `npm run build`, 운영 API가 `dist`를 `/kc/`로 서빙

## 운영 환경

- DB: PostgreSQL 17 · `postgresql://wp:wp@127.0.0.1:5433/wp_tt`
- API 서비스: `systemctl --user {restart,status} wp-api.service` (release 빌드 실행)
- 대시보드: `http://100.95.189.16:8080` (Tailscale) · 지식센터 `/kc/`
- 시간대: DB·로그는 UTC, 사람이 보는 시각은 터미널 현지 **MYT(UTC+8)**로 변환(터미널=말레이시아 Westports/Port Klang)

:::note
운영 절차·로드맵은 [기획 / 로드맵](/kc/planning/roadmap/)에, 검증 명령은 이 프로젝트의 README/코드 주석에 둔다.
:::
