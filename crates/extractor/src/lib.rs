//! Library surface of the extractor, so integration tests can exercise the
//! parse/upsert/run-log paths without the binary.

pub mod baseline;
pub mod crane_guard;
pub mod db;
pub mod handover;
pub mod kpis;
pub mod params;
pub mod qc_moves;
pub mod rtg_moves;
pub mod runner;
pub mod shift;
pub mod stowplan;
pub mod transform;
pub mod vessel;
pub mod weather;
pub mod workpool;
