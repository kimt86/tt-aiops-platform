//! JSON response shapes. `key` is the internal machine id; the UI renders `name_*`,
//! never the key.

use serde::Serialize;

#[derive(Serialize)]
pub struct KpiCard {
    pub key: String,
    pub name_en: String,
    pub name_ko: String,
    pub unit: String,
    pub tier: Option<String>,
    pub direction: Option<String>,
    pub value: Option<f64>,
    pub sample_n: Option<i32>,
    pub is_provisional: bool,
    pub as_of: String,
    pub baseline: Option<f64>,
    pub baseline_n_days: Option<i32>,
    pub delta_abs: Option<f64>,
    pub delta_pct: Option<f64>,
    pub p_value: Option<f64>,
    pub cohens_d: Option<f64>,
    pub is_significant: Option<bool>,
    pub target: Option<f64>,
    pub excellent: Option<f64>,
    pub meets_target: Option<bool>,
    pub meets_excellent: Option<bool>,
    /// per-jobtype TT cycle seconds (K_CYCLE only): discharge (DS) / load (LD)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ds_cycle_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ld_cycle_s: Option<f64>,
}

#[derive(Serialize)]
pub struct KpisResponse {
    pub as_of: String,
    pub period: String,
    pub range_from: String,
    pub range_to: String,
    pub prev_from: String,
    pub prev_to: String,
    pub kpis: Vec<KpiCard>,
}

#[derive(Serialize)]
pub struct TrendPoint {
    pub date: String,
    pub value: f64,
    pub sample_n: Option<i32>,
}

#[derive(Serialize)]
pub struct TrendResponse {
    pub key: String,
    pub unit: String,
    pub target: Option<f64>,
    pub baseline: Option<f64>,
    pub points: Vec<TrendPoint>,
}

// ---- KPI history matrix (by day / week / month) ----

#[derive(Serialize)]
pub struct HistoryColumn {
    pub key: String,
    pub name_en: String,
    pub name_ko: String,
    pub unit: String,
    pub direction: Option<String>,
}

#[derive(Serialize)]
pub struct HistoryCell {
    pub value: Option<f64>,
    pub sample_n: Option<i64>,
}

#[derive(Serialize)]
pub struct HistoryBucket {
    pub bucket: String,     // ISO date: day=that day, week=Monday, month=1st of month
    pub label_from: String, // inclusive range covered
    pub label_to: String,
    pub is_provisional: bool,
    pub cells: std::collections::HashMap<String, HistoryCell>, // keyed by KPI key
}

#[derive(Serialize)]
pub struct HistoryResponse {
    pub gran: String,
    pub kpis: Vec<HistoryColumn>,
    pub buckets: Vec<HistoryBucket>, // newest-first
}

#[derive(Serialize)]
pub struct QcRow {
    pub qc: String,
    pub mph: Option<f64>,
    pub qc_wait_sec: Option<f64>,
    pub status: Option<String>,
}

#[derive(Serialize)]
pub struct BreakdownResponse {
    pub as_of: String,
    pub rows: Vec<QcRow>,
}

#[derive(Serialize)]
pub struct StatsResponse {
    pub key: String,
    pub as_of: String,
    pub baseline: Option<f64>,
    pub baseline_n_days: Option<i32>,
    pub delta_abs: Option<f64>,
    pub delta_pct: Option<f64>,
    pub p_value: Option<f64>,
    pub cohens_d: Option<f64>,
    pub is_significant: Option<bool>,
}

#[derive(Serialize)]
pub struct FreshnessRow {
    pub source: String,
    pub last_status: Option<String>,
    pub last_success_date: Option<String>,
    pub is_stale: bool,
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub overall: String,
    pub postgres: String,
    pub sources: Vec<FreshnessRow>,
}

// ---- LIVE (current shift) ----

#[derive(Serialize)]
pub struct LiveKpi {
    pub key: String,
    pub name_en: String,
    pub name_ko: String,
    pub unit: String,
    pub tier: Option<String>,
    pub direction: Option<String>,
    pub value: Option<f64>,
    pub sample_n: Option<i32>,
    pub prev_value: Option<f64>,
    pub delta_abs: Option<f64>,
    pub delta_pct: Option<f64>,
    pub target: Option<f64>,
    pub excellent: Option<f64>,
    pub meets_target: Option<bool>,
    /// per-jobtype TT cycle seconds (K_CYCLE only): discharge (DS) / load (LD)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ds_cycle_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ld_cycle_s: Option<f64>,
}

#[derive(Serialize)]
pub struct LiveResponse {
    pub business_date: String,
    pub shift: String,
    pub shift_name_ko: String,
    pub shift_name_en: String,
    pub window_start: String, // HH:MM (terminal)
    pub as_of: String,        // HH:MM (terminal)
    pub elapsed_min: i64,
    pub remaining_min: i64,
    pub prev_shift: String,
    pub kpis: Vec<LiveKpi>,
}

#[derive(Serialize)]
pub struct VesselQc {
    pub qc: String,
    pub moves: Option<i32>,
    pub load_moves: Option<i32>,
    pub discharge_moves: Option<i32>,
    pub mph: Option<f64>,
}

#[derive(Serialize)]
pub struct VesselRow {
    pub vessel: String,
    pub voyage: String,
    pub qcs: Vec<String>,
    pub qc_count: Option<i32>,
    pub moves: Option<i32>,
    pub load_moves: Option<i32>,
    pub discharge_moves: Option<i32>,
    pub mph: Option<f64>,
    pub first_move: Option<String>,
    pub last_move: Option<String>,
    pub planned_moves: Option<i32>,
    pub progress_pct: Option<f64>,
    pub qc_rows: Vec<VesselQc>,
    pub departed: bool,
}

#[derive(Serialize)]
pub struct VesselsResponse {
    pub shift: String,
    pub as_of: String,
    pub vessels: Vec<VesselRow>,
}
