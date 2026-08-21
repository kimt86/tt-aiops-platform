# Scheduling (user systemd units — no root)

Unit files are named `tt-*`. Two families live here and they are deliberately kept apart:

- **Critical extraction** (`tt-nightly`, `tt-shift-*`, `tt-qc-moves`, …) — feeds the dashboard.
- **Scenario subsystem** (`tt-scenario-*`) — simulation input, non-critical. **2026-08-21 부터
  별도 저장소 `~/projects/tt-scengen` 소유**이고, 유닛 파일도 그쪽에 있다. 이 호스트에서 같이
  돌지만 여기서 빌드·설치하지 않는다.

## Long-running services

| unit | what |
|---|---|
| `tt-api` | read-only axum API over PostgreSQL |
| `tt-ws-bridge` | SSH tunnel for the live position feed |

## Timers — critical extraction

Conservative, load-conscious cadence against the live Oracle:

| timer | command | cadence | Oracle |
|---|---|---|---|
| `tt-nightly` | `extractor run --kpi all` (yesterday, authoritative) | 01:30 daily | k_cycle only (rest local since 2026-08-06) |
| `tt-shift-t1` | `extractor tick --shift --tier t1` (MPH/QC-wait/util + vessels, **LIVE tab**) | 3 min | **no** (local since 2026-08-06) |
| `tt-shift-t2` | `extractor tick --shift --tier t2` (voyage_plan + cumulative KPIs, **LIVE tab**) | 15 min | voyage_plan only |
| `tt-qc-moves` | `extractor qc-moves` (quay-crane move stream) | 60 s | yes (PK seek) |
| `tt-rtg-moves` | `extractor rtg-moves` (yard-crane move stream) | 60 s | yes (PK seek) |
| `tt-handover` | `extractor handover` | 60 s | yes (index seek) |
| `tt-workpool` | `extractor workpool` (merged 1-pull since 2026-08-10) | 60 s | yes |
| `tt-vessel-schedule` | `extractor vessel-schedule` | 5 min | yes |
| `tt-stowplan` | `extractor stowplan` (UPD_DT delta) | 2 min | yes |
| `tt-stowplan-recon` | `extractor stowplan --reconcile` | 1 h | yes |
| `tt-weather-live` | `extractor weather-live` | 3 min | **no** (HTTP) |
| `tt-weather` | `extractor weather` | 60 min | **no** (HTTP) |

Local (no Oracle) derivations driven by `psql` over `scripts/*.sql`:

| timer | script | cadence |
|---|---|---|
| `tt-move-log` | `populate_tt_move_log.sql` | 5 min |
| `tt-cycle-recon` | `populate_tt_cycle_recon.sql` | 10 min |
| `tt-cycle-pred-shadow` | `populate_cycle_pred_shadow.sql` | 2 min |
| `tt-learn-cycle-remaining` | `populate_learn_cycle_remaining.sql` | 30 min |

K_UTIL is intentionally **not** in the today-provisional path (full-day denominator → misleading
mid-day). In the **shift** ticks K_UTIL *is* included — its denominator is elapsed-shift-minutes,
so it is correct mid-shift. The `tt-shift-*` timers feed the **LIVE tab** (`/api/live`), which
reads `kpi_shift` for the *current terminal-time shift*; without them the LIVE tab goes blank as
soon as a new shift starts.

## Timers — scenario subsystem

**여기 없다. `~/projects/tt-scengen` 로 옮겼다(2026-08-21).**

시나리오/에뮬레이터 수집기는 별도 저장소가 됐다 — 유닛 파일도 그쪽 `deploy/systemd/` 에 있고,
`ExecStart` 도 그 저장소의 빌드 산출물을 가리킨다. 여기서 빌드해도 그 바이너리는 만들어지지
않는다(워크스페이스 멤버에서 빠졌다).

같이 옮겨간 것: `crates/scengen` · `scenario.*` 마이그레이션 18개 · 유닛 21개.
**남은 것**: `0109_move_log_queue_vessel.sql` — 이름과 달리 운영 표(`qc_move_log`·
`rtg_move_log`)에 컬럼을 더하는 마이그레이션이라 여기가 주인이다.

⚠ 수집 방식도 함께 바뀌었다: **상시 타이머가 아니라 요청받은 하루만** 돈다(Oracle 부하).
지금 이 호스트의 시나리오 타이머 10개는 전부 정지·`disabled` 상태다. 자세한 것은 그쪽 CLAUDE.md.

## Install (as the `tkadmin` user, no sudo)

```bash
cd ~/projects/tt-aiops-platform

# build the release binaries the units reference
cargo build --release -p tt-extractor -p tt-api

# install user units
mkdir -p ~/.config/systemd/user
cp deploy/systemd/tt-*.{service,timer} ~/.config/systemd/user/   # tt-scenario-* 는 여기 없다
systemctl --user daemon-reload

# critical extraction
systemctl --user enable --now tt-api.service
systemctl --user enable --now tt-nightly.timer tt-shift-t1.timer tt-shift-t2.timer \
                              tt-qc-moves.timer tt-rtg-moves.timer tt-handover.timer \
                              tt-workpool.timer tt-vessel-schedule.timer \
                              tt-stowplan.timer tt-stowplan-recon.timer \
                              tt-weather.timer tt-weather-live.timer
systemctl --user enable --now tt-move-log.timer tt-cycle-recon.timer \
                              tt-cycle-pred-shadow.timer tt-learn-cycle-remaining.timer

# scenario subsystem — ~/projects/tt-scengen 참조. 타이머는 상시로 켜지 않는다(요청 기반).

# keep everything running after logout (REQUIRED — otherwise --user units stop on SSH disconnect)
loginctl enable-linger tkadmin

# inspect
systemctl --user list-timers 'tt-*'
journalctl --user -u tt-shift-t1.service -n 50
```

`.env` (loaded via `EnvironmentFile`) must define `DATABASE_URL` and `SKILL_DIR`.

## Tuning load

- Widen intervals (`OnUnitActiveSec`, or the `OnCalendar` step for the second-staggered units)
  to reduce Oracle hits; `daemon-reload` after editing. Firing seconds are deliberately spread
  (:05 qc / :15 stowplan / :25 rtg / :30 gate / :35 vessel / :45 handover / :50 contspec /
  :55 workpool) — keep a new timer off those slots.
- The scenario collectors serialize their Oracle access with `flock(1)` among themselves. The
  critical extractors are deliberately **outside** that lock — several fire every 60 s and must not
  queue behind a scenario query.
- Watch the admin page's per-run health column: there is no Oracle-side statement timeout, so a
  query plan flipping to a full scan shows up only as a quietly repeating slow poll.

## `tt-*` 가 아닌 것들 — 터널 2개와 crontab

이 저장소의 `deploy/systemd/` 는 대부분 `tt-*` 이지만, 아래 셋은 이름도 성격도 다르다.
셋 다 **멈추면 조용히 나빠지는** 종류라 따로 적어둔다.

| 대상 | 위치 | 멈추면 |
|---|---|---|
| GPS 웹소켓 터널 | `wp-ws-bridge.service` | 위치 파이프라인의 **단일 장애점**. 트럭 좌표가 멈추고, 매칭이 GPS 미연결 틱을 건너뛰므로 배차 추천도 함께 선다 |
| ETW 게이트웨이 터널 | `wp-etw-bridge.service` | 추출기가 **warn 만 남기고 조용히 건너뛴다** — 마감 정밀도가 티 안 나게 나빠진다 |
| crontab 2건 | `../crontab.example` | 도로망 재추론이 **배차 비용의 본체**를 만든다. 멈추면 전 OD 가 맨해튼 폴백으로 떨어져 매칭 품질과 `cost_tier` 가 동시에 왜곡된다 |

이름이 `wp-*` 로 남은 것은 개명 결정상 터널은 그대로 두기로 했기 때문이다(2026-07-23).

⚠ **ETW 게이트웨이는 다른 시스템도 함께 쓴다** — 주기·질의·중단을 우리 판단으로 바꾸지
않는다(2026-08-05 지시). 돌고 있는 터널을 저장소 사본으로 덮어쓸 이유도 없다.

⚠ crontab 만 `.example` 이다. systemd 유닛과 달리 파일을 복사하는 방식이 아니라 기존
crontab 에 **덧붙이는** 것이라, 중복 등록을 피하려면 `crontab -l` 로 먼저 확인해야 한다.

> **2026-08-11 정정**: 이 절을 처음 쓸 때 "터널 유닛 2개가 형상관리 밖에 있다"고 적고
> `.example` 템플릿까지 만들었는데 **틀렸다.** 두 유닛은 이미 2026-07-28 커밋 `5a24c7e`(⑤항)
> 으로 저장소에 들어와 있었다. 2026-07-22 디스커버리 문서의 "저장소에 없음" 서술을 그대로
> 믿고 현물을 확인하지 않은 탓이다. 잘못 만든 `.example` 2개는 지웠다.
> **실제로 빠져 있던 것은 crontab 2건뿐이다.**
