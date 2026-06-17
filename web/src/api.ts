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
}
export interface WpQc {
  qc: string; vessels: string[]; active_moves: number; remaining: number;
  queues: WpQueue[]; moves: WpMove[];
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
  laden_km: number | null; p25_s: number | null; p75_s: number | null;
  ds: number; ld: number; other: number; last_drop: string; first_drop: string;
}
export interface CycleBucket { t: string; n: number; }
export interface CycleSummary {
  hours: number; total_cycles: number; trucks: number;
  fleet_median_s: number | null; fleet_laden_km: number; cycles_per_hr: number;
  bucket_min: number; buckets: CycleBucket[]; trucks_list: CycleTruckAgg[];
}
export interface CycleRow {
  dropped_at: string; pickup_at: string | null; pickup_arrived_at: string | null; pickup_left_at: string | null; assigned_at: string | null; arrived_at: string | null;
  jobtype: string | null; vessel: string | null; voyage: string | null; container: string | null; qc: string | null;
  cycle_s: number | null; laden_leg_s: number | null; laden_leg_m: number | null;
  empty_leg_s: number | null; empty_leg_m: number | null; container_to_container: boolean;
  // v2 shadow 6-event model (null where unobserved / no v2 row)
  v2_opened_at: string | null; v2_empty_travel_start_at: string | null; v2_empty_arrived_at: string | null;
  v2_pickup_left_at: string | null; v2_laden_arrived_at: string | null;
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
  evaluated: number; mape_pct: number | null; median_abs_err_s: number | null; within_30pct: number | null;
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
}
export interface SoonIdleData {
  predictions: number; matched: number; precision_pct: number | null;
  by_source: SoonIdleSource[]; by_jobtype: SoonIdleRecall[]; lead_by_jobtype: SoonIdleLead[]; metric_series: SoonIdleMetricPoint[];
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
};
