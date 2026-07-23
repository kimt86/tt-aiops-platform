//! Shared types and logic for the tt-aiops-platform backend.
//!
//! - [`parse`]  — double-parse of the `remote-toolbox-sql` response.
//! - [`stats`]  — paired t-test and Cohen's d for KPI baselines.
//! - [`kpi`]    — the canonical set of KPI keys and their display units.

pub mod kpi;
pub mod parse;
pub mod shift;
pub mod stats;

pub use parse::{parse_rows, parse_values, ParseError};
pub use stats::{paired_t_test, PairedTest};
