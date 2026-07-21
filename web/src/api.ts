// Typed client for the Rust axum API. Shapes mirror crates/api/src/models.rs.

export interface KpiCard {
  key: string;
  name_en: string;
  name_ko: string;
  unit: string;
  tier: string | null;
  direction: "LOWER_BETTER" | "HIGHER_BETTER" | null;
  value: number | null;
  sample_n: number | null;
  is_provisional: boolean;
  as_of: string;
  baseline: number | null;
  baseline_n_days: number | null;
  delta_abs: number | null;
  delta_pct: number | null;
  p_value: number | null;
  cohens_d: number | null;
  is_significant: boolean | null;
  target: number | null;
  excellent: number | null;
  meets_target: boolean | null;
  meets_excellent: boolean | null;
  ds_cycle_s?: number | null;
  ld_cycle_s?: number | null;
}
export interface KpisResponse {
  as_of: string;
  period: string;
  range_from: string;
  range_to: string;
  prev_from: string;
  prev_to: string;
  kpis: KpiCard[];
}

export interface TrendPoint { date: string; value: number; sample_n: number | null; }
export interface TrendResponse { key: string; unit: string; target: number | null; baseline: number | null; points: TrendPoint[]; }

// KPI history matrix (by day / week / month)
export interface HistoryCell { value: number | null; sample_n: number | null; }
export interface HistoryColumn { key: string; name_en: string; name_ko: string; unit: string; direction: string | null; }
export interface HistoryBucket { bucket: string; label_from: string; label_to: string; is_provisional: boolean; cells: Record<string, HistoryCell>; }
export interface HistoryResponse { gran: string; kpis: HistoryColumn[]; buckets: HistoryBucket[]; }

export interface QcRow { qc: string; mph: number | null; qc_wait_sec: number | null; status: string | null; }
export interface BreakdownResponse { as_of: string; rows: QcRow[]; }

export interface FreshnessRow { source: string; last_status: string | null; last_success_date: string | null; is_stale: boolean; }
export interface HealthResponse { overall: string; postgres: string; sources: FreshnessRow[]; }

export interface LiveKpi {
  key: string; name_en: string; name_ko: string; unit: string;
  tier: string | null; direction: "LOWER_BETTER" | "HIGHER_BETTER" | null;
  value: number | null; sample_n: number | null;
  prev_value: number | null; delta_abs: number | null; delta_pct: number | null;
  target: number | null; excellent: number | null; meets_target: boolean | null;
  ds_cycle_s?: number | null; ld_cycle_s?: number | null;
}
export interface LiveResponse {
  business_date: string; shift: string; shift_name_ko: string; shift_name_en: string;
  window_start: string; as_of: string; elapsed_min: number; remaining_min: number;
  prev_shift: string; kpis: LiveKpi[];
}
export interface VesselQc {
  qc: string; moves: number | null; load_moves: number | null; discharge_moves: number | null; mph: number | null;
}
export interface VesselRow {
  vessel: string; voyage: string; qcs: string[]; qc_count: number | null;
  moves: number | null; load_moves: number | null; discharge_moves: number | null;
  mph: number | null; first_move: string | null; last_move: string | null;
  planned_moves: number | null; progress_pct: number | null;
  qc_rows: VesselQc[];
}
export interface VesselsResponse { shift: string; as_of: string; vessels: VesselRow[]; }

// Live work pool (per-QC sequence + active move front), from the 90s extractor snapshot.
export interface WpMove {
  qc: string | null; queuename: string; vessel: string; jobtype: string | null;
  yt_status: string | null; ytno: string | null; armgc: string | null;
  etw_ts: string | null; etw_accurate?: string | null; etw_expires?: string | null;
  actv_ts?: string | null;
  contno: string | null; yt_topos: string | null;
  from_pos: string | null; to_pos: string | null; twintandem: string | null;
}
export interface WpQueue {
  queuename: string; vessel: string; voyage: string | null; disload: string | null;
  seq: number | null; total: number; done: number; remaining: number;
  deadline_ts?: string | null; // SHADOW: when this bay must finish (deadline distribution)
  work_eta_ts?: string | null; // SHADOW: when the QC starts this bay (now + work before it)
  proc_s?: number | null;      // SHADOW: this bay's total processing seconds
}
export interface WpQc {
  qc: string; vessels: string[]; active_moves: number; remaining: number;
  queues: WpQueue[]; moves: WpMove[];
  estdep_ts?: string | null; work_left_s?: number | null; slack_s?: number | null; // SHADOW deadline
}
export interface WpCandidate {
  qc: string | null; queuename: string; vessel: string; jobtype: string | null;
  src_block: string | null; rtg: string | null; n: number;
  moves_until: number; active: boolean;
}
export interface WorkpoolResponse {
  as_of: string | null; qc_count: number; active_moves: number; total_remaining: number;
  qcs: WpQc[]; pool: WpMove[];
  candidates: WpCandidate[]; candidate_total: number;
}

// TT work-cycle history (from the accumulated tt_cycle_log).
export interface CycleTruckAgg {
  ytno: string; cycles: number; median_s: number | null; avg_s: number | null;
  drive_km: number | null; p25_s: number | null; p75_s: number | null;
  ds: number; ld: number; other: number; last_drop: string; first_drop: string;
}
export interface CycleBucket { t: string; n: number; }
export interface CycleSummary {
  hours: number; total_cycles: number; trucks: number;
  fleet_median_s: number | null; fleet_drive_km: number; cycles_per_hr: number;
  bucket_min: number; buckets: CycleBucket[]; trucks_list: CycleTruckAgg[];
}
// One physical trip. Cycle boundaries are TOS-authoritative (tt_move_log); the 7-phase durations are
// GPS-reconstructed (tt_cycle_recon) and reconcile exactly to cycle_s. gps_covered=false ⇒ no drive
// segment observed (GPS-silent / aged out of hifreq) — only cycle_s is meaningful, no split.
export interface CycleRow {
  dispatch_ts: string; pickup_ts: string | null; free_ts: string;
  jobtype: string | null; container: string | null; is_twin: boolean; n_containers: number;
  cycle_s: number;
  dispatch_wait_s: number; e_drive_s: number; e_stop_s: number; pickup_dwell_s: number;
  l_drive_s: number; l_stop_s: number; drop_dwell_s: number;
  e_drive_m: number; l_drive_m: number;
  gps_covered: boolean; n_fix: number; long_gap_s: number;
  pickup_crane: string | null; free_crane: string | null;
}
export interface CycleDetail { ytno: string; hours: number; cycles: CycleRow[]; }

// 학습 센터 — 블록 작업지점 좌표 모델(②)
export interface LearnToposPoint {
  topos: string; is_crane: boolean; lat: number; lon: number;
  n: number; obs: number; spread_m: number | null; updated_at: string;
}
export interface LearnMetricPoint {
  captured_at: string; distinct_topos: number; confident_topos: number;
  total_obs: number; median_spread_m: number | null;
}
export interface LearnTopos {
  distinct_topos: number; confident_topos: number; block_points: number;
  total_obs: number; median_spread_m: number | null;
  points: LearnToposPoint[]; metric_series: LearnMetricPoint[];
}
// 학습 센터 ③ — 차량 주행 차선
export interface LaneCellOut {
  lat: number; lon: number; passes: number;
  heading_deg: number | null; directionality: number | null; mean_speed: number | null;
}
export interface LaneMetricPoint {
  captured_at: string; cells: number; road_cells: number; total_passes: number; oneway_frac: number | null;
}
export interface LanesData {
  cells: number; road_cells: number; total_passes: number; oneway_frac: number | null;
  grid: LaneCellOut[]; metric_series: LaneMetricPoint[];
}
// 학습 센터 ① — TT 이동시간
export interface TravelOd {
  origin: string; dest: string; n: number;
  median_s: number | null; dist_m: number | null; speed_kmh: number | null;
}
export interface TravelMetricPoint {
  captured_at: string; samples: number; od_pairs: number; confident_pairs: number; median_speed_kmh: number | null;
}
export interface TravelAccuracy {
  evaluated: number; mape_pct: number | null; median_abs_err_s: number | null; mae_s: number | null; within_30pct: number | null;
}
export interface TravelData {
  samples: number; od_pairs: number; confident_pairs: number; median_speed_kmh: number | null;
  accuracy: TravelAccuracy; od: TravelOd[]; metric_series: TravelMetricPoint[];
}

// 학습 센터 ④ — Soon-idle 예측 정확도 (그림자: 예측 vs 권위 정답 comp_ts)
export interface SoonIdleSource {
  jobtype: string; source: string; predictions: number; matched: number;
  precision_pct: number | null; lead_p10_s: number | null; lead_p50_s: number | null; lead_p90_s: number | null;
}
export interface SoonIdleRecall {
  jobtype: string; truth_idles: number; predicted_any: number; predicted_gps: number;
  recall_pct: number | null; recall_gps_pct: number | null;
}
export interface SoonIdleMetricPoint {
  captured_at: string; jobtype: string; source: string; predictions: number; matched: number;
  precision_pct: number | null; recall_pct: number | null; lead_p50_s: number | null;
}
export interface SoonIdleLead {
  jobtype: string; matched: number; lead_p10_s: number | null; lead_p50_s: number | null; lead_p90_s: number | null;
  mape_pct: number | null; mae_s: number | null; within_30pct: number | null;
}
export interface SoonIdleEtaCell {
  dist_bin: number; source: string; n: number; pred_s: number | null; p10_s: number | null; p90_s: number | null;
}
export interface SoonIdleEtaModel {
  evaluated: number; feat_mape_pct: number | null; flat_mape_pct: number | null; feat_mae_s: number | null; within_30pct: number | null;
}
export interface SoonIdleData {
  predictions: number; matched: number; precision_pct: number | null;
  by_source: SoonIdleSource[]; by_jobtype: SoonIdleRecall[]; lead_by_jobtype: SoonIdleLead[];
  ds_eta: SoonIdleEtaModel; ds_eta_cells: SoonIdleEtaCell[]; metric_series: SoonIdleMetricPoint[];
}

export interface DispatchPredData {
  samples: number[]; resolved_total: number; distinct_cont: number;
  ds_eval: number; ds_med_err_min: number | null; ds_within10_pct: number | null;
  ld_eval: number; ld_med_err_min: number | null;
}

export interface LearnExtra {
  mm_legs: number; mm_saw_pct: number | null; mm_missed: number; mm_recoverable: number; mm_avg_prog: number | null;
  cyc_n: number; cyc_empty_miss_pct: number | null; cyc_laden_miss_pct: number | null; cyc_pickdone_pct: number | null;
  qc_total: number; qc_projected: number;
  s2_rows: number; s2_feasible_pct: number | null; s2_switched: number; s2_gap_pct: number | null;
  si_gate_m: number | null; si_gate_prec: number | null; si_gate_n: number; si_gate_nearmiss_n: number;
  fi_stages: { state: string; jobtype: string; n: number; med_rem_s: number }[];
}

export interface Stage2Match {
  ytno: string; qc: string | null; vessel: string | null; queuename: string | null;
  jobtype: string | null; src_block: string | null; veh_state: string | null;
  arrival_s: number | null; deadline_slack_s: number | null; feasible: boolean | null;
  cost_tier: string | null; switched: boolean | null;
}
export interface Stage2Shadow {
  summary: {
    matches_30m: number; switched_pct: number | null; feasible_pct: number | null;
    routed_pct: number | null; median_arrival_s: number | null; vehicles: number; works: number;
  };
  latest_ts: string | null;
  latest: Stage2Match[];
  inefficiency: { starve_ticks: number; with_free_pct: number | null; avg_free: number | null; qcs: number };
  solver: { ticks: number; savings_pct: number | null; greedy_miss: number | null; optimal_miss: number | null };
}
export interface HealthDispatch {
  up: boolean; last_tick_age_s: number | null; ticks_1h: number; matches_latest: number;
  thrash_pct: number | null; feasible_pct: number | null; savings_pct: number | null; routed_pct: number | null;
  arr_p50_s: number | null; arr_p90_s: number | null;
  arrival_hist: { label: string; n: number }[];
  trend: { hour: string; thrash_pct: number | null; matches: number }[];
  decisions: {
    ts: string; ytno: string; qc: string | null; queuename: string | null; jobtype: string | null;
    arrival_s: number | null; deadline_slack_s: number | null; feasible: boolean | null;
    cost_tier: string | null; switched: boolean | null;
  }[];
}
export interface DispatchCompare {
  summary: {
    n: number; divergence_pct: number | null; ours_faster_pct: number | null;
    avg_delta_s: number | null; median_delta_s: number | null;
    avg_our_arrival_s: number | null; avg_tos_arrival_s: number | null;
    same_n: number; ours_closer_n: number; tos_closer_n: number;
  };
  recent: {
    ts: string; qc: string; queuename: string; jobtype: string | null;
    tos_ytno: string; tos_arrival_s: number | null; our_ytno: string | null; our_arrival_s: number | null;
    agree: boolean | null; reason: string | null; delta_s: number | null;
  }[];
}
export interface WharfPoint {
  topos: string; lat: number; lon: number; n: number; spread_m: number | null;
}
export interface Stage2Advisory {
  ytno: string; qc: string | null; jobtype: string | null; src_block: string | null;
  dest_lat: number | null; dest_lon: number | null; src_lat: number | null; src_lon: number | null;
  arrival_s: number | null; feasible: boolean | null;
}
export interface ComparePick {
  qc: string; queuename: string; tos_ytno: string;
  our_ytno: string | null; our_arrival_s: number | null; tos_arrival_s: number | null;
  agree: boolean | null; delta_s: number | null;
}
export interface WorkPoint {
  qc: string; queuename: string; jobtype: string | null; lat: number; lon: number; src_block: string | null;
  tos_ytno: string | null; tos_arrival_s: number | null; our_ytno: string | null; our_arrival_s: number | null;
  agree: boolean | null; delta_s: number | null; avg_delta_s: number | null; n: number; agree_n: number;
  tos_trucks: string[]; our_trucks: string[];
}
export interface FairCompare {
  ts: string; window_min: number; n: number; tos_total_s: number; our_total_s: number; savings_pct: number; same_n: number;
}
export interface FairCompareOut { latest: FairCompare | null; avg_savings_pct: number | null; recent: FairCompare[]; }
// 학습 센터 — 데이터 수집 카탈로그(데이터 탭)
export interface DataStat { key: string; total: number; n_1h: number; n_24h: number; latest: string | null; }
export type DataRow = Record<string, string | number | boolean | null>;

export interface FairBucket { key: string; pairs: number; savings_pct: number | null; worse_pct: number | null; }
export interface FairBreakdown {
  by_job: FairBucket[]; by_hour: FairBucket[]; by_dist: FairBucket[]; by_crane: FairBucket[];
  pairs: number; worse_pct: number | null; same_pct: number | null; median_save_s: number | null; mean_save_s: number | null;
}

async function get<T>(path: string): Promise<T> {
  const r = await fetch(path);
  if (!r.ok) throw new Error(`${path}: ${r.status}`);
  return r.json() as Promise<T>;
}

export const api = {
  kpis: (period: string) => get<KpisResponse>(`/api/kpis?period=${encodeURIComponent(period)}`),
  trend: (key: string, opts?: { days?: number; from?: string; to?: string }) => {
    const qs = opts?.from && opts?.to ? `from=${opts.from}&to=${opts.to}` : `days=${opts?.days ?? 14}`;
    return get<TrendResponse>(`/api/kpis/${key}/trend?${qs}`);
  },
  breakdown: (period: string) => get<BreakdownResponse>(`/api/breakdown/qc?period=${encodeURIComponent(period)}`),
  kpiHistory: (gran: string, n?: number) => get<HistoryResponse>(`/api/kpis/history?gran=${gran}${n ? `&n=${n}` : ""}`),
  health: () => get<HealthResponse>("/api/health"),
  live: () => get<LiveResponse>("/api/live"),
  liveVessels: () => get<VesselsResponse>("/api/live/vessels"),
  workpool: () => get<WorkpoolResponse>("/api/workpool"),
  cycleSummary: (hours: number) => get<CycleSummary>(`/api/tt-cycles/summary?hours=${hours}`),
  cycleDetail: (ytno: string, hours: number, limit = 200) =>
    get<CycleDetail>(`/api/tt-cycles/detail?ytno=${encodeURIComponent(ytno)}&hours=${hours}&limit=${limit}`),
  learnTopos: () => get<LearnTopos>("/api/learn/topos"),
  learnLanes: () => get<LanesData>("/api/learn/lanes"),
  learnTravel: () => get<TravelData>("/api/learn/travel"),
  learnSoonIdle: () => get<SoonIdleData>("/api/learn/soon-idle"),
  learnDispatchPred: () => get<DispatchPredData>("/api/learn/dispatch-pred"),
  learnExtra: () => get<LearnExtra>("/api/learn/extra"),
  learnDataCatalog: () => get<DataStat[]>("/api/learn/data-catalog"),
  learnDataSample: (key: string) => get<DataRow[]>(`/api/learn/data-sample?key=${encodeURIComponent(key)}`),
  stage2Shadow: () => get<Stage2Shadow>("/api/stage2/shadow"),
  stage2Advisory: () => get<Stage2Advisory[]>("/api/stage2/advisory"),
  stage2ComparePicks: () => get<ComparePick[]>("/api/stage2/compare-picks"),
  stage2WorkPoints: () => get<WorkPoint[]>("/api/stage2/work-points"),
  dispatchCompare: () => get<DispatchCompare>("/api/stage2/compare"),
  stage2FairCompare: () => get<FairCompareOut>("/api/stage2/fair-compare"),
  stage2FairBreakdown: () => get<FairBreakdown>("/api/stage2/fair-breakdown"),
  livemapWharf: () => get<WharfPoint[]>("/api/livemap/wharf"),
  healthDispatch: () => get<HealthDispatch>("/api/health/dispatch"),
};
