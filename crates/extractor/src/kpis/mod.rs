//! Per-KPI extract modules. Each owns its SQL template, row type, parse and upsert.

pub mod common;

pub mod k_crane_q_daily;
pub mod k_crane_q_hour;
pub mod k_cycle;
pub mod k_empty;
pub mod k_mph_realtime;
pub mod k_mph_voyage;
pub mod k_qc_q;
pub mod qc_move_time;
pub mod k_tt_cycle;
pub mod k_util_crane;
pub mod k_util_tt;
