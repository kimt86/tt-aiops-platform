# tt-aiops-platform

Operations KPI dashboard for the WP terminal, built on the `tos-db-research` findings.
Six KPIs (TT utilization, empty-travel distance & ratio, TT cycle time, crane wait,
QC moves/hour) are extracted from the **live production Oracle TOS DB**, served from
PostgreSQL, and shown in a bilingual (KO/EN) React dashboard.

> **Hard rule:** the source Oracle is live production. Only two binaries ever touch it —
> the **extractor** (critical path) and **scengen** (scenario collection, non-critical and
> individually killable) — both read-only, capped, index-range queries via `remote-toolbox-sql`.
> The API and web app read **only PostgreSQL**.

## Architecture

```
Oracle TOSADM ──(extractor, Rust)──> PostgreSQL ──(axum API, Rust)──> React/Vite (Chart.js)
  read-only        L0 raw → L1 rollup → L2 baseline      read-only        KO/EN, polling
```

- **L0** `raw_*` — one table per validated SQL, per business day (idempotent upserts).
- **L1** `kpi_daily` (6 headline values/day) + `kpi_breakdown_qc`.
- **L2** `kpi_baseline` — 28-day baseline + Welch two-sample significance.
- Realtime: intra-day `tick` refreshes "today so far" as **provisional**; the API
  serves each KPI at its own latest snapshot (heterogeneous freshness).

## Crates / dirs

Package names are `tt-*` (`tt-core`, `tt-extractor`, `tt-api`); `scengen` keeps its own name.

- `crates/core` — double-parse of toolbox output, stats (paired/Welch t-test), KPI metadata.
- `crates/extractor` — the critical Oracle-touching binary. `run` / `tick` / `backfill` / `transform`.
- `crates/api` — read-only axum API over L1/L2.
- `crates/scengen` — simulation scenario + emulator generator. Isolated on purpose: its own
  binary, its own `scenario` schema (never `public`), its own port (`:8899`), its own kill
  switch. Collects from Oracle on a slow cadence and assembles a chosen window locally.
- `web/` — React + Vite + Chart.js dashboard.
- `kc/` — knowledge centre (static HTML), served at `/kc/`.
- `db/` — migrations, seed, read-only `grants.sql`.
- `deploy/` — `dev-db.sh` (rootless podman Postgres), `systemd/` units.
- `scripts/` — `dev.sh` (full local stack), `snapshot-report.sh`, and the `psql` batches the
  local-derivation timers run.

## Run locally

```bash
# 1. dev Postgres (rootless podman) + schema
./deploy/dev-db.sh up
DATABASE_URL=postgresql://wp:wp@127.0.0.1:5433/wp_tt ./db/apply.sh

# 2. extract real data (read-only against prod Oracle)
export DATABASE_URL=postgresql://wp:wp@127.0.0.1:5433/wp_tt
export SKILL_DIR=/home/aiadmin/.codex/skills/yard-db-ops
cargo run -p tt-extractor -- run --kpi all --date 2026-06-04 --target oracle-prod
# optional history for trends/baseline:
cargo run -p tt-extractor -- backfill --from 2026-05-01 --to 2026-06-04

# 3. full stack (API + web)
./scripts/dev.sh           # → http://127.0.0.1:5173
```

## Extractor commands

| command | purpose | Oracle load |
|---|---|---|
| `run --kpi all --date D` | authoritative full-day (8 extracts + transform + baseline) | 8 capped queries |
| `tick --tier t1\|t2` | intra-day "today" provisional refresh | 1–4 light queries |
| `backfill --from --to` | seed history, one day at a time, throttled | N days × above |
| `transform --date D` | recompute L1/L2 from L0 (no Oracle) | none |

Scheduling: see [deploy/systemd/README.md](deploy/systemd/README.md). Cadence is deliberately
conservative to spare the source DB.

## Scenario generator (`scengen`)

Produces the two JSON inputs a terminal simulator needs for a chosen period: the **scenario**
(what happened — vessel work lists, landside gate moves, equipment deployment, the yard as it
stood at t=0) and the **emulator** spec (measured equipment timings). Collection runs on slow
timers; assembly is local and touches Oracle not at all.

```bash
cargo run -p scengen -- collect --target oracle-prod   # one incremental tick
cargo run -p scengen -- yard-build                     # local replay, no Oracle
cargo run -p scengen -- serve --port 8899              # monitor + download page
```

Pick a period on the page and download; nothing is generated in advance. Stop everything with
`UPDATE scenario.config SET enabled=false`.

## Tests

```bash
DATABASE_URL=postgresql://wp:wp@127.0.0.1:5433/wp_tt cargo test   # backend (offline + dev-PG)
cd web && npm run build                                          # frontend type-check + build
```
Backend tests use golden `remote-toolbox-sql` fixtures + the dev Postgres; none hit Oracle.

## Production hardening

- API connects as the read-only `wp_ro` role (`db/grants.sql`) — SELECT on L1/L2 only,
  no access to `raw_*`/`stg_*`, no writes.
- Tighten `CorsLayer` to the dashboard origin.
- Build `--release` for the binaries the timers reference.

## Known data facts (from extraction)

- **`JOB_ORDER_HISTORY` retains ~15 days** live → empty/cycle/crane-wait baselines use a
  shorter window (`baseline_n_days` records the actual count). MCH_OPERATION / VSS go back ≥35d.
- `YT_DIS_DT` / `ACTV_DT` (crane-wait inputs) are sparse on older days → those days yield 0 rows.
- The toolbox returns `{"result":"null"}` for an empty result set (handled as 0 rows).

## Open decisions (need sign-off — see plan)

1. Per-KPI daily aggregation rule (mean vs p50 for cycle, etc.).
2. Per-QC empty/crane-wait & hourly empty heatmap need new source SQL (phase-1 shows `—`).
3. Paired-vs-two-sample significance grain (currently Welch recent-vs-baseline on daily series).
4. KPI target/excellent thresholds in research units (seeded NULL pending sign-off).
