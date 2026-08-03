//! scengen — ISOLATED simulation scenario/emulator collector + assembler.
//! Separate binary from the critical `extractor` on purpose (see lib.rs). Subcommands:
//!   collect  — continuous, watermark-incremental move-stream pull (systemd timer)
//!   assemble — on-demand LOCAL slice of a period -> scenario + emulator JSON (zero Oracle)
//!   backfill — bounded pull of a past window into scenario.move_hist

use anyhow::Result;
use clap::{Parser, Subcommand};

use scengen::{assemble, collect, cont_spec, crane_deploy, db, enrich, gate, qc_plan, serve, snapshot, yard};

#[derive(Parser)]
#[command(
    name = "scengen",
    about = "Simulation scenario/emulator collector + assembler (isolated, non-critical)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Continuous incremental collector tick. Honors the kill switch + off-peak window.
    Collect {
        #[arg(long, default_value = "oracle-prod")]
        target: String,
    },
    /// Periodic as-of yard-occupancy snapshot (shift cadence) -> scenario.yard_snapshot.
    Snapshot {
        #[arg(long, default_value = "oracle-prod")]
        target: String,
    },
    /// Per-voyage enrichment (BAPLIE/MOVINS/vessel/berth) -> scenario.vessel_call + .container.
    Enrich {
        #[arg(long, default_value = "oracle-prod")]
        target: String,
    },
    /// Yard-crane (RTG) move stream with decoded stack position -> scenario.yard_move.
    YardMoves {
        #[arg(long, default_value = "oracle-prod")]
        target: String,
    },
    /// QC deployment history (crane<->vessel assignments) -> scenario.crane_deploy.
    CraneDeploy {
        #[arg(long, default_value = "oracle-prod")]
        target: String,
    },
    /// Landside gate transactions (intake/exit for the GI/GO containers we already have)
    /// -> scenario.gate_event.
    Gate {
        #[arg(long, default_value = "oracle-prod")]
        target: String,
    },
    /// Look up the ISO size of gate containers we do not know yet -> scenario.container_spec.
    /// Small and frequent on purpose: the yard inventory only holds a box while it is there.
    ContainerSpec {
        #[arg(long, default_value = "oracle-prod")]
        target: String,
    },
    /// Archive the quay-crane work plan per vessel call (LOCAL only, zero Oracle) -> scenario.qc_plan.
    /// Row-level revisions while a call is pre-berth, sealed once it berths. See mig 0110.
    QcPlan {},
    /// Recover the plan for calls the live path missed (first seen post-berth, or from before the
    /// archiver existed) by asking Oracle per (vessel, voyage) with the live query's time predicates
    /// removed. One batched round trip per invocation.
    PlanBackfill {
        #[arg(long, default_value = "oracle-prod")]
        target: String,
    },
    /// Reconstruct scenario.yard_cell by replaying yard_move (LOCAL only, zero Oracle).
    YardBuild {},
    /// On-demand assembly worker (local only, zero Oracle): pending jobs -> scenario+emulator JSON.
    Assemble {},
    /// Isolated monitor/control web service (own port): read scenario.* + enqueue jobs / kill switch.
    Serve {
        #[arg(long, default_value_t = 8899)]
        port: u16,
    },
    /// Backfill a past window into scenario.move_hist (bounded, throttled).
    Backfill {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long, default_value = "oracle-prod")]
        target: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let pool = db::pool().await?;
    match cli.command {
        Command::Collect { target } => collect::run(&pool, &target).await?,
        Command::Snapshot { target } => snapshot::run(&pool, &target).await?,
        Command::Enrich { target } => enrich::run(&pool, &target).await?,
        Command::YardMoves { target } => yard::run(&pool, &target).await?,
        Command::CraneDeploy { target } => crane_deploy::run(&pool, &target).await?,
        Command::Gate { target } => gate::run(&pool, &target).await?,
        Command::ContainerSpec { target } => cont_spec::run(&pool, &target).await?,
        Command::QcPlan {} => qc_plan::run(&pool).await?,
        Command::PlanBackfill { target } => qc_plan::backfill(&pool, &target).await?,
        Command::YardBuild {} => yard::build(&pool).await?,
        Command::Assemble {} => assemble::run(&pool).await?,
        Command::Serve { port } => serve::run(pool, port).await?,
        Command::Backfill { from, to, target } => {
            tracing::info!(%from, %to, %target, "backfill: skeleton stub — not yet implemented");
        }
    }
    Ok(())
}
