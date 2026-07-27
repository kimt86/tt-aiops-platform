# Scheduling (user systemd units — no root)

Unit files are named `tt-*`. Two families live here and they are deliberately kept apart:

- **Critical extraction** (`tt-nightly`, `tt-shift-*`, `tt-qc-moves`, …) — feeds the dashboard.
- **Scenario subsystem** (`tt-scenario-*`) — simulation input, non-critical. Separate binary,
  separate `scenario` schema, its own kill switch. A failure here must never disturb the above.

## Long-running services

| unit | what |
|---|---|
| `tt-api` | read-only axum API over PostgreSQL |
| `tt-scenario-web` | isolated scenario monitor/download page on its own port (`:8899`) |
| `tt-ws-bridge` | SSH tunnel for the live position feed |

## Timers — critical extraction

Conservative, load-conscious cadence against the live Oracle:

| timer | command | cadence |
|---|---|---|
| `tt-nightly` | `extractor run --kpi all` (yesterday, authoritative) | 01:30 daily |
| `tt-shift-t1` | `extractor tick --shift --tier t1` (MPH/QC-wait/util + vessels, **LIVE tab**) | 3 min |
| `tt-shift-t2` | `extractor tick --shift --tier t2` (empty/cycle/crane-q cumulative, **LIVE tab**) | 15 min |
| `tt-qc-moves` | `extractor qc-moves` (quay-crane move stream) | 60 s |
| `tt-rtg-moves` | `extractor rtg-moves` (yard-crane move stream) | 60 s |
| `tt-handover` | `extractor handover` | 60 s |
| `tt-workpool` | `extractor workpool` | 90 s |
| `tt-weather-live` | `extractor weather-live` | 3 min |
| `tt-weather` | `extractor weather` | 60 min |

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

| timer | command | cadence | Oracle |
|---|---|---|---|
| `tt-scenario-collect` | `scengen collect` (vessel attribution stream) | 10 min | yes |
| `tt-scenario-yard` | `scengen yard-moves` (yard-crane moves + decoded slot) | 5 min | yes |
| `tt-scenario-gate` | `scengen gate` (gate transaction times for the local GI/GO containers) | 5 min | yes |
| `tt-scenario-enrich` | `scengen enrich` (vessel particulars, container details) | 15 min | yes |
| `tt-scenario-yard-build` | `scengen yard-build` (replays moves into the stack model) | 10 min | **no** |
| `tt-scenario-snapshot` | `scengen snapshot` | — | **do not enable** |

`tt-scenario-gate` is the odd one out: its watermark points at **our own** `rtg_move_log`, not at an
Oracle key, because `CYC_HISTORY` has no time-leading index. It walks the local gate moves forward
and looks those containers up by number, staying 60 minutes behind live so a truck's exit has been
written before it asks. Reset its watermark to re-collect a past range.

`tt-scenario-snapshot` is retired as a periodic job: it swept a hot 110k-row table six times a day
to keep a 302-row static block-name map current. Run it **by hand** when new yard blocks appear —
the admin page shows an "unresolved" count that tells you when. It is an Oracle job, so the kill
switch has to be on for it to do anything.

## Install (as the `tkadmin` user, no sudo)

```bash
cd ~/projects/tt-aiops-platform

# build the release binaries the units reference
cargo build --release -p tt-extractor -p tt-api -p scengen

# install user units
mkdir -p ~/.config/systemd/user
cp deploy/systemd/tt-*.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload

# critical extraction
systemctl --user enable --now tt-api.service
systemctl --user enable --now tt-nightly.timer tt-shift-t1.timer tt-shift-t2.timer \
                              tt-qc-moves.timer tt-rtg-moves.timer tt-handover.timer \
                              tt-workpool.timer tt-weather.timer tt-weather-live.timer
systemctl --user enable --now tt-move-log.timer tt-cycle-recon.timer \
                              tt-cycle-pred-shadow.timer tt-learn-cycle-remaining.timer

# scenario subsystem (5 timers — snapshot is on-demand, see above)
systemctl --user enable --now tt-scenario-web.service
systemctl --user enable --now tt-scenario-collect.timer tt-scenario-yard.timer \
                              tt-scenario-gate.timer tt-scenario-yard-build.timer \
                              tt-scenario-enrich.timer

# keep everything running after logout (REQUIRED — otherwise --user units stop on SSH disconnect)
loginctl enable-linger tkadmin

# inspect
systemctl --user list-timers 'tt-*'
journalctl --user -u tt-shift-t1.service -n 50
```

`.env` (loaded via `EnvironmentFile`) must define `DATABASE_URL` and `SKILL_DIR`.

## Stopping the scenario subsystem

Two levers, in order of bluntness:

```bash
# soft: collectors keep firing but return immediately; survives restarts
psql "$DATABASE_URL" -c "UPDATE scenario.config SET enabled=false"

# hard: stop the timers entirely
systemctl --user disable --now 'tt-scenario-*.timer'
```

The kill switch is read at the top of every collector tick, so it takes effect within one cadence.
`tt-scenario-yard-build` is deliberately **not** gated by it — replaying already-collected moves
costs no Oracle and should be allowed to finish even while collection is paused.

Resuming after a long pause: seed `scenario.watermark` to where you want each stream to restart
**before** turning the kill switch back on. The first tick of a stream with no watermark row jumps
forward to "now", which turns the gap into permanent data loss.

## Tuning load

- Widen intervals (`OnUnitActiveSec`) to reduce Oracle hits; `daemon-reload` after editing.
- The scenario collectors serialize their Oracle access with `flock(1)` among themselves. The
  critical extractors are deliberately **outside** that lock — several fire every 60 s and must not
  queue behind a scenario query.
- Watch the admin page's per-run health column: there is no Oracle-side statement timeout, so a
  query plan flipping to a full scan shows up only as a quietly repeating slow poll.
