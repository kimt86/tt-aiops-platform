//! tt-aiops-platform extractor — the ONLY component that touches the production
//! Oracle (via `remote-toolbox-sql`). Subcommands:
//!   run      — authoritative full-day extract for a date (nightly)
//!   tick     — intra-day incremental extract (T1/T2 tiers)
//!   backfill — loop run over a date range to seed history
//!   transform— recompute L1/L2 from L0 (no Oracle access)

use tt_extractor::{baseline, db, kpis, transform};

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "extractor", about = "tt-aiops-platform KPI extractor")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Authoritative full-day extract for one business date (default: yesterday).
    Run {
        #[arg(long)]
        date: Option<String>,
        #[arg(long, default_value = "all")]
        kpi: String,
        #[arg(long, default_value = "oracle-prod")]
        target: String,
    },
    /// Intra-day "today so far" refresh (provisional). Excludes K_UTIL, whose
    /// full-day denominator makes partial-day values misleading.
    ///   t1 = cheap/frequent (MPH);  t2 = heavier JOB_ORDER_HISTORY KPIs.
    /// With --shift: current-shift cumulative KPIs + vessel panel (LIVE tab).
    Tick {
        #[arg(long, default_value = "t1")]
        tier: String,
        #[arg(long, default_value = "oracle-prod")]
        target: String,
        /// Current-shift cumulative mode (LIVE tab) instead of today-provisional.
        #[arg(long)]
        shift: bool,
    },
    /// Backfill a date range (inclusive), one day at a time, throttled.
    Backfill {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long, default_value = "oracle-prod")]
        target: String,
        /// Seconds to sleep between days (gentle on production Oracle).
        #[arg(long, default_value = "3")]
        sleep: u64,
    },
    /// Recompute L1/L2 rollups from L0 (no Oracle access).
    Transform {
        #[arg(long)]
        date: Option<String>,
    },
    /// Live work-pool snapshot (JOB_QUEUE_SCHEDULE + JOB_ORDER_LIST) → Postgres.
    /// Refreshes the per-QC work queues + dispatchable moves; run ~every 90s.
    Workpool {
        #[arg(long, default_value = "oracle-prod")]
        target: String,
    },
    /// 적부계획의 상자별 작업 순번(VSP_SHIP.VSP_SHP_PLANSEQ) → live_stow_plan.
    /// 구역 안 순서의 권위 값 — 작업지시 표에는 순서가 없다. 적·양하 둘 다(mig0129).
    /// STOWPLAN_MODE=delta(기본)|snapshot. --reconcile 이면 전체 스냅샷으로 드리프트 점검+치유.
    Stowplan {
        #[arg(long, default_value = "oracle-prod")]
        target: String,
        #[arg(long, default_value_t = false)]
        reconcile: bool,
    },
    /// Authoritative soon-idle labels: incrementally poll JOB_ORDER_HISTORY completions
    /// (JOBSTATUS='C') → tos_handover_label via etl_watermark. Run ~every 60s.
    Handover {
        #[arg(long, default_value = "oracle-prod")]
        target: String,
    },
    /// Yard-crane (RTG/ES) move stream from MCH_OPERATION → rtg_move_log (full work mix,
    /// not just DS). Incremental via etl_watermark. Run ~every 60s.
    RtgMoves {
        #[arg(long, default_value = "oracle-prod")]
        target: String,
    },
    /// Quay-crane (C/M/Z) move stream from MCH_OPERATION → qc_move_log (DS pickup + LD drop
    /// handovers; TRK_ID always present). Incremental via etl_watermark. Run ~every 60s.
    QcMoves {
        #[arg(long, default_value = "oracle-prod")]
        target: String,
    },
    /// Hourly terminal weather (Open-Meteo, no Oracle) → weather_hourly. A travel-time
    /// feature (rain/wind/visibility). Run ~hourly.
    Weather {},
    /// Live 1-minute weather (Tomorrow.io, no card) → weather_1min. Powers the live-map
    /// weather chip + the squall feature. Needs TOMORROW_API_KEY. Run ~every 3min.
    WeatherLive {},
}

fn parse_date(s: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(s, "%Y%m%d"))
        .with_context(|| format!("invalid date '{s}' (use YYYY-MM-DD or YYYYMMDD)"))
}

fn yesterday() -> NaiveDate {
    Local::now().date_naive().pred_opt().unwrap()
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Run { date, kpi, target } => {
            let date = match date {
                Some(d) => parse_date(&d)?,
                None => yesterday(),
            };
            let pool = db::pool().await?;
            run_kpi(&pool, &kpi, date, &target).await?;
            // A full nightly run rolls L0 up to L1, then recomputes the L2 baseline.
            if kpi == "all" {
                transform::run(&pool, date).await?;
                baseline::run(&pool, date).await?;
            }
        }
        Command::Tick { tier, target, shift } => {
            if shift {
                let pool = db::pool().await?;
                tt_extractor::shift::tick_shift(&pool, &target, &tier).await?;
                return Ok(());
            }
            let today = Local::now().date_naive();
            let kpis: &[&str] = match tier.as_str() {
                "t1" => &["k_mph_realtime", "k_qc_q", "k_tt_cycle"],
                "t2" => &["k_empty", "k_cycle", "k_crane_q", "k_crane_q_hour"],
                "all" => &["k_mph_realtime", "k_qc_q", "k_tt_cycle", "k_empty", "k_cycle", "k_crane_q", "k_crane_q_hour"],
                other => anyhow::bail!("unknown --tier '{other}' (t1|t2|all)"),
            };
            let pool = db::pool().await?;
            for k in kpis {
                if let Err(e) = run_kpi(&pool, k, today, &target).await {
                    tracing::error!(kpi = k, error = %e, "tick extract failed (continuing)");
                }
            }
            // recompute today's L1 as provisional, then refresh today's baseline
            transform::run_marked(&pool, today, true).await?;
            baseline::run(&pool, today).await?;
            tracing::info!(%tier, %today, "tick done");
        }
        Command::Backfill { from, to, target, sleep } => {
            let from = parse_date(&from)?;
            let to = parse_date(&to)?;
            anyhow::ensure!(from <= to, "from must be <= to");
            let pool = db::pool().await?;
            let mut day = from;
            let mut ok = 0u32;
            while day <= to {
                tracing::info!(%day, "backfill day");
                if let Err(e) = run_kpi(&pool, "all", day, &target).await {
                    tracing::error!(%day, error = %e, "backfill day failed (continuing)");
                } else {
                    transform::run(&pool, day).await?;
                    ok += 1;
                }
                day = day.succ_opt().unwrap();
                if day <= to && sleep > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(sleep)).await;
                }
            }
            tracing::info!(days_ok = ok, "backfill complete");
        }
        Command::Transform { date } => {
            let date = match date {
                Some(d) => parse_date(&d)?,
                None => yesterday(),
            };
            let pool = db::pool().await?;
            transform::run(&pool, date).await?;
            baseline::run(&pool, date).await?;
        }
        Command::Workpool { target } => {
            let pool = db::pool().await?;
            tt_extractor::workpool::tick_workpool(&pool, &target).await?;
        }
        Command::Stowplan { target, reconcile } => {
            let pool = db::pool().await?;
            if reconcile {
                tt_extractor::stowplan::reconcile_stowplan(&pool, &target).await?;
            } else {
                tt_extractor::stowplan::tick_stowplan(&pool, &target).await?;
            }
        }
        Command::Handover { target } => {
            let pool = db::pool().await?;
            tt_extractor::handover::tick_handover(&pool, &target).await?;
        }
        Command::RtgMoves { target } => {
            let pool = db::pool().await?;
            tt_extractor::rtg_moves::tick_rtg_moves(&pool, &target).await?;
        }
        Command::QcMoves { target } => {
            let pool = db::pool().await?;
            tt_extractor::qc_moves::tick_qc_moves(&pool, &target).await?;
        }
        Command::Weather {} => {
            let pool = db::pool().await?;
            tt_extractor::weather::tick_weather(&pool).await?;
        }
        Command::WeatherLive {} => {
            let pool = db::pool().await?;
            tt_extractor::weather::tick_weather_live(&pool).await?;
        }
    }
    Ok(())
}

async fn run_kpi(pool: &sqlx::PgPool, kpi: &str, date: NaiveDate, target: &str) -> Result<()> {
    // Each extract serializes Oracle access internally (single global lock). For
    // "all" we order cheap LOW-load sources first, then the heavier
    // JOB_ORDER_HISTORY queries, and continue past individual failures (PARTIAL run).
    macro_rules! step {
        ($name:expr, $fut:expr) => {{
            match $fut.await {
                Ok(n) => tracing::info!(kpi = $name, rows = n, "done"),
                Err(e) => {
                    tracing::error!(kpi = $name, error = %e, "extract failed");
                    if kpi != "all" {
                        return Err(e);
                    }
                }
            }
        }};
    }

    match kpi {
        "k_util_crane" => step!("k_util_crane", kpis::k_util_crane::extract(pool, date, target)),
        "k_mph_realtime" => step!("k_mph_realtime", kpis::k_mph_realtime::extract(pool, date, target)),
        "k_qc_q" => step!("k_qc_q", kpis::k_qc_q::extract(pool, date, target)),
        "qc_move_time" => step!("qc_move_time", kpis::qc_move_time::extract(pool, date, target)),
        "k_tt_cycle" => step!("k_tt_cycle", kpis::k_tt_cycle::extract(pool, date, target)),
        "k_empty" => step!("k_empty", kpis::k_empty::extract(pool, date, target)),
        "k_cycle" => step!("k_cycle", kpis::k_cycle::extract(pool, date, target)),
        "k_crane_q" => step!("k_crane_q", kpis::k_crane_q_daily::extract(pool, date, target)),
        "k_crane_q_hour" => step!("k_crane_q_hour", kpis::k_crane_q_hour::extract(pool, date, target)),
        "all" => {
            // LOW-load first
            step!("k_util_crane", kpis::k_util_crane::extract(pool, date, target));
            step!("k_mph_realtime", kpis::k_mph_realtime::extract(pool, date, target));
            step!("k_qc_q", kpis::k_qc_q::extract(pool, date, target));
            step!("qc_move_time", kpis::qc_move_time::extract(pool, date, target));
            step!("k_tt_cycle", kpis::k_tt_cycle::extract(pool, date, target));
            // heavier JOB_ORDER_HISTORY range scans
            step!("k_empty", kpis::k_empty::extract(pool, date, target));
            step!("k_cycle", kpis::k_cycle::extract(pool, date, target));
            step!("k_crane_q", kpis::k_crane_q_daily::extract(pool, date, target));
            step!("k_crane_q_hour", kpis::k_crane_q_hour::extract(pool, date, target));
        }
        other => anyhow::bail!(
            "unknown --kpi '{other}' (have: all, k_util_crane, k_mph_realtime, \
             k_empty, k_cycle, k_crane_q, k_crane_q_hour)"
        ),
    }
    Ok(())
}
