//! scengen — ISOLATED simulation scenario/emulator collector + assembler.
//! Separate binary from the critical `extractor` on purpose (see lib.rs). Subcommands:
//!   collect  — continuous, watermark-incremental move-stream pull (systemd timer)
//!   assemble — on-demand LOCAL slice of a period -> scenario + emulator JSON (zero Oracle)
//!   backfill — bounded pull of a past window into scenario.move_hist

use anyhow::Result;
use clap::{Parser, Subcommand};

use scengen::{assemble, collect, db, serve, snapshot};

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
        Command::Assemble {} => assemble::run(&pool).await?,
        Command::Serve { port } => serve::run(pool, port).await?,
        Command::Backfill { from, to, target } => {
            tracing::info!(%from, %to, %target, "backfill: skeleton stub — not yet implemented");
        }
    }
    Ok(())
}
