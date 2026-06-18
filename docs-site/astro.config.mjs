// @ts-check
import { defineConfig } from 'astro/config';
import mermaid from 'astro-mermaid';
import starlight from '@astrojs/starlight';

// 지식센터는 Rust API가 정적 빌드(dist)를 `/kc/`로 서빙한다 → base를 '/kc'로 둬야
// 링크·자산·Pagefind 검색 경로가 맞는다. (대시보드 SPA는 '/'를 쓰므로 충돌 방지.)
// https://astro.build/config
export default defineConfig({
	base: '/kc',
	integrations: [
		// mermaid는 starlight보다 먼저 — ```mermaid 코드펜스를 클라이언트에서 SVG로 렌더(브라우저 빌드 불필요)
		mermaid({ theme: 'dark', autoTheme: true }),
		starlight({
			title: 'WP-TT 지식센터',
			description:
				'컨테이너 터미널 TT(트랜스퍼 트럭) 모니터링·배차·사이클 분석과 AI 배차 연구의 단일 출처 — 리서치·아키텍처·의사결정·실험·기록을 한곳에.',
			// 주 언어를 루트 로케일로 (URL 접두사 없음)
			defaultLocale: 'root',
			locales: {
				root: { label: '한국어', lang: 'ko-KR' },
			},
			social: [
				{ icon: 'github', label: 'GitHub', href: 'https://github.com/kimt86/wp-tt-dashboard' },
			],
			// Starlight 0.39+ : label이 있는 자동생성 그룹은 반드시 items로 감쌀 것.
			sidebar: [
				{ label: '시작하기', items: [{ autogenerate: { directory: 'start' } }] },
				{ label: '대시보드 해설', items: [{ autogenerate: { directory: 'dashboard' } }] },
				{ label: '배차 로직', items: [{ autogenerate: { directory: 'dispatch' } }] },
				{ label: '기획', items: [{ autogenerate: { directory: 'planning' } }] },
				{ label: '리서치', items: [{ autogenerate: { directory: 'research' } }] },
				{ label: '아키텍처', items: [{ autogenerate: { directory: 'architecture' } }] },
				{ label: '의사결정 (ADR)', items: [{ autogenerate: { directory: 'decisions' } }] },
				{ label: '실험', items: [{ autogenerate: { directory: 'experiments' } }] },
				{ label: '구현 기록', items: [{ autogenerate: { directory: 'journal' } }] },
				{ label: '지식베이스', items: [{ autogenerate: { directory: 'knowledge' } }] },
				{ label: '템플릿', items: [{ autogenerate: { directory: 'templates' } }] },
			],
		}),
	],
	// (선택) 다른 기기에서 dev 접근 — Tailscale 등. 정적 서빙(API)이 1차 경로라 보조.
	server: { host: true },
	vite: { server: { allowedHosts: true } },
});
