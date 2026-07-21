//! scengen — ISOLATED simulation scenario/emulator collector + assembler.
//!
//! Deliberately a SEPARATE binary from the critical `extractor`: a fault here (panic,
//! hang, or even a compile error) must never affect the dispatch/dashboard extraction.
//! It shares only `wp-core` types and the `remote-toolbox-sql` script (which serializes
//! Oracle access across processes). Control is decoupled through Postgres `scenario.*`
//! tables — the UI never drives this process directly.
//!
//! Subsystems:
//!   collect  — continuous, watermark-incremental pull of the move stream -> scenario.move_hist
//!   assemble — on-demand, LOCAL-ONLY slice of a period -> scenario + emulator JSON (zero Oracle)
//!   state    — the observe/command contract (run-state, events, watermark, config/kill-switch)
//!   toolbox  — isolated Oracle gateway (own copy, shorter timeout)

pub mod assemble;
pub mod collect;
pub mod db;
pub mod enrich;
pub mod serve;
pub mod snapshot;
pub mod state;
pub mod toolbox;
pub mod util;
pub mod yard;
