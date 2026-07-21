//! On-demand assembly worker — LOCAL ONLY, zero Oracle. Picks up pending
//! scenario.assembly_job rows, slices scenario.move_hist (+ later: attrs/cells/snapshots)
//! for the requested window, and emits the scenario + emulator JSON. Skeleton stub for now.

use anyhow::Result;
use sqlx::PgPool;

pub async fn run(_pool: &PgPool) -> Result<()> {
    tracing::info!("assemble worker: skeleton stub — pending-job processing not implemented yet");
    // TODO(next step): claim pending scenario.assembly_job, slice scenario.move_hist for the
    // window, build scenario+emulator JSON (transform layer), write outputs + summary.
    Ok(())
}
