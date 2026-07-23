# Scheduling (user systemd timers — no root)

Conservative, load-conscious cadence against the live Oracle:

| timer | what | cadence | Oracle load |
|---|---|---|---|
| `wp-nightly` | `run --kpi all` for yesterday (authoritative) + transform + baseline | 01:30 daily | 8 capped queries, off-peak |
| `wp-tick-t1` | `tick --tier t1` (MPH only, today provisional) | every 5 min | 1 LOW query |
| `wp-tick-t2` | `tick --tier t2` (empty/cycle/crane-q, today provisional) | every 20 min | 3–4 index-range queries |
| `wp-shift-t1` | `tick --shift --tier t1` (MPH/QC-wait/util + vessels, **LIVE tab**) | every 3 min | 3 LOW queries (MCH_OPERATION) |
| `wp-shift-t2` | `tick --shift --tier t2` (empty/cycle/crane-q cumulative, **LIVE tab**) | every 15 min | 3 index-range queries (JOB_ORDER_HISTORY) |

K_UTIL is intentionally **not** in the today-provisional ticks (full-day denominator → misleading mid-day); it refreshes only at the nightly run. In the **shift** ticks K_UTIL *is* included — its denominator is elapsed-shift-minutes, so it is correct mid-shift.

The `wp-shift-*` timers feed the **LIVE tab** (`/api/live`), which reads `kpi_shift` for the *current terminal-time shift*. Without these timers running, the LIVE tab goes blank as soon as a new shift starts (no rows for today's shift). The `wp-tick-*` (non-shift) timers feed only the HISTORY tab's today-provisional overlay and are independent.

## Install (as the `tkadmin` user, no sudo)

```bash
# build the release binary the units reference
cd ~/projects/tt-aiops-platform && cargo build --release -p wp-extractor

# install user units
mkdir -p ~/.config/systemd/user
cp deploy/systemd/wp-*.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now wp-nightly.timer wp-tick-t1.timer wp-tick-t2.timer

# LIVE-tab shift ticks (keep the LIVE tab populated for the current shift)
systemctl --user enable --now wp-shift-t1.timer wp-shift-t2.timer

# keep timers running after logout (REQUIRED — otherwise --user timers stop on SSH disconnect)
loginctl enable-linger tkadmin

# inspect
systemctl --user list-timers 'wp-*'
journalctl --user -u wp-tick-t1.service -n 50
```

`.env` (loaded via `EnvironmentFile`) must define `DATABASE_URL` and `SKILL_DIR`.

## Tuning load

- Widen tick intervals (edit `OnUnitActiveSec`) to reduce Oracle hits.
- To run ticks only during operating hours, add `OnCalendar=` windows or a guard.
- A future enhancement: adaptive backoff in `tick` (skip when no new completed work).
