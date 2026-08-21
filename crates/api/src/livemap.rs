//! Live-map GPS ingest. Connects OUT to the WP-TT GPS websocket — reachable here
//! only through the local SSH tunnel `127.0.0.1:9986` -> azure-wp-poc -> 172.21.30.72:9986
//! (the source is a WSL2 NAT IP unreachable directly). Performs the `wpt_gps` zone
//! handshake (identify -> 2s -> checkin), then keeps the latest fix per device in an
//! in-memory map plus ingest health counters.
//!
//! - `GET /api/livemap/positions` — snapshot the LiveMap polls (active devices).
//! - `GET /api/livemap/health`    — ingest/feed health (connection, freshness, rate).
//!
//! NOTE: this is the ONE outbound network client in the API crate, and it talks ONLY to
//! the local tunnel endpoint — no Oracle/SSH access, cannot reach the production DB.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::Json;
use sqlx::PgPool;
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::sync::{Mutex, RwLock};
use tokio_tungstenite::tungstenite::Message;

/// Devices fresher than this are "active" and served to the map.
const STALE_AFTER_S: i64 = 120;
/// Fixes older than this are dropped entirely (a device that left the yard).
const LOST_AFTER_S: i64 = 600;
/// Freshness band: a fix newer than this is "fresh".
const FRESH_UNDER_S: i64 = 15;
const SPARK_MIN: usize = 30; // minutes of throughput history

#[derive(Clone, Default)]
struct Pos {
    cls: String, // device-id alpha prefix: TT / RTG / C / TC / ...
    lat: f64,
    lon: f64,
    speed: f64,  // km/h
    engine: i32, // 1 = engine_on contains "ON", else 0
    last_seen_ms: i64,
    // rich fields straight off the gps_update (mostly populated for TT prime movers)
    jobtype: Option<String>,
    vslname: Option<String>,
    container1: Option<String>,
    container2: Option<String>,
    cur_loc: Option<String>,
    topos1: Option<String>,
    arrival: Option<String>, // "ARRIVED" when the TT has reached its handover point
    fuel: Option<f64>,
    accuracy: Option<f64>,
    userid: Option<String>,
    batt: Option<String>,
    nett: Option<String>,
    dtime: Option<String>,
    distance: Option<f64>,
    // TT cycle tracking (carried across fixes). A delivery completes when `container1`
    // changes away from a non-empty value — i.e. nonempty→empty OR nonempty→other (a
    // truck often goes container A→container B with no empty gap, so a loaded→EMPTY-only
    // edge misses ~3/4 of deliveries; observed ~574/hr vs ~90/hr for the empty-only edge).
    // The interval between a truck's consecutive deliveries, capped, is one cycle. The
    // values are exposed via /api/livemap/positions (see the KC websocket-kpi-accuracy doc).
    carry_since_ms: i64, // when the current non-empty container1 began (0 if empty)
    last_drop_ms: i64,   // last counted delivery
    carry_trip_m: f64,   // path length the truck has driven since the current carry began
    // ── per-truck cycle state machine (for the persisted tt_cycle_log + idle→staging) ──
    // Latched job fields: kept across heartbeats that OMIT the field (the raw container1/
    // jobtype/etc. above are cleared per-message). A latch updates only on a present,
    // non-empty value, so an intermittent feed doesn't drop the truck's job. Cleared on a
    // validated cycle completion (container) or when the work pool drops the truck (A4).
    latched_container: Option<String>,
    latched_jobtype: Option<String>,
    latched_vessel: Option<String>,
    latched_topos: Option<String>,
    assigned: bool,        // authoritative: in live_assigned_tt (refreshed by spawn_assignment_refresh)
    empty_since_ms: i64,   // when the current empty leg began (0 if loaded)
    empty_trip_m: f64,     // path length driven while empty (assignment→pickup leg)
    empty_arrived_ms: i64, // when an empty assigned truck first reached ARRIVED at its pickup (0 if none)
    cycle_open: Option<OpenCycle>, // accumulating metadata for the in-progress cycle
    // ── v2 SHADOW (leg-based phases, design: docs/cycle-detection-v2-design.md) ──
    // Parsed-but-not-yet-used-by-v1 feed fields + the parallel leg tracker. None of the
    // v1 logic reads these.
    arr_dtime_ms: i64,     // parsed `arr_dtime` (HH:MM:SS @ terminal tz) as epoch ms, 0 if absent
    latched_topos2: Option<String>,
    v2: V2State,
}

/// In-progress cycle for one truck — opened at pickup (container1 empty→non-empty), enriched
/// from the latched job fields + work-pool cache, finalized into a `CompletedCycle` on the
/// validated drop edge (the existing `fleet_drop`). Phase timestamps are 0 when not observed.
#[derive(Clone, Default)]
struct OpenCycle {
    assigned_at_ms: i64,
    pickup_arrived_at_ms: i64,
    pickup_left_at_ms: i64,
    pickup_at_ms: i64,
    arrived_at_ms: i64,
    // SHADOW (observational): crane-side arrival from GPS proximity / PLC. Does not feed the
    // live phase timestamps — written to separate columns for validation.
    pickup_arrived_crane_ms: i64,
    arrived_crane_ms: i64,
    crane_arr_method: Option<&'static str>,
    idle_before_ms: i64,
    empty_leg_ms: i64,
    empty_leg_m: f64,
    jobtype: Option<String>,
    vessel: Option<String>,
    voyage: Option<String>,
    container: Option<String>,
    qc: Option<String>,
    twintandem: Option<String>,
}

/// A finalized cycle queued for the flusher to persist into `tt_cycle_log`.
#[derive(Clone)]
struct CompletedCycle {
    ytno: String,
    assigned_at_ms: i64,
    pickup_arrived_at_ms: i64,
    pickup_left_at_ms: i64,
    pickup_at_ms: i64,
    arrived_at_ms: i64,
    pickup_arrived_crane_ms: i64,
    arrived_crane_ms: i64,
    crane_arr_method: Option<&'static str>,
    dropped_at_ms: i64,
    idle_before_ms: i64,
    empty_leg_ms: i64,
    empty_leg_m: f64,
    laden_leg_ms: i64,
    laden_leg_m: f64,
    jobtype: Option<String>,
    vessel: Option<String>,
    voyage: Option<String>,
    container: Option<String>,
    qc: Option<String>,
    twintandem: Option<String>,
    container_to_container: bool,
}

/// v2 SHADOW leg tracker. A leg = one topos1 target: assignment → arrival → handover →
/// departure. The cycle = the legs between two validated drops. Runs in parallel with the
/// v1 machine and writes only to tt_cycle_v2 (see the design doc).
#[derive(Clone, Default)]
struct V2State {
    opened_ms: i64,       // first assignment signal after the previous validated drop (사이클시작)
    empty_travel_start_ms: i64, // 공차이동시작: first movement after open, before reaching pickup
    jobtype: Option<String>, // snapshotted at OPEN (close-time latched would be the NEXT job's on c2c)
    legs: Vec<V2Leg>,     // closed legs of the open cycle (capped)
    cur: Option<V2Leg>,   // the in-progress leg
}
#[derive(Clone)]
struct V2Leg {
    target: String,
    crane: bool,
    assigned_ms: i64,
    arrived_ms: i64,
    arr_src: &'static str, // arr_dtime|arrived|cur_loc|gps|pre_positioned
    left_ms: i64,
    // truck GPS where this leg's arrival was recorded (0.0 = uncaptured). For quay-zone
    // gridding: a QC id is not a fixed location (cranes roam the rail), so the physical
    // handover coordinate is the stable anchor. Block legs use logical codes instead.
    arrived_lat: f64,
    arrived_lon: f64,
}
const V2_LEGS_MAX: usize = 6;

/// A finalized v2 cycle queued for the flusher.
#[derive(Clone)]
struct CompletedV2 {
    ytno: String,
    dropped_ms: i64,
    opened_ms: i64,
    empty_travel_start_ms: i64, // 공차이동시작
    jobtype: Option<String>,
    legs: Vec<V2Leg>,
    // v2.4: v1's robust continuous-tracker arrivals for the SAME (ytno, dropped_at), used at
    // flush to backfill a block arrival the leg model missed (leg-formation gap). Keeps v2
    // capture ≥ v1 without touching the leg model. 0 = v1 had none either.
    v1_pickup_arrived_ms: i64,
    v1_drop_arrived_ms: i64,
}

/// Authoritative assignment snapshot for one truck, cached from the work pool
/// (live_assigned_tt ⨝ live_workpool) and refreshed every ~30s. The boolean assignment
/// (any active job, all job types) comes from live_assigned_tt; the metadata is the
/// best-available DS/LD work-pool row (NULL for yard moves not in live_workpool).
#[derive(Clone, Default)]
struct AssignedJob {
    jobtype: Option<String>,
    vessel: Option<String>,
    voyage: Option<String>,
    contno: Option<String>,
    qc: Option<String>,
    twintandem: Option<String>,
    // TOS says the order's crane (RTG for DS) is active on this move (JOB_ODR_ACTV_DT set).
    // Authoritative soon-idle signal for DS, where the websocket has no RTG PLC.
    rtg_active: bool,
}

// TT cycle bounds. A loose physical sanity band only — the *real* artifact filter is GPS
// movement (MIN_CARRY_TRIP_M below), not duration. We keep [2,20]m so a single absurd
// interval (clock skew, a >20m idle gap that isn't one cycle) can't poison the median, but
// we do NOT use the lower bound to manufacture a "realistic" number — see the movement
// filter, which is what actually separates a real delivery from a TOS re-assignment.
const MIN_CYCLE_S: i64 = 120;
const MAX_CYCLE_S: i64 = 1200;
// a delivery only counts if the container was actually carried ≥30s (filters a flicker).
const MIN_LOADED_MS: i64 = 30_000;
// the principled artifact filter (per external eval): a *real* laden delivery means the
// truck physically drove the container from pickup to handover. If `container1` changes but
// the truck accumulated < this much path length while carrying it, the truck never moved —
// it is TOS pre-assigning / rewriting the container field while the truck sits, NOT a
// delivery. Validating on movement (not on a duration threshold) avoids circularly using a
// "cycles should be long" prior to inflate the median. ~150m clears GPS jitter and is below
// even the shortest real quay↔block haul.
const MIN_CARRY_TRIP_M: f64 = 150.0;
// a reject whose carried path falls in this near-miss band [100,150)m is tracked separately:
// it is exposed so a reviewer can see how many genuine ultra-short hauls the 150m cut might
// be discarding (the one direction this filter can bias the median upward).
const NEAR_TRIP_M: f64 = 100.0;
// guard: ignore a single inter-fix jump larger than this when accumulating path length
// (GPS teleport / accuracy spike), so jitter can't fake "movement".
const MAX_FIX_STEP_M: f64 = 600.0;

/// 위치 이력 한 틱에 쓸 수 있는 최대 행수.
///
/// 세 개의 위치 이력 기록기는 "지금 지도에 있는 기기 수"만큼 행을 쓴다. 기기 지도는 10분
/// 창으로 정리되므로 무한히 자라진 않지만, 손상된 기기 ID가 쏟아지면 그 창 안에서 얼마든지
/// 부풀 수 있고 그만큼이 3초마다 DB로 나간다. 이건 07-28 사고와 같은 유형(미검증 입력이
/// 쓰기 행수를 정함)이고, 실행자가 Postgres 라 프로세스 메모리 상한이 듣지 않는 쪽이다.
/// 실측 2026-08-02(틱당 행수 최대/평균): truck_pos_hist 253/185 · truck_pos_hifreq 123/46.
/// 물리 천장은 동시 관측 대수가 아니라 120초 창에 보고한 서로 다른 TT 수이고 하루 최대 532대다.
/// 2,000 은 그 천장의 3.8배 — 넘으면 쓰기를 멈추고 알림을 올린다(조용히 자르면 어떤 기기가
/// 빠졌는지 나중에 알 수 없고, 부분집합은 이 표를 읽는 그림자 분석을 그럴듯하게 오염시킨다).
const POS_WRITE_MAX: usize = 2_000;
/// RTG/ES 전용 상한. TT 로 잰 값을 그대로 쓰면 안 된다 — 이 모집단은 실측 틱당 최대 8행·
/// 기기 30대라 2,000 은 250배 헐거워 **어떤 폭주도 못 잡는다**(측정하지 않은 모집단에 남의
/// 상한을 적용한 셈). 200 은 기기 수 기준 6.7배 여유.
const RTG_WRITE_MAX: usize = 200;

/// UPSTREAM COORDINATE GATE.
///
/// 2026-07-28: one corrupt fix (lat 4.1975 / lon 99.7827, 219km out) sized the road-inference
/// raster 3,374x and OOM-killed the box. Every >50km fix in that window was a TT, and
/// truck_pos_hist/hifreq — the only tables the raster reads — hold TT and nothing else
/// (1.03M / 6.33M rows, zero other classes). So TT is the population this gate bounds.
///
/// ⚠ THE FIRST VERSION OF THIS GATE WAS WRONG and this is why the split exists. It applied a
/// TT-derived radius (25km) to every device class, and silently dropped live H-* external
/// hauliers reporting from 25.66km — 57 real fixes in 51 minutes before it was caught. The
/// footprint had been measured on a TT-only table; applying it to classes absent from that
/// sample invented a limit for them. A bound is only valid for the population it was measured on.
///
/// TT distance from the anchor, all 6,331,399 fixes:
///   <5km 6,331,239 (99.9975%, max 4,959m) · 5-10km 31 · 10-20km 92
///   · 20-30km 5 · 30-50km 12 · >50km 20 (89km..219km — the corrupt ones) · p99.99 3,032m
/// 20km keeps 99.9997% of real TT and sits 4.5x inside the nearest corrupt fix. It equals
/// MAX_R_M in infer_road_network.py, so the gate and the raster agree on what a TT can be.
const TERMINAL_LAT: f64 = 2.928;
const TERMINAL_LON: f64 = 101.2927;
/// Hard bound, TT only — a yard tractor does not leave the terminal.
const TT_MAX_R_M: f64 = 20_000.0;
/// ADVISORY ONLY, for classes with no measured footprint (external hauliers, cranes, stackers).
/// Counted and logged so the data needed to bound them accumulates — never dropped. Guessing a
/// bound for an unmeasured population is exactly what broke the live map above.
const UNMEASURED_ADVISORY_R_M: f64 = 50_000.0;

fn dist_from_terminal_m(lat: f64, lon: f64) -> f64 {
    let dn = (lat - TERMINAL_LAT) * 111_320.0;
    let de = (lon - TERMINAL_LON) * 111_320.0 * TERMINAL_LAT.to_radians().cos();
    (dn * dn + de * de).sqrt()
}

/// What to do with one incoming fix.
pub(crate) enum FixGate {
    Keep,
    /// kept, but far enough out to be worth counting — see UNMEASURED_ADVISORY_R_M
    KeepButFar(f64),
    Drop(&'static str),
}

/// Gate one fix. Drops only what we can justify: non-finite values, and TT beyond its measured
/// physical range. Everything else is kept — see the ⚠ note on TT_MAX_R_M.
pub(crate) fn gate_fix(cls: &str, lat: f64, lon: f64) -> FixGate {
    if !lat.is_finite() || !lon.is_finite() {
        // a single NaN/Inf poisons every downstream min/max, mean and raster bound
        return FixGate::Drop("non-finite coordinate");
    }
    let d = dist_from_terminal_m(lat, lon);
    if cls == "TT" {
        if d > TT_MAX_R_M {
            return FixGate::Drop("TT beyond its measured physical range");
        }
        return FixGate::Keep;
    }
    if d > UNMEASURED_ADVISORY_R_M {
        return FixGate::KeepButFar(d);
    }
    FixGate::Keep
}
// the median needs a non-trivial sample before it is shown — a 5-sample median is noise.
// Below this the UI shows "collecting n/N" instead of a number (per external eval #4).
const MIN_CYCLE_SAMPLES: usize = 20;
// a working quay crane idle longer than this with no TT present ≈ likely waiting for a
// truck. Set past a normal inter-move gap (~90–120s) so routine gaps don't trip it.
const QCQ_IDLE_S: i64 = 120;

/// Crane PLC state from the `ctab` zone (`plc_data`). Dynamic equipment only
/// (C/M/Z prefixes). Keyed by crane id, which matches the GPS device id.
///
/// We also count *completed moves* from the hook-load signal: each laden→empty
/// transition (the crane set a container down and released) is one move. Counting
/// those over a rolling hour gives a live, per-second-fresh per-QC throughput
/// (move/hr) — a websocket cross-check that refines the coarse TOS K_MPH (whose
/// active_hours is bucketed to whole hours). See `kc/websocket-kpi-accuracy`.
#[derive(Clone, Default)]
struct Plc {
    load_t: Option<f64>, // hook load in metric tons
    lock: Option<bool>,
    land: Option<bool>,
    hpos: Option<f64>, // hoist position (crane-local axis)
    tpos: Option<f64>, // trolley position
    last_seen_ms: i64,
    laden: bool,             // current laden state (hysteresis-debounced)
    moves: VecDeque<i64>,    // pickup (rising-edge) timestamps, pruned to 1h
    last_move_ms: i64,
}

// Hook-load thresholds (tons). Empty hook reads ~0 / slightly negative; a loaded
// spreader reads several tons. Hysteresis (laden ≥2t / empty <0.5t) keeps the state
// from flapping. We count a move on the empty→laden RISING edge (a pickup), and a
// min gap of 40s between counted moves absorbs any mid-cycle flicker (one spreader
// cycle can't complete in <40s) while still counting every real move (cycles are
// ~60–120s apart). Rising-edge + gap is robust to a noisy load signal.
const PLC_LADEN_T: f64 = 2.0;
const PLC_EMPTY_T: f64 = 0.5;
const MIN_MOVE_GAP_MS: i64 = 40_000;
const MOVE_WINDOW_MS: i64 = 3_600_000; // 1 hour → move count == move/hr

/// Learned position of a yard block/bay code, accumulated from the GPS of TTs observed
/// ARRIVED there. Lets us estimate "how far is an empty TT from its assigned pickup" with
/// no TOS/layout dependency (standalone).
#[derive(Clone, Copy, Default)]
pub struct Centroid {
    lat: f64,
    lon: f64,
    n: u32,       // capped sample weight (≤500) — mild adaptivity of the mean
    obs: u64,     // total observations ever (uncapped) — accumulation count
    var_lat: f64, // EWMA variance (matches the capped mean) → spread/precision
    var_lon: f64,
}
const CENTROID_GATE_M: f64 = 300.0; // reject a block work-point sample >this from the running centroid
const CENTROID_GATE_MIN_N: u32 = 10; // ...but only once the centroid has settled
impl Centroid {
    fn push(&mut self, lat: f64, lon: f64) {
        self.obs += 1;
        self.n = (self.n + 1).min(500); // cap so it stays mildly adaptive
        let k = 1.0 / self.n as f64;
        let d_lat = lat - self.lat; // delta to the OLD mean (for EWMA variance)
        let d_lon = lon - self.lon;
        self.lat += d_lat * k;
        self.lon += d_lon * k;
        self.var_lat = (1.0 - k) * (self.var_lat + k * d_lat * d_lat);
        self.var_lon = (1.0 - k) * (self.var_lon + k * d_lon * d_lon);
    }
    /// Like `push`, but drops a sample lying more than `gate_m` from the established centroid — a
    /// cross-block mislabel (stale topos1) that would otherwise drag the mean and blow up spread.
    /// Only gates once the centroid has settled (≥ CENTROID_GATE_MIN_N samples).
    fn push_gated(&mut self, lat: f64, lon: f64, gate_m: f64) {
        if self.n >= CENTROID_GATE_MIN_N && dist_m((self.lat, self.lon), (lat, lon)) > gate_m {
            return;
        }
        self.push(lat, lon);
    }
    /// spatial spread (m) ≈ √(var_lat + var_lon), the model's precision at this point.
    fn spread_m(&self) -> f64 {
        if self.n < 2 {
            return 0.0;
        }
        let m_lat = self.var_lat.sqrt() * 111_320.0;
        let m_lon = self.var_lon.sqrt() * 111_320.0 * self.lat.to_radians().cos();
        (m_lat * m_lat + m_lon * m_lon).sqrt()
    }
}

// ── lane learning (③): moving-TT GPS traces → grid cells with traffic + direction ──
const LANE_CELL_DEG: f64 = 0.0002; // ~22m grid
const LANE_MIN_SPEED_KMH: f64 = 5.0;
const LANE_MIN_M: f64 = 5.0; // min step to take a heading sample
const LANE_MAX_M: f64 = 200.0; // reject GPS jumps

/// One grid cell of the learned driving-lane network: traffic + circular-mean heading
/// (→ direction & one-way/two-way) + mean speed. Accumulated from moving-TT GPS traces.
#[derive(Clone, Copy, Default)]
pub struct LaneCell {
    passes: u64,
    sum_sin: f64, // Σ sin(bearing) — circular accumulation (handles 0/360 wrap)
    sum_cos: f64, // Σ cos(bearing)
    sum_speed: f64,
}
impl LaneCell {
    fn push(&mut self, bearing: f64, speed_kmh: f64) {
        self.passes += 1;
        let r = bearing.to_radians();
        self.sum_sin += r.sin();
        self.sum_cos += r.cos();
        self.sum_speed += speed_kmh;
    }
    fn heading_deg(&self) -> f64 {
        (self.sum_sin.atan2(self.sum_cos).to_degrees() + 360.0) % 360.0
    }
    /// 0..1: resultant length / passes. ~1 = consistent one-way, ~0 = two-way/mixed.
    fn directionality(&self) -> f64 {
        if self.passes == 0 {
            return 0.0;
        }
        (self.sum_sin * self.sum_sin + self.sum_cos * self.sum_cos).sqrt() / self.passes as f64
    }
    fn mean_speed(&self) -> f64 {
        if self.passes == 0 {
            0.0
        } else {
            self.sum_speed / self.passes as f64
        }
    }
}

/// Initial bearing (deg, 0..360) from a→b.
fn bearing_deg(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (lat1, lat2) = (a.0.to_radians(), b.0.to_radians());
    let dlon = (b.1 - a.1).to_radians();
    let y = dlon.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlon.cos();
    (y.atan2(x).to_degrees() + 360.0) % 360.0
}

/// Per-minute throughput ring for the health sparkline.
struct Ring {
    minute: i64,
    buf: [u32; SPARK_MIN],
    idx: usize,
}
impl Ring {
    fn new() -> Self {
        Self { minute: 0, buf: [0; SPARK_MIN], idx: 0 }
    }
    fn advance(&mut self, m: i64) {
        if self.minute == 0 {
            self.minute = m;
            return;
        }
        while self.minute < m {
            self.idx = (self.idx + 1) % SPARK_MIN;
            self.buf[self.idx] = 0;
            self.minute += 1;
        }
    }
    fn bump(&mut self, m: i64) {
        self.advance(m);
        self.buf[self.idx] += 1;
    }
    fn series(&self) -> Vec<u32> {
        (1..=SPARK_MIN).map(|k| self.buf[(self.idx + k) % SPARK_MIN]).collect()
    }
    /// Display rate: the larger of the current (still-filling) and previous minute, so a
    /// busy feed reads a sane number immediately instead of 0 at each minute boundary.
    fn rate(&self) -> u32 {
        let prev = self.buf[(self.idx + SPARK_MIN - 1) % SPARK_MIN];
        self.buf[self.idx].max(prev)
    }
}

// ── QC handover work-point: quay work-line + per-crane recency centroid ────────────────
// A QC's GPS is spreader-mounted → it swings ship↔truck ~26m every crane cycle, and its
// all-time centroid also smears the crane's whole gantry travel (~185m). Trucks don't swing
// and their ARRIVED positions lie on ONE quay line (±11m, measured 2026-07). So: learn that
// line, keep a recency-decayed centroid of recent handovers per crane, and project it onto the
// line → the current handover point (~11-15m) that tracks the gantry with the swing removed.
const CRANE_WP_TAU_S: f64 = 900.0; // ~15-min recency half-life for the per-crane centroid
const REF_LAT: f64 = 2.926; // terminal-local metre origin (keeps the PCA well-conditioned)
const REF_LON: f64 = 101.29;
fn to_local(lat: f64, lon: f64) -> (f64, f64) {
    let mlon = 111_320.0 * REF_LAT.to_radians().cos();
    ((lon - REF_LON) * mlon, (lat - REF_LAT) * 111_320.0)
}
fn from_local(x: f64, y: f64) -> (f64, f64) {
    let mlon = 111_320.0 * REF_LAT.to_radians().cos();
    (REF_LAT + y / 111_320.0, REF_LON + x / mlon) // (lat, lon)
}

/// Total-least-squares (PCA) line fit, accumulated from crane truck-ARRIVED positions (local m).
#[derive(Clone, Copy, Default)]
struct QuayLine {
    n: f64,
    sx: f64,
    sy: f64,
    sxx: f64,
    syy: f64,
    sxy: f64,
}
impl QuayLine {
    fn push(&mut self, x: f64, y: f64) {
        self.n += 1.0;
        self.sx += x;
        self.sy += y;
        self.sxx += x * x;
        self.syy += y * y;
        self.sxy += x * y;
    }
    /// (point_x, point_y, unit_dir_x, unit_dir_y) of the fitted line; None until it's confident.
    fn fit(&self) -> Option<(f64, f64, f64, f64)> {
        if self.n < 50.0 {
            return None;
        }
        let n = self.n;
        let (mx, my) = (self.sx / n, self.sy / n);
        let cxx = self.sxx / n - mx * mx;
        let cyy = self.syy / n - my * my;
        let cxy = self.sxy / n - mx * my;
        let tr = cxx + cyy;
        let disc = (tr * tr / 4.0 - (cxx * cyy - cxy * cxy)).max(0.0).sqrt();
        let l1 = tr / 2.0 + disc; // major eigenvalue = variance along the line
        if l1 < 400.0 {
            return None; // <~20 m along-span → not yet a line
        }
        let (mut dx, mut dy) = if cxy.abs() > 1e-9 {
            (l1 - cyy, cxy)
        } else if cxx >= cyy {
            (1.0, 0.0)
        } else {
            (0.0, 1.0)
        };
        let norm = (dx * dx + dy * dy).sqrt();
        if norm < 1e-9 {
            return None;
        }
        dx /= norm;
        dy /= norm;
        Some((mx, my, dx, dy))
    }
    /// Project a local point onto the fitted line (identity until the line is confident).
    fn project(&self, x: f64, y: f64) -> (f64, f64) {
        match self.fit() {
            Some((mx, my, dx, dy)) => {
                let t = (x - mx) * dx + (y - my) * dy;
                (mx + t * dx, my + t * dy)
            }
            None => (x, y),
        }
    }
}

/// Per-crane time-decayed centroid of recent truck-ARRIVED (handover) positions, in local m.
#[derive(Clone, Copy, Default)]
struct CraneWp {
    last_ms: i64,
    wx: f64,
    wy: f64,
    w: f64,
    obs: u64,
}
impl CraneWp {
    fn push(&mut self, x: f64, y: f64, now_ms: i64) {
        if self.w > 0.0 && self.last_ms > 0 {
            let dt = ((now_ms - self.last_ms).max(0) as f64) / 1000.0;
            let decay = (-dt / CRANE_WP_TAU_S).exp();
            self.wx *= decay;
            self.wy *= decay;
            self.w *= decay;
        }
        self.wx += x;
        self.wy += y;
        self.w += 1.0;
        self.last_ms = now_ms;
        self.obs += 1;
    }
    fn centroid(&self) -> Option<(f64, f64)> {
        (self.w > 0.3).then(|| (self.wx / self.w, self.wy / self.w))
    }
}

/// Resolve each crane to its handover WORK-POINT for dispatch: the recency centroid projected
/// onto the quay line (swing-free, gantry-tracking); else the live GPS projected onto the line
/// (strips the spreader swing) for cranes with no recent handover. classify_tt still falls back
/// to the plain `centroids` when a crane has neither.
fn resolve_crane_wp(
    line: &QuayLine,
    cwp: &HashMap<String, CraneWp>,
    live: &HashMap<String, (f64, f64)>,
) -> HashMap<String, (f64, f64)> {
    let mut out: HashMap<String, (f64, f64)> = HashMap::with_capacity(cwp.len() + live.len());
    for (id, w) in cwp {
        if let Some((cx, cy)) = w.centroid() {
            let (px, py) = line.project(cx, cy);
            out.insert(id.clone(), from_local(px, py));
        }
    }
    for (id, &(la, lo)) in live {
        out.entry(id.clone()).or_insert_with(|| {
            let (x, y) = to_local(la, lo);
            let (px, py) = line.project(x, y);
            from_local(px, py)
        });
    }
    out
}

/// Shared ingest state.
pub struct LiveMap {
    devices: RwLock<HashMap<String, Pos>>,
    plc: RwLock<HashMap<String, Plc>>, // crane PLC state from the ctab zone
    centroids: RwLock<HashMap<String, Centroid>>, // learned block/bay positions (topos1 → centroid)
    lanes: RwLock<HashMap<(i32, i32), LaneCell>>, // learned driving-lane grid (③): cell → traffic+direction
    ring: Mutex<Ring>,
    connected: AtomicBool,
    messages: AtomicU64,
    reconnects: AtomicU64,
    /// GPS fixes dropped by the upstream coordinate gate (see TT_MAX_R_M).
    rejected_far: AtomicU64,
    /// fixes KEPT but flagged far — classes with no measured footprint (see UNMEASURED_ADVISORY_R_M).
    far_unmeasured: AtomicU64,
    last_msg_ms: AtomicU64,
    connected_since_ms: AtomicU64,
    started_ms: AtomicU64,
    last_error: RwLock<Option<String>>,
    plc_connected: AtomicBool,
    plc_messages: AtomicU64,
    // TT cycle: fleet drop timestamps (for throughput λ) + accepted cycle-interval
    // samples (drop_ms, interval_s) for the median. Both pruned to the 1h window.
    tt_drops: Mutex<VecDeque<i64>>,
    tt_cycles: Mutex<VecDeque<(i64, i64)>>,
    // container1 changes rejected by the movement filter (truck didn't move while carrying)
    // — i.e. suspected TOS re-assignment artifacts. Kept for auditability: the artifact:real
    // ratio is exposed so the filter's effect is visible rather than hidden.
    tt_artifacts: Mutex<VecDeque<i64>>,
    // subset of the above whose carried path was in the near-miss band [100,150)m — possibly
    // genuine ultra-short hauls the cut discards. Exposed so the upward-bias is measurable.
    tt_artifacts_near: Mutex<VecDeque<i64>>,
    // authoritative per-truck assignment (ytno → job), refreshed ~30s from the work pool by
    // `spawn_assignment_refresh`. Drives idle→staging classification and cycle metadata.
    assigned_pool: RwLock<HashMap<String, AssignedJob>>,
    // completed cycles awaiting persistence; drained into tt_cycle_log by `spawn_cycle_flusher`.
    cycle_log: Mutex<VecDeque<CompletedCycle>>,
    // v2 SHADOW: completed leg-based cycles → tt_cycle_v2 (same flusher).
    cycle_v2: Mutex<VecDeque<CompletedV2>>,
    // soon_idle prediction trips already logged this carry — (ytno, container) — so the 30s
    // logger records each trip's FIRST soon_idle entry once; released when the truck stops
    // carrying that container. Powers the soon-idle accuracy shadow (tt_soon_idle_pred).
    soon_idle_open: Mutex<HashSet<(String, String)>>,
    // dedup for the near-miss (wait_rtg) log — one row per carry-trip's first wait_rtg entry (⑤ gate).
    nearmiss_open: Mutex<HashSet<(String, String)>>,
    // SMOOTHED live QC starvation (GPS-distance, pending-work gated) — rolling mean over the last
    // few 30s ticks, written by spawn_qc_wait_logger and read by the positions endpoint. Replaces
    // the jumpy per-request topos1 count. (count, avg_wait_s).
    qc_wait_live: RwLock<Option<(usize, Option<i64>)>>,
    // QC handover model: one quay work-line (from all crane truck-ARRIVED positions) + a
    // per-crane recency centroid. Work-point = recency centroid projected onto the line
    // (swing-free, gantry-tracking). Blocks/RTG stay on `centroids` above.
    quay_line: RwLock<QuayLine>,
    crane_wp: RwLock<HashMap<String, CraneWp>>,
    // ⑤⑥ time-to-free self-cal (mig 0086): (state, jobtype, dist_bin) → learned median seconds-to-free,
    // per cycle stage, replacing the free_in constants. Loaded by spawn_selfcal_refresh (~15min).
    free_in_bias: RwLock<HashMap<(String, String, i16), i64>>,
    // 정차 앵커(mig 0091): jobtype → (median, p90) seconds-to-free measured from the GPS-stationary
    // moment (last delivering → drop), not the loose laden_arrived. Used as the dispatch base for a
    // candidate truck that is genuinely STOPPED at its drop; moving trucks keep free_in_bias.
    stationary_free: RwLock<HashMap<String, (i64, i64)>>,
    // 매처(spawn_stage2_shadow)가 마지막 틱에 실제로 쓴 차량 풀. positions 가 이걸 그대로
    // 서빙해서 TT 페이지의 후보 카드가 매처와 같은 숫자로 정렬/표시된다(재유도 아님 — 동일 값).
    stage2_pool: RwLock<Stage2Pool>,
}

/// The vehicle pool the Stage-2 matcher ACTUALLY used on its last tick (published every 60s).
/// `bases` = ytno → cost base (seconds-to-free; 0 = idle). `held` = GPS-silent trucks the matcher
/// keeps in the pool (age > STALE_AFTER_S, so the positions device list cannot show them).
#[derive(Default)]
struct Stage2Pool {
    as_of_ms: i64,
    bases: HashMap<String, i64>,
    held: Vec<HeldCandidateOut>,
}

/// GPS-silent candidate the matcher holds — surfaced separately because the device list
/// filters out stale fixes. `anchored` = the time-to-free counts down from this trip's own
/// crane pickup (move-log anchor) rather than the learned stationary constant.
#[derive(Serialize, Clone)]
struct HeldCandidateOut {
    id: String,
    jobtype: String,
    free_in_s: i64,
    anchored: bool,
}

/// Cap on the in-memory completed-cycle buffer. If the flusher stalls we drop the oldest
/// (with a warn) rather than grow unbounded — same spirit as the device pruner.
const CYCLE_BUF_MAX: usize = 5000;

impl LiveMap {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            devices: RwLock::new(HashMap::new()),
            plc: RwLock::new(HashMap::new()),
            centroids: RwLock::new(HashMap::new()),
            lanes: RwLock::new(HashMap::new()),
            ring: Mutex::new(Ring::new()),
            tt_drops: Mutex::new(VecDeque::new()),
            tt_cycles: Mutex::new(VecDeque::new()),
            tt_artifacts: Mutex::new(VecDeque::new()),
            tt_artifacts_near: Mutex::new(VecDeque::new()),
            assigned_pool: RwLock::new(HashMap::new()),
            cycle_log: Mutex::new(VecDeque::new()),
            cycle_v2: Mutex::new(VecDeque::new()),
            soon_idle_open: Mutex::new(HashSet::new()),
            nearmiss_open: Mutex::new(HashSet::new()),
            qc_wait_live: RwLock::new(None),
            quay_line: RwLock::new(QuayLine::default()),
            crane_wp: RwLock::new(HashMap::new()),
            free_in_bias: RwLock::new(HashMap::new()),
            stationary_free: RwLock::new(HashMap::new()),
            stage2_pool: RwLock::new(Stage2Pool::default()),
            connected: AtomicBool::new(false),
            messages: AtomicU64::new(0),
            reconnects: AtomicU64::new(0),
            rejected_far: AtomicU64::new(0),
            far_unmeasured: AtomicU64::new(0),
            last_msg_ms: AtomicU64::new(0),
            connected_since_ms: AtomicU64::new(0),
            started_ms: AtomicU64::new(Utc::now().timestamp_millis() as u64),
            last_error: RwLock::new(None),
            plc_connected: AtomicBool::new(false),
            plc_messages: AtomicU64::new(0),
        })
    }
}

// ───────────────────────── positions endpoint ─────────────────────────

#[derive(Serialize)]
struct DeviceOut {
    id: String,
    cls: String,
    lat: f64,
    lon: f64,
    speed: f64,
    engine: i32,
    age_s: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    jobtype: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vslname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    container1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    container2: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cur_loc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    topos1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arrival: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fuel: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    accuracy: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    userid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    batt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nett: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    distance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plc: Option<PlcOut>,
    // dispatch-state classification (TT only): idle|empty_travel|delivering|soon_idle|wait_rtg
    #[serde(skip_serializing_if = "Option::is_none")]
    dispatch: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dispatch_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nearest_rtg_m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dest_remaining_m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    swappable: Option<bool>,
    // Time-to-free estimate (median + p90 seconds). For candidate states (soon_idle/wait_rtg)
    // this is the Stage-2 matcher's published cost base (same number it matched with, ≤60s old);
    // other states get the free_in() constants (display-only).
    #[serde(skip_serializing_if = "Option::is_none")]
    free_in_s: Option<i64>,
    /// 매처가 마지막 틱에 **실제로 후보 풀에 넣은** 트럭인가(mig 0154 · pull 재정의 2026-08-19).
    /// 프론트가 GPS 상태 라벨로 후보를 재구성하면 안 된다 — 풀은 이제 원천 자유 신호와 예측 자유 시간으로
    /// 정해지고, `delivering`/`staging` 라벨이 붙은 트럭도 실제로는 빈 트럭이라 풀에 들어간다.
    #[serde(skip_serializing_if = "Option::is_none")]
    in_pool: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    free_in_hi_s: Option<i64>,
}

/// Crane PLC state served alongside a crane's GPS fix.
#[derive(Serialize)]
struct PlcOut {
    is_loaded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    load_t: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lock: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    land: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hpos: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tpos: Option<f64>,
    age_s: i64,
    /// completed moves in the last hour (= live move/hr) counted from the PLC
    mph: u32,
    /// seconds since this crane's last completed move (None if never seen)
    #[serde(skip_serializing_if = "Option::is_none")]
    last_move_age_s: Option<i64>,
}

#[derive(Serialize)]
pub struct PositionsOut {
    source: &'static str,
    connected: bool,
    as_of: Option<DateTime<Utc>>,
    count: usize,
    messages: u64,
    /// TT dispatch-state counts (idle/empty_travel/delivering/soon_idle/wait_rtg)
    dispatch_counts: HashMap<&'static str, usize>,
    /// websocket-derived live K_MPH cross-check: total crane moves in the last hour,
    /// the number of cranes that worked in that window, and their average move/hr.
    crane_moves_60m: u32,
    cranes_working: usize,
    crane_mph_live: Option<f64>,
    /// live TT cycle time (s) — replaces the mislabeled TOS span. Little's-law fleet
    /// average + median of per-truck drop intervals, with sample/throughput counts.
    tt_cycle_littles_s: Option<i64>,
    tt_cycle_median_s: Option<i64>,
    /// spread of the cycle samples (25th/75th pctile, s) so a thin/noisy median is visible
    tt_cycle_p25_s: Option<i64>,
    tt_cycle_p75_s: Option<i64>,
    tt_drops_60m: u32,
    tt_cycle_samples: u32,
    /// min samples before the median is shown (UI shows "collecting n/N" below this)
    tt_cycle_min_samples: u32,
    /// container1 changes rejected by the movement filter in the last hour (suspected TOS
    /// re-assignment artifacts). Exposed for audit: artifacts vs real deliveries.
    tt_artifacts_60m: u32,
    /// subset of rejects in the [100,150)m near-miss band — possible ultra-short hauls the
    /// 150m cut discards (measures the one direction this filter can bias the median up).
    tt_artifacts_near_60m: u32,
    /// how full the rolling 1h window is, in minutes (0..=60). <60 ⇒ still settling.
    window_fill_min: u32,
    active_trucks: usize,
    /// live K_UTIL (%) — TRUE utilization: of manned trucks, the fraction with an active job
    /// assignment (allocated→completed, even while stopped). Idle = manned but unassigned.
    tt_util_live: Option<i64>,
    /// secondary (%) — of manned trucks, the fraction physically moving/carrying right now
    /// (the remainder of the assigned ones are queued/waiting within their job).
    tt_engaged_live: Option<i64>,
    /// shift-to-date TIME-BASED utilization (%) — mean of the 60s assignment samples this
    /// shift (assigned/on-duty). The history-bearing figure; live value is the instant.
    tt_util_shift_avg: Option<i64>,
    /// live QC starvation (K_QC_TT_WAIT_GPS basis) — quay cranes idle with NO truck + their avg wait (s)
    qc_starving: usize,
    qc_wait_live_s: Option<i64>,
    /// GPS-silent trucks the Stage-2 matcher HOLDS as candidates (stale fixes are filtered out
    /// of `devices`, so without this the TT-page pool card undercounts the matcher's pool).
    /// Empty when the matcher hasn't published within ~3 min.
    stage2_held: Vec<HeldCandidateOut>,
    /// seconds since the matcher last published its pool (None = never, e.g. right after boot)
    stage2_pool_age_s: Option<i64>,
    devices: Vec<DeviceOut>,
}

// RTG within this of a TT ≈ engaged. Measured at ground-truth DS-drop handovers (rtg_move_log ⨝
// rtg_pos_hist ⨝ truck_pos_hist): RTG↔truck separation is ~40m median (crane antenna offset from
// the truck lane, NOT GPS jitter — pure jitter is ~2m). 30m caught only 32%; 50m catches 63%.
// The offset is ~isotropic (along 28 / cross 21), so an anisotropic box does NOT beat a wider circle.
const RTG_BAY_M: f64 = 50.0;
// ⑤ soon-idle gate self-cal (mig 0084): DS block soon_idle RTG-distance cutoff, learned to hold
// precision ≥0.82. Default = RTG_BAY_M×1000 (mm); spawn_selfcal_refresh updates it from
// learn_soon_idle_gate every ~15min. classify_tt reads it via soon_idle_gate_m().
static SOON_IDLE_GATE_MM: AtomicU64 = AtomicU64::new(50_000);
fn soon_idle_gate_m() -> f64 { SOON_IDLE_GATE_MM.load(Ordering::Relaxed) as f64 / 1000.0 }
// RTG-distance bin (matches learn_free_in_bias / ds_eta): -1 none, 0 ≤30m, 1 ≤80m, 2 ≤150m, 3 >150m.
fn dist_bin_of(nearest_rtg_m: Option<f64>) -> i16 {
    match nearest_rtg_m {
        None => -1,
        Some(m) if m <= 30.0 => 0,
        Some(m) if m <= 80.0 => 1,
        Some(m) if m <= 150.0 => 2,
        Some(_) => 3,
    }
}
// loaded TT stopped within this of its drop-target (topos1) centroid ≈ arrived (websocket geofence,
// replaces TOS rtg_active). Tuned by measured recall/precision vs crane ground truth (comparable load):
//   70m  → recall DS59/LD61, precision 96%   ← chosen (Pareto point)
//   85m  → recall DS61/LD59, precision 90%   ← DOMINATED (≈70m recall, worse precision) → dropped
//   100m → recall DS73/LD68, precision 87%   ← recall-max, but −9pp precision (early/false arrivals)
// The 70→85m band adds false arrivals without real catches; the meaningful recall gain is only at
// 100m, at a precision cost. 70m is the safe default (recall still +10pp over the 50% baseline).
// Gated on the truck's OWN topos1. Bump to 100m if max recall is worth the precision hit.
const GEOFENCE_DROP_M: f64 = 70.0;
const IDLE_SPEED_KMH: f64 = 3.0;
// A TT within this of its ASSIGNED quay crane's GPS ≈ arrived at the crane. Used ONLY to
// populate the SHADOW crane-arrival columns (observational); the live phase logic is untouched.
const CRANE_ARRIVE_M: f64 = 40.0;
// A fresh, EMPTY + UNASSIGNED TT within this of a crane ≈ a truck that was genuinely available
// nearby. Used by the per-QC starvation log (near_idle_tt) to separate "no truck dispatched in
// time" (Stage-1) from "no free truck was anywhere near" (Stage-2/location) when validating timing.
const NEAR_TT_M: f64 = 600.0;
// the crane is "actively handling" if its PLC logged a pickup within this window.
const CRANE_PLC_ACTIVE_MS: i64 = 120_000;
const SWAP_MIN_M: f64 = 500.0; // default swap threshold (frontend slider overrides for display)

/// A topos1 like "C46"/"M4"/"Z6" = a quay/dynamic crane (vs a block code like "03U-21").
fn is_crane_code(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && matches!(b[0], b'C' | b'M' | b'Z') && b[1..].iter().all(u8::is_ascii_digit)
}

/// Block/area prefix of a yard code: "07F-06" → "07F", "WHARF_23_B" → "WHARF_23_B".
fn block_prefix(s: &str) -> &str {
    s.split('-').next().unwrap_or(s)
}

/// v2.3 (B): pre-positioned arrival. A block-pickup leg whose truck is already stopped at
/// the target block at assignment time was waiting there before being assigned (its arr_dtime
/// predates the assignment and is rejected by the `>= assigned` guard, and it may leave before
/// a stopped-at-block frame is processed). Anchor arrival to the assignment instant. Crane
/// legs are excluded — WHARF is too coarse and would reintroduce the early-arrival bias.
fn prepositioned_arrival(crane: bool, target: &str, stopped: bool, cur_loc: &str) -> bool {
    !crane && stopped && !cur_loc.is_empty() && block_prefix(cur_loc) == block_prefix(target)
}

/// Approximate ground distance (m) between two lat/lon points (equirectangular).
fn dist_m(a: (f64, f64), b: (f64, f64)) -> f64 {
    let lat = (a.0 + b.0) / 2.0 * std::f64::consts::PI / 180.0;
    let dx = (a.1 - b.1) * 111_320.0 * lat.cos();
    let dy = (a.0 - b.0) * 111_320.0;
    (dx * dx + dy * dy).sqrt()
}

/// Open a cycle at pickup (container1 empty→non-empty). The empty leg = previous-drop→pickup
/// (time + path accumulated while empty). Job metadata = the truck's latched GPS fields,
/// enriched from the work-pool cache (`aj`) for fields the GPS doesn't carry (voyage/twin).
fn open_cycle(now: i64, p: &Pos, aj: Option<&AssignedJob>) -> OpenCycle {
    let empty_leg_ms = if p.empty_since_ms > 0 { (now - p.empty_since_ms).max(0) } else { 0 };
    let or_pool = |gps: &Option<String>, pool: Option<&String>| gps.clone().or_else(|| pool.cloned());
    OpenCycle {
        // assignment≈start of the empty drive to this pickup (proxy: previous drop time)
        assigned_at_ms: if p.empty_since_ms > 0 { p.empty_since_ms } else { now },
        // pickup-side ARRIVED (split 공차이동 vs 받기) — seeded ONLY when the empty leg's start
        // was actually observed (empty_since > 0). After a restart / GPS gap the leg start is
        // unknown (0): an earlier latched ARRIVED could belong to the PREVIOUS job's
        // destination, and assigned_at falls back to the pickup instant, which produced
        // pickup_arrived < assigned inversions (verified: all 55 inverted rows had
        // assigned==pickup, 83% were the truck's first post-restart cycle). Unknown → NULL.
        pickup_arrived_at_ms: if p.empty_since_ms > 0 && p.empty_arrived_ms > p.empty_since_ms { p.empty_arrived_ms } else { 0 },
        pickup_left_at_ms: 0,
        pickup_at_ms: now,
        arrived_at_ms: 0,
        pickup_arrived_crane_ms: 0,
        arrived_crane_ms: 0,
        crane_arr_method: None,
        idle_before_ms: 0, // derived offline from consecutive rows (pickup_at − prev dropped_at)
        empty_leg_ms,
        empty_leg_m: p.empty_trip_m,
        jobtype: or_pool(&p.latched_jobtype, aj.and_then(|a| a.jobtype.as_ref())),
        vessel: or_pool(&p.latched_vessel, aj.and_then(|a| a.vessel.as_ref())),
        voyage: aj.and_then(|a| a.voyage.clone()),
        container: p.container1.clone().or_else(|| p.latched_container.clone()).or_else(|| aj.and_then(|a| a.contno.clone())),
        qc: aj.and_then(|a| a.qc.clone()).or_else(|| p.latched_topos.clone().filter(|t| is_crane_code(t))),
        twintandem: aj.and_then(|a| a.twintandem.clone()),
    }
}

/// Finalize the open cycle into a `CompletedCycle` on the validated drop. `c2c` = the truck
/// went straight into another container (no empty gap). Reads `carry_trip_m` as the laden
/// path length — call BEFORE it is reset for the next box.
fn finalize_cycle(id: &str, p: &Pos, now: i64, c2c: bool) -> CompletedCycle {
    let oc = p.cycle_open.clone().unwrap_or_default();
    let pickup = if oc.pickup_at_ms > 0 { oc.pickup_at_ms } else { p.carry_since_ms };
    let laden_leg_ms = if pickup > 0 { (now - pickup).max(0) } else { 0 };
    let assigned_at = if oc.assigned_at_ms > 0 { oc.assigned_at_ms } else { pickup };
    CompletedCycle {
        ytno: id.to_string(),
        assigned_at_ms: assigned_at,
        pickup_arrived_at_ms: oc.pickup_arrived_at_ms,
        pickup_left_at_ms: oc.pickup_left_at_ms,
        pickup_at_ms: pickup,
        arrived_at_ms: oc.arrived_at_ms,
        pickup_arrived_crane_ms: oc.pickup_arrived_crane_ms,
        arrived_crane_ms: oc.arrived_crane_ms,
        crane_arr_method: oc.crane_arr_method,
        dropped_at_ms: now,
        idle_before_ms: oc.idle_before_ms,
        empty_leg_ms: oc.empty_leg_ms,
        empty_leg_m: oc.empty_leg_m,
        laden_leg_ms,
        laden_leg_m: p.carry_trip_m,
        jobtype: oc.jobtype.or_else(|| p.latched_jobtype.clone()),
        vessel: oc.vessel.or_else(|| p.latched_vessel.clone()),
        voyage: oc.voyage,
        container: oc.container.or_else(|| p.latched_container.clone()),
        qc: oc.qc,
        twintandem: oc.twintandem,
        container_to_container: c2c,
    }
}

#[derive(Default)]
struct Classed {
    state: &'static str,
    reason: Option<String>,
    nearest_rtg_m: Option<f64>,
    dest_remaining_m: Option<f64>, // empty_travel: distance to its assigned pickup (topos1)
    swappable: Option<bool>,       // empty_travel: still far enough from pickup to re-match
}

/// Classify a TT's dispatch state from its mission fields + RTG proximity + QC PLC, and for
/// empty-travelling TTs assess swap-worthiness (remaining distance to the assigned pickup —
/// crane destinations use live crane GPS, block destinations use learned centroids).
///  - idle: empty + ~stationary (available now)
///  - empty_travel: empty + moving toward a pickup (swap candidate if still far enough)
///  - delivering: loaded + en route
///  - soon_idle: loaded + ARRIVED at final handover AND crane engaged (QC=PLC / block=RTG near)
///  - wait_rtg: loaded + ARRIVED at block but no RTG near yet (arrived ≠ soon-idle)
fn classify_tt(
    p: &Pos,
    _aj: Option<&AssignedJob>, // TOS work-pool — no longer used (real-time path is websocket-only)
    rtgs: &[(f64, f64)],
    plc: &HashMap<String, Plc>,
    cranes: &HashMap<String, (f64, f64)>,
    centroids: &HashMap<String, Centroid>,
    now: i64,
) -> Classed {
    let st = |state, reason: Option<String>| Classed { state, reason, ..Default::default() };
    // WEBSOCKET-ONLY (no TOS work-pool in the real-time decision path — TOS is offline/learning
    // only): a pending mission = a live or latched container/target. latched_* persist across feed
    // gaps that clear the raw fields, so an intermittent feed doesn't drop the assignment.
    let assigned = p.container1.as_deref().is_some_and(|s| !s.is_empty())
        || p.latched_container.as_deref().is_some_and(|s| !s.is_empty())
        || p.latched_topos.as_deref().is_some_and(|s| !s.is_empty());
    let loaded = p.container1.as_deref().is_some_and(|s| !s.is_empty());
    if !loaded {
        if p.speed < IDLE_SPEED_KMH {
            // empty + stationary: only truly UNASSIGNED trucks are idle. A truck the work pool
            // says is assigned is queued/staging for its pickup, not available — the GPS feed
            // clears the job fields between updates, which used to over-count these as idle.
            if assigned {
                return st("staging", Some("배차됨 · 픽업 대기/정차".into()));
            }
            return st("idle", None);
        }
        // empty + moving = empty_travel. Swap-worthiness = remaining distance to its pickup.
        let topos = p.topos1.as_deref().unwrap_or("");
        if topos.is_empty() {
            return Classed { state: "empty_travel", reason: Some("공차 주행 중 · 회송/대기".into()), swappable: Some(false), ..Default::default() };
        }
        let destpos = if is_crane_code(topos) {
            // live crane GPS, else its learned position (crane may not be broadcasting)
            cranes.get(topos).copied().or_else(|| centroids.get(topos).map(|c| (c.lat, c.lon)))
        } else {
            centroids.get(topos).or_else(|| centroids.get(block_prefix(topos))).map(|c| (c.lat, c.lon))
        };
        let remaining = destpos.map(|dp| dist_m((p.lat, p.lon), dp));
        let rem_r = remaining.map(|r| (r * 10.0).round() / 10.0);
        let swappable = remaining.is_none_or(|r| r >= SWAP_MIN_M);
        let reason = match remaining {
            Some(r) if r < SWAP_MIN_M => format!("공차 주행 중 · 목적지 근접 {r:.0}m (스왑 부적합)"),
            Some(r) => format!("공차 주행 중 · 잔여 {r:.0}m"),
            None => "공차 주행 중 · 목적지 미학습".into(),
        };
        return Classed { state: "empty_travel", reason: Some(reason), dest_remaining_m: rem_r, swappable: Some(swappable), ..Default::default() };
    }
    // ── loaded: which side UNLOADS this job (= frees the TT)? LD at the quay crane; DS/MO/MI at a
    // block. A loaded TT whose current target (topos1) is the OTHER side just picked up → delivering.
    let topos = p.topos1.as_deref().unwrap_or("");
    let is_crane = is_crane_code(topos);
    let drop_at_crane = match p.jobtype.as_deref().unwrap_or("") {
        "LD" => true,
        "DS" | "MO" | "MI" => false,
        _ => is_crane,
    };
    if drop_at_crane && !is_crane {
        return st("delivering", Some("적재 이동 (안벽行)".into()));
    }
    if !drop_at_crane && is_crane {
        return st("delivering", Some("적재 이동 (블록行)".into()));
    }
    // WEBSOCKET-ONLY arrival at the drop: the ARRIVED flag OR a geofence (stopped within
    // GEOFENCE_DROP_M of the drop target). The geofence rescues arrivals the ARRIVED flag misses —
    // this is what replaces the TOS `rtg_active` signal (now offline/learning-only). GPS misses DS
    // RTGs (no PLC), so being physically stopped at the drop block is the real-time arrival signal.
    let drop_pos = if is_crane {
        cranes.get(topos).copied().or_else(|| centroids.get(topos).map(|c| (c.lat, c.lon)))
    } else {
        centroids.get(topos).or_else(|| centroids.get(block_prefix(topos))).map(|c| (c.lat, c.lon))
    };
    let drop_dist = drop_pos.map(|dp| dist_m((p.lat, p.lon), dp));
    let arrived_flag = p.arrival.as_deref() == Some("ARRIVED");
    let geofence_arrived = p.speed < IDLE_SPEED_KMH && drop_dist.is_some_and(|d| d <= GEOFENCE_DROP_M);
    if !arrived_flag && !geofence_arrived {
        return st("delivering", Some("적재 이동 중".into()));
    }
    let geo_tag = if !arrived_flag { " · 지오펜스" } else { "" };
    if drop_at_crane {
        // LD drop at the quay crane
        let plc_ok = plc.get(topos).is_some_and(|c| (now - c.last_seen_ms) / 1000 <= STALE_AFTER_S);
        let src = if plc_ok { " · PLC 확인" } else { geo_tag };
        return st("soon_idle", Some(format!("안벽 {topos} 핸드오버{src}")));
    }
    // DS/MO/MI drop at a block: RTG GPS ≤30m = engaged (soon_idle); else arrived-and-waiting
    // (wait_rtg). Both are dispatch candidates with the same free-in.
    let nearest = rtgs.iter().map(|r| dist_m((p.lat, p.lon), *r)).fold(f64::INFINITY, f64::min);
    let d = nearest.is_finite().then(|| (nearest * 10.0).round() / 10.0);
    match d {
        Some(dm) if dm <= soon_idle_gate_m() => {
            Classed { state: "soon_idle", reason: Some(format!("블록 RTG 근접 {dm:.0}m")), nearest_rtg_m: Some(dm), ..Default::default() }
        }
        _ => {
            let where_txt = drop_dist.map(|x| format!("{x:.0}m")).unwrap_or_else(|| "미학습".into());
            Classed { state: "wait_rtg", reason: Some(format!("블록 도착{geo_tag} · RTG 대기 ({where_txt})")), nearest_rtg_m: d, ..Default::default() }
        }
    }
}

/// CONSTANT-table estimate of time-to-free (median, p90) in seconds, by dispatch state + jobtype.
/// Role today: the LAST fallback of the matcher's cost base (anchor → stationary → learned bias
/// → this), and the display value for non-candidate states (e.g. delivering) or when the matcher
/// hasn't published a pool recently. Candidate states normally serve the matcher's base instead.
///
/// Grounded in `tt_cycle_v2` measurement (DS, last 36h, 2026-06-16, all from the same GPS clock so
/// the tiers are internally consistent):
///   - delivering (loaded, still driving): pickup_left→dropped  p50 17.2m / p90 40.3m  (n=7,264)
///   - arrived at block (wait_rtg): laden_arrived→dropped  p50 8.0m / p90 27m (n=6,249)
///   - soon_idle (RTG ≤30m engaged, or quay PLC): ~2m — least-grounded tier (no RTG-distance history),
///     rough "handover in progress" value.
/// Only DS is grounded; other jobtypes get None (their free-point differs and is unmeasured here).
fn free_in(state: &str, jobtype: Option<&str>) -> (Option<i64>, Option<i64>) {
    let ds = jobtype == Some("DS");
    match state {
        "delivering" if ds => (Some(1030), Some(2420)),
        "wait_rtg" if ds => (Some(480), Some(1620)),
        "soon_idle" => (Some(120), Some(360)),    // imminent: DS RTG≤30m or LD quay handover
        _ => (None, None),
    }
}

/// `GET /api/livemap/positions` — active device fixes (age ≤ 120s).
pub async fn positions(State(lm): State<Arc<LiveMap>>, State(pool): State<PgPool>) -> Json<PositionsOut> {
    let now = Utc::now().timestamp_millis();
    // observation window for the live move/hr rate: capped at 1h, but right after a
    // restart we've collected less, so divide the move count by the actual elapsed
    // hours (min 1 min) instead of a full hour — otherwise the rate reads far too low
    // until the 1h ring fills.
    let started = lm.started_ms.load(Ordering::Relaxed) as i64;
    let obs_h = (((now - started) as f64) / 3_600_000.0).clamp(0.1, 1.0);
    let rate = |moves: usize| ((moves as f64 / obs_h).round()) as u32;
    let map = lm.devices.read().await;
    let plc = lm.plc.read().await;
    let centroids = lm.centroids.read().await;
    let assigned_pool = lm.assigned_pool.read().await;
    // fresh RTG positions for the discharge same-bay proximity check
    let rtgs: Vec<(f64, f64)> = map
        .values()
        .filter(|p| p.cls == "RTG" && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
        .map(|p| (p.lat, p.lon))
        .collect();
    // fresh crane (C/M/Z) positions — destination for empty TTs heading to pick up at a quay
    let cranes: HashMap<String, (f64, f64)> = map
        .iter()
        .filter(|(_, p)| matches!(p.cls.as_str(), "C" | "M" | "Z") && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
        .map(|(id, p)| (id.clone(), (p.lat, p.lon)))
        .collect();
    // dispatch uses the swing-free QC WORK-POINT (recency centroid / live GPS, projected onto the
    // quay line), not the raw spreader GPS. classify_tt falls back to `centroids` for the rest.
    let cranes = {
        let line = *lm.quay_line.read().await;
        let g = lm.crane_wp.read().await;
        resolve_crane_wp(&line, &g, &cranes)
    };
    // Stage-2 matcher pool published on its last tick (60s cadence). Candidate states serve
    // these bases as free_in_s so the TT-page card sorts/labels by the SAME numbers the matcher
    // used; if the matcher hasn't published within ~3 min (boot/outage) fall back to constants.
    let (s2_bases, s2_held, s2_age_s) = {
        let sp = lm.stage2_pool.read().await;
        let age_s = (sp.as_of_ms > 0).then(|| ((now - sp.as_of_ms) / 1000).max(0));
        let fresh = age_s.is_some_and(|a| a <= 180);
        (
            if fresh { sp.bases.clone() } else { HashMap::new() },
            if fresh { sp.held.clone() } else { Vec::new() },
            age_s,
        )
    };
    let s2_fresh = !s2_bases.is_empty() || s2_age_s.is_some_and(|a| a <= 180);
    let mut devices: Vec<DeviceOut> = map
        .iter()
        .filter_map(|(id, p)| {
            let age = (now - p.last_seen_ms) / 1000;
            if age > STALE_AFTER_S {
                return None;
            }
            let c = (p.cls == "TT").then(|| classify_tt(p, assigned_pool.get(id), &rtgs, &plc, &cranes, &centroids, now));
            let dispatch = c.as_ref().map(|c| c.state);
            let dispatch_reason = c.as_ref().and_then(|c| c.reason.clone());
            let nearest_rtg_m = c.as_ref().and_then(|c| c.nearest_rtg_m);
            let dest_remaining_m = c.as_ref().and_then(|c| c.dest_remaining_m);
            let swappable = c.as_ref().and_then(|c| c.swappable);
            // time-to-free: candidate states (soon_idle/wait_rtg) mirror the matcher's published
            // cost base — the same number the matcher used last tick (DS move-log anchor first,
            // stationary median when stopped, learned bias otherwise). Non-candidate states and
            // matcher-stale windows keep the free_in() constants. Published bases carry no p90.
            // ★풀 소속은 매처가 발행한 것을 그대로 쓴다(2026-08-19 pull 재정의). 상태 라벨로 재구성하면
            //   `delivering`/`staging` 로 보이지만 실제로는 빈 트럭인 경우를 프론트가 놓친다.
            // ⚠매처 발행이 낡으면(부팅 직후·매처 정지) **모름(None)** 이어야 한다. `false` 로 내보내면 프론트의
            //   `d.in_pool ?? 폴백` 이 안 걸려 "후보 0"을 아무 표시 없이 그린다(2026-08-21 2차 리뷰).
            let in_pool = (p.cls == "TT" && s2_fresh).then(|| s2_bases.contains_key(id));
            let (free_in_s, free_in_hi_s) = match (s2_bases.get(id), dispatch) {
                (Some(&b), _) => (Some(b), None),
                (None, Some(s)) => free_in(s, p.jobtype.as_deref()),
                (None, None) => (None, None),
            };
            // attach crane PLC state (ctab zone) when fresh — id matches the crane id.
            let plc_out = plc.get(id).and_then(|c| {
                let pa = (now - c.last_seen_ms) / 1000;
                (pa <= STALE_AFTER_S).then(|| PlcOut {
                    is_loaded: c.load_t.is_some_and(|t| t >= 1.0),
                    load_t: c.load_t,
                    lock: c.lock,
                    land: c.land,
                    hpos: c.hpos,
                    tpos: c.tpos,
                    age_s: pa.max(0),
                    mph: rate(c.moves.iter().filter(|&&tm| now - tm <= MOVE_WINDOW_MS).count()),
                    last_move_age_s: (c.last_move_ms != 0).then(|| (now - c.last_move_ms) / 1000),
                })
            });
            Some(DeviceOut {
                id: id.clone(),
                in_pool,
                cls: p.cls.clone(),
                lat: p.lat,
                lon: p.lon,
                speed: p.speed,
                engine: p.engine,
                age_s: age.max(0),
                jobtype: p.jobtype.clone(),
                vslname: p.vslname.clone(),
                container1: p.container1.clone(),
                container2: p.container2.clone(),
                cur_loc: p.cur_loc.clone(),
                topos1: p.topos1.clone(),
                arrival: p.arrival.clone(),
                fuel: p.fuel,
                accuracy: p.accuracy,
                userid: p.userid.clone(),
                batt: p.batt.clone(),
                nett: p.nett.clone(),
                dtime: p.dtime.clone(),
                distance: p.distance,
                plc: plc_out,
                dispatch,
                dispatch_reason,
                nearest_rtg_m,
                dest_remaining_m,
                swappable,
                free_in_s,
                free_in_hi_s,
            })
        })
        .collect();
    devices.sort_by(|a, b| a.id.cmp(&b.id));
    let mut dispatch_counts: HashMap<&'static str, usize> = HashMap::new();
    for d in &devices {
        if let Some(s) = d.dispatch {
            *dispatch_counts.entry(s).or_default() += 1;
        }
    }
    // fleet live K_MPH cross-check: count moves/hr per crane, average over the cranes
    // that actually worked in the window (a crane idle all hour shouldn't drag the mean).
    let mut crane_moves_60m = 0u32;
    let mut cranes_working = 0usize;
    for c in plc.values() {
        let m = c.moves.iter().filter(|&&tm| now - tm <= MOVE_WINDOW_MS).count() as u32;
        if m > 0 {
            crane_moves_60m += m;
            cranes_working += 1;
        }
    }
    let crane_mph_live = (cranes_working > 0)
        .then(|| (crane_moves_60m as f64 / cranes_working as f64 / obs_h * 10.0).round() / 10.0);

    // ── live TT cycle time ── (replaces the mislabeled TOS "cycle"). Two estimates:
    //  • Little's law W = L/λ — robust fleet average: L = trucks in a cycle (non-idle),
    //    λ = fleet delivery (drop) rate. No per-truck edge timing → hard to skew.
    //  • median of per-truck drop-to-drop intervals (capped) — the typical cycle.
    let active_trucks: usize = dispatch_counts.iter().filter(|(k, _)| **k != "idle").map(|(_, v)| *v).sum();
    // L for Little's law = trucks on a laden round-trip arc (en route to pick up, carrying,
    // or at handover). Exclude idle and wait_rtg (parked) so W = L/λ isn't biased high.
    let cycling_trucks: usize = ["empty_travel", "delivering", "soon_idle"]
        .iter()
        .map(|k| dispatch_counts.get(k).copied().unwrap_or(0))
        .sum();
    let drops_60m = {
        let d = lm.tt_drops.lock().await;
        d.iter().filter(|&&t| now - t <= MOVE_WINDOW_MS).count() as u32
    };
    let lambda = drops_60m as f64 / (obs_h * 3600.0); // deliveries / second
    let tt_cycle_littles_s = (cycling_trucks > 0 && lambda > 0.0)
        .then(|| (cycling_trucks as f64 / lambda).round() as i64);
    let mut cyc_samples: Vec<i64> = {
        let c = lm.tt_cycles.lock().await;
        c.iter().filter(|&&(t, _)| now - t <= MOVE_WINDOW_MS).map(|&(_, i)| i).collect()
    };
    let tt_cycle_samples = cyc_samples.len() as u32;
    cyc_samples.sort_unstable();
    let pctile = |v: &[i64], p: f64| v.get(((v.len() as f64 * p) as usize).min(v.len().saturating_sub(1))).copied();
    let have_median = cyc_samples.len() >= MIN_CYCLE_SAMPLES;
    let tt_cycle_median_s = have_median.then(|| cyc_samples[cyc_samples.len() / 2]);
    let tt_cycle_p25_s = have_median.then(|| pctile(&cyc_samples, 0.25)).flatten();
    let tt_cycle_p75_s = have_median.then(|| pctile(&cyc_samples, 0.75)).flatten();
    let tt_cycle_min_samples = MIN_CYCLE_SAMPLES as u32;
    let tt_artifacts_60m = {
        let a = lm.tt_artifacts.lock().await;
        a.iter().filter(|&&t| now - t <= MOVE_WINDOW_MS).count() as u32
    };
    let tt_artifacts_near_60m = {
        let a = lm.tt_artifacts_near.lock().await;
        a.iter().filter(|&&t| now - t <= MOVE_WINDOW_MS).count() as u32
    };
    // how full the rolling 1h window is (min). Until it is full the rates/median are still
    // settling, so the UI labels "window filling" rather than implying a steady-state hour.
    let started = lm.connected_since_ms.load(Ordering::Relaxed) as i64;
    let window_fill_min = if started > 0 {
        (((now - started) / 60_000).clamp(0, MOVE_WINDOW_MS / 60_000)) as u32
    } else { 0 };

    // ── live K_UTIL (TT utilization) — pure TOS, no GPS (GPS counts were unreliable) ──
    // From the work pool (live_assigned_tt): a truck is utilized when it is actively
    // dispatched on a job (status A = working now, allocation→completion incl. queuing at a
    // crane). The denominator is the *tasked* fleet = trucks with any active/blocked/queued
    // job (A/B/Q); the gap is trucks between jobs / waiting their turn. No-job trucks aren't
    // in the pool (TOS can't see them — same limitation either way).
    let (active_n, deployed_n) = sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
        "SELECT count(DISTINCT ytno) FILTER (WHERE jobstatus = 'A'),
                count(DISTINCT ytno)
           FROM live_assigned_tt WHERE as_of_ts > now() - interval '5 minutes'",
    )
    .fetch_one(&pool)
    .await
    .map(|(a, d)| (a.unwrap_or(0) as usize, d.unwrap_or(0) as usize))
    .unwrap_or((0, 0));
    let tt_util_live = (deployed_n > 0)
        .then(|| (active_n as f64 / deployed_n as f64 * 100.0).round() as i64);
    let tt_engaged_live: Option<i64> = None; // GPS moving-fraction retired (unreliable)
    // shift-to-date TIME-BASED utilization: mean of the 60s assignment samples this shift.
    let (bd_cur, sh_cur) = tt_core::shift::current(tt_core::shift::terminal_now().naive_local());
    let tt_util_shift_avg: Option<i64> = sqlx::query_scalar::<_, Option<f64>>(
        "SELECT round(avg(100.0*assigned/nullif(on_duty,0)))::float8
           FROM util_tt_sample WHERE business_date=$1 AND shift=$2",
    )
    .bind(bd_cur)
    .bind(sh_cur.label())
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten()
    .flatten()
    .map(|v| v as i64);

    // ── live QC starvation (QC waiting for a truck; = K_QC_TT_WAIT_GPS basis). GPS-distance based
    // (no fresh TT within CRANE_ARRIVE_M of the crane), gated on the crane having pending work, and
    // smoothed over ~3 min — computed by spawn_qc_wait_logger and read here. Replaces the jumpy
    // per-request topos1 count (which over-reported ~2× per the 19h evaluation).
    let (qc_starving, qc_wait_live_s) = (*lm.qc_wait_live.read().await).unwrap_or((0, None));

    let last_ms = lm.last_msg_ms.load(Ordering::Relaxed);
    let as_of = (last_ms != 0).then(|| DateTime::from_timestamp_millis(last_ms as i64)).flatten();
    Json(PositionsOut {
        source: "live",
        connected: lm.connected.load(Ordering::Relaxed),
        as_of,
        count: devices.len(),
        messages: lm.messages.load(Ordering::Relaxed),
        dispatch_counts,
        crane_moves_60m,
        cranes_working,
        crane_mph_live,
        tt_cycle_littles_s,
        tt_cycle_median_s,
        tt_cycle_p25_s,
        tt_cycle_p75_s,
        tt_drops_60m: drops_60m,
        tt_cycle_samples,
        tt_cycle_min_samples,
        tt_artifacts_60m,
        tt_artifacts_near_60m,
        window_fill_min,
        active_trucks,
        tt_util_live,
        tt_engaged_live,
        tt_util_shift_avg,
        qc_starving,
        qc_wait_live_s,
        stage2_held: s2_held,
        stage2_pool_age_s: s2_age_s,
        devices,
    })
}

// ───────────────────────── health endpoint ─────────────────────────

#[derive(Serialize)]
pub struct HealthOut {
    /// overall state: "green" | "amber" | "red"
    color: &'static str,
    state_word: &'static str,
    cause: String,
    connected: bool,
    /// seconds since the upstream socket connected (null if never / down)
    connected_for_s: Option<i64>,
    /// seconds since the last GPS message (null if none yet)
    last_msg_age_s: Option<i64>,
    last_message_at: Option<DateTime<Utc>>,
    messages_total: u64,
    reconnects: u64,
    /// fixes dropped by the upstream coordinate gate (non-finite, or TT past its measured range)
    rejected_far_fixes: u64,
    /// fixes KEPT but far out, from a class with no measured footprint — data to bound them later
    far_unmeasured_fixes: u64,
    last_error: Option<String>,
    uptime_s: i64,
    /// messages in the last completed minute
    rate_per_min: u32,
    /// per-minute counts, oldest→newest (length 30)
    sparkline: Vec<u32>,
    // freshness bands (device counts)
    fresh: usize,
    stale: usize,
    lost: usize,
    total_devices: usize,
    // fleet + quality
    by_class: HashMap<String, usize>,
    engine_on: usize,
    with_job: usize,
    avg_accuracy_m: Option<f64>,
    fresh_under_s: i64,
    stale_after_s: i64,
    // ctab zone (crane PLC)
    plc_connected: bool,
    plc_devices: usize,
    plc_messages: u64,
}

/// `GET /api/livemap/health` — feed health for the WS-data monitoring page.
/// `GET /api/weather` — latest live 1-minute weather (Tomorrow.io → weather_1min) for the
/// live-map chip. Returns null until the collector has data (needs TOMORROW_API_KEY).
#[derive(Serialize)]
pub struct WeatherOut {
    ts: DateTime<Utc>,
    precip_mm_hr: Option<f64>,
    visibility_km: Option<f64>,
    wind_ms: Option<f64>,
    weather_code: Option<i32>,
    age_s: i64,
}
/// Learned wharf/quay segment positions (cur_loc=WHARF_*), from `learn_topos_point`. Powers the
/// live-map wharf overlay. Confident points only (n>=5). topos = the wharf label (e.g. WHARF_14_C).
#[derive(Serialize, sqlx::FromRow)]
pub struct WharfPoint {
    topos: String,
    lat: f64,
    lon: f64,
    n: i32,
    spread_m: Option<f64>,
}
pub async fn wharf(State(pool): State<PgPool>) -> Json<Vec<WharfPoint>> {
    let pts = sqlx::query_as::<_, WharfPoint>(
        "SELECT topos, lat, lon, n, spread_m FROM learn_topos_point
          WHERE topos LIKE 'WHARF%' AND n >= 5 ORDER BY topos",
    )
    .fetch_all(&pool)
    .await
    .unwrap_or_default();
    Json(pts)
}

pub async fn weather(State(pool): State<PgPool>) -> Json<Option<WeatherOut>> {
    let row: Option<(DateTime<Utc>, Option<f64>, Option<f64>, Option<f64>, Option<i32>)> =
        sqlx::query_as(
            "SELECT ts, precip_mm_hr, visibility_km, wind_ms, weather_code
               FROM weather_1min ORDER BY ts DESC LIMIT 1",
        )
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten();
    Json(row.map(|(ts, precip_mm_hr, visibility_km, wind_ms, weather_code)| WeatherOut {
        age_s: (Utc::now() - ts).num_seconds().max(0),
        ts,
        precip_mm_hr,
        visibility_km,
        wind_ms,
        weather_code,
    }))
}

pub async fn health(State(lm): State<Arc<LiveMap>>) -> Json<HealthOut> {
    let now = Utc::now().timestamp_millis();
    let now_min = now / 60_000;
    let connected = lm.connected.load(Ordering::Relaxed);

    let (sparkline, rate_per_min) = {
        let mut ring = lm.ring.lock().await;
        ring.advance(now_min);
        (ring.series(), ring.rate())
    };

    let last_ms = lm.last_msg_ms.load(Ordering::Relaxed) as i64;
    let last_msg_age_s = (last_ms != 0).then(|| (now - last_ms) / 1000);
    let last_message_at = (last_ms != 0).then(|| DateTime::from_timestamp_millis(last_ms)).flatten();
    let csince = lm.connected_since_ms.load(Ordering::Relaxed) as i64;
    let connected_for_s = (connected && csince != 0).then(|| (now - csince) / 1000);
    let started = lm.started_ms.load(Ordering::Relaxed) as i64;

    let (mut fresh, mut stale, mut lost, mut engine_on, mut with_job) = (0, 0, 0, 0, 0);
    let mut by_class: HashMap<String, usize> = HashMap::new();
    let (mut acc_sum, mut acc_n) = (0.0_f64, 0_u32);
    {
        let map = lm.devices.read().await;
        for p in map.values() {
            let age = (now - p.last_seen_ms) / 1000;
            if age <= FRESH_UNDER_S {
                fresh += 1;
            } else if age <= STALE_AFTER_S {
                stale += 1;
            } else {
                lost += 1;
            }
            if age <= STALE_AFTER_S {
                *by_class.entry(p.cls.clone()).or_default() += 1;
                if p.engine == 1 {
                    engine_on += 1;
                }
                if p.jobtype.as_deref().is_some_and(|s| !s.is_empty()) {
                    with_job += 1;
                }
                if let Some(a) = p.accuracy {
                    acc_sum += a;
                    acc_n += 1;
                }
            }
        }
    }
    let total_devices = fresh + stale + lost;
    let avg_accuracy_m = (acc_n > 0).then(|| (acc_sum / acc_n as f64 * 10.0).round() / 10.0);
    let plc_devices = {
        let p = lm.plc.read().await;
        p.values().filter(|c| (now - c.last_seen_ms) / 1000 <= STALE_AFTER_S).count()
    };

    // overall state: red if not connected or no data >60s; amber if data 20-60s stale
    // or zero active devices; else green.
    let active = fresh + stale;
    let (color, state_word, cause): (&str, &str, String) = if !connected {
        ("red", "장애", "WS 미연결 — SSH 터널/소스 확인".into())
    } else if last_msg_age_s.is_none_or(|a| a > 60) {
        ("red", "장애", "60초 이상 데이터 없음".into())
    } else if active == 0 {
        ("amber", "주의", "활성 장비 없음".into())
    } else if last_msg_age_s.is_some_and(|a| a > 20) {
        ("amber", "주의", format!("최근 수신 {}초 전", last_msg_age_s.unwrap_or(0)))
    } else {
        ("green", "정상", format!("{active}대 추적 중 · {rate_per_min}/분"))
    };

    Json(HealthOut {
        color,
        state_word,
        cause,
        connected,
        connected_for_s,
        last_msg_age_s,
        last_message_at,
        messages_total: lm.messages.load(Ordering::Relaxed),
        reconnects: lm.reconnects.load(Ordering::Relaxed),
        rejected_far_fixes: lm.rejected_far.load(Ordering::Relaxed),
        far_unmeasured_fixes: lm.far_unmeasured.load(Ordering::Relaxed),
        last_error: lm.last_error.read().await.clone(),
        uptime_s: (now - started) / 1000,
        rate_per_min,
        sparkline,
        fresh,
        stale,
        lost,
        total_devices,
        by_class,
        engine_on,
        with_job,
        avg_accuracy_m,
        fresh_under_s: FRESH_UNDER_S,
        stale_after_s: STALE_AFTER_S,
        plc_connected: lm.plc_connected.load(Ordering::Relaxed),
        plc_devices,
        plc_messages: lm.plc_messages.load(Ordering::Relaxed),
    })
}

// ───────────────────────── ingest loop ─────────────────────────

/// Spawn the background ingest task + a periodic pruner.
/// (active, deployed) TT counts from the work pool (pure TOS, no GPS). active = trucks on a
/// status-A (dispatched) job = working now; deployed = trucks with any A/B/Q job = the tasked
/// fleet (denominator). Utilization = active / deployed.
pub async fn assigned_on_duty(pool: &PgPool) -> (usize, usize) {
    sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
        "SELECT count(DISTINCT ytno) FILTER (WHERE jobstatus = 'A'),
                count(DISTINCT ytno)
           FROM live_assigned_tt WHERE as_of_ts > now() - interval '5 minutes'",
    )
    .fetch_one(pool)
    .await
    .map(|(a, d)| (a.unwrap_or(0) as usize, d.unwrap_or(0) as usize))
    .unwrap_or((0, 0))
}

/// Every 60s, snapshot (active, deployed) into util_tt_sample. Averaging these over a shift
/// yields a TIME-BASED utilization (active/deployed) that accrues history forward.
pub fn spawn_util_sampler(_lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        loop {
            ticker.tick().await;
            let (assigned, on_duty) = assigned_on_duty(&pool).await;
            // skip when the work pool is stale/empty (a near-empty denominator is unreliable)
            if on_duty < 20 {
                continue;
            }
            let (bd, sh) = tt_core::shift::current(tt_core::shift::terminal_now().naive_local());
            if let Err(e) = sqlx::query(
                "INSERT INTO util_tt_sample (business_date, shift, assigned, on_duty) VALUES ($1,$2,$3,$4)",
            )
            .bind(bd)
            .bind(sh.label())
            .bind(assigned as i32)
            .bind(on_duty as i32)
            .execute(&pool)
            .await
            {
                tracing::warn!(error = %e, "util_tt_sample insert failed");
            }
        }
    });
}

/// One soon_idle prediction to persist (collected under read locks, inserted after release).
struct PredRow {
    ytno: String,
    container: String,
    jobtype: Option<String>,
    qc: Option<String>,
    topos: Option<String>,
    source: &'static str,
    gps_would_fire: bool,
    nearest_rtg_m: Option<f64>,
    reason: String,
}

/// One near-miss (first `wait_rtg` entry per trip) to persist into `tt_soon_idle_nearmiss`: a truck
/// arrived-and-loaded at a block but the nearest RTG was BEYOND the soon_idle gate. Joined to actual
/// idle later, it gives P(idle-soon | distance>gate) — the data that lets ⑤'s gate LOOSEN, not just
/// tighten. Without it the gate is blind above the current cutoff (mig 0085).
struct NearMissRow {
    ytno: String,
    container: String,
    jobtype: Option<String>,
    nearest_rtg_m: Option<f64>,
}

/// SHADOW: every 30s, evaluate each TT's dispatch state (read-only `classify_tt`) and log the
/// FIRST soon_idle entry per carry-trip into `tt_soon_idle_pred`, tagged with the firing signal
/// (gps_rtg/tos_actv/qc_plc/both) and a counterfactual `gps_would_fire` flag. The hot path is
/// untouched. Ground truth (actual idle) = `tos_handover_label.comp_ts`, matched in the learn
/// API. See research/soon-idle-tos (다음단계 ④).
pub fn spawn_soon_idle_logger(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(30));
        loop {
            ticker.tick().await;
            let now = Utc::now().timestamp_millis();
            // collect prediction rows + current carry map under read locks, then release
            let (cur_container, soon, nearmiss): (HashMap<String, String>, Vec<PredRow>, Vec<NearMissRow>) = {
                let devices = lm.devices.read().await;
                let plc = lm.plc.read().await;
                let centroids = lm.centroids.read().await;
                let assigned = lm.assigned_pool.read().await;
                let rtgs: Vec<(f64, f64)> = devices
                    .values()
                    .filter(|p| p.cls == "RTG" && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|p| (p.lat, p.lon))
                    .collect();
                let cranes: HashMap<String, (f64, f64)> = devices
                    .iter()
                    .filter(|(_, p)| matches!(p.cls.as_str(), "C" | "M" | "Z") && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|(id, p)| (id.clone(), (p.lat, p.lon)))
                    .collect();
                let cranes = {
                    let line = *lm.quay_line.read().await;
                    let g = lm.crane_wp.read().await;
                    resolve_crane_wp(&line, &g, &cranes)
                };
                let mut cur: HashMap<String, String> = HashMap::new();
                let mut soon: Vec<PredRow> = Vec::new();
                let mut nearmiss: Vec<NearMissRow> = Vec::new();
                for (id, p) in devices.iter() {
                    if p.cls != "TT" || (now - p.last_seen_ms) / 1000 > STALE_AFTER_S {
                        continue;
                    }
                    let aj = assigned.get(id);
                    // trip identity = the carried container (container1 live, latched across
                    // feed gaps, else the assigned order's contno) — matches cycle OPEN.
                    let container = p
                        .container1
                        .clone()
                        .filter(|s| !s.is_empty())
                        .or_else(|| p.latched_container.clone())
                        .or_else(|| aj.and_then(|a| a.contno.clone()));
                    if let Some(c) = &container {
                        cur.insert(id.clone(), c.clone());
                    }
                    let cl = classify_tt(p, aj, &rtgs, &plc, &cranes, &centroids, now);
                    // soon_idle → prediction log; wait_rtg → near-miss log (arrived at block but RTG
                    // beyond the gate — the counterfactual that lets ⑤'s gate loosen safely).
                    if cl.state != "soon_idle" && cl.state != "wait_rtg" {
                        continue;
                    }
                    let Some(container) = container else { continue }; // loaded ⇒ present
                    if cl.state == "wait_rtg" {
                        nearmiss.push(NearMissRow {
                            ytno: id.clone(),
                            container,
                            jobtype: p.jobtype.clone().or_else(|| p.latched_jobtype.clone()),
                            nearest_rtg_m: cl.nearest_rtg_m,
                        });
                        continue;
                    }
                    let reason = cl.reason.clone().unwrap_or_default();
                    // source ↔ classify_tt branch; gps_would_fire = would GPS/PLC alone have fired
                    let (source, gps_would_fire) = if reason.starts_with("블록 RTG 근접") {
                        if aj.is_some_and(|a| a.rtg_active) { ("both", true) } else { ("gps_rtg", true) }
                    } else if reason.starts_with("TOS RTG 활성") {
                        ("tos_actv", false)
                    } else if reason.starts_with("안벽") {
                        ("qc_plc", true)
                    } else {
                        ("other", false)
                    };
                    soon.push(PredRow {
                        ytno: id.clone(),
                        container,
                        jobtype: p.jobtype.clone().or_else(|| p.latched_jobtype.clone()),
                        qc: aj.and_then(|a| a.qc.clone()).or_else(|| p.latched_topos.clone().filter(|t| is_crane_code(t))),
                        topos: p.topos1.clone().or_else(|| p.latched_topos.clone()),
                        source,
                        gps_would_fire,
                        nearest_rtg_m: cl.nearest_rtg_m,
                        reason,
                    });
                }
                (cur, soon, nearmiss)
            };

            // release ended trips (truck no longer carrying that container) + pick new first-entries
            let to_insert: Vec<PredRow> = {
                let mut open = lm.soon_idle_open.lock().await;
                open.retain(|(yt, c0)| cur_container.get(yt).map(|c| c == c0).unwrap_or(false));
                soon.into_iter()
                    .filter(|r| open.insert((r.ytno.clone(), r.container.clone())))
                    .collect()
            };
            // near-misses: first wait_rtg entry per trip (⑤ gate loosen data)
            let nm_insert: Vec<NearMissRow> = {
                let mut open = lm.nearmiss_open.lock().await;
                open.retain(|(yt, c0)| cur_container.get(yt).map(|c| c == c0).unwrap_or(false));
                nearmiss.into_iter()
                    .filter(|r| open.insert((r.ytno.clone(), r.container.clone())))
                    .collect()
            };
            if to_insert.is_empty() && nm_insert.is_empty() {
                continue;
            }
            let (bd, sh) = tt_core::shift::current(tt_core::shift::terminal_now().naive_local());
            for r in &nm_insert {
                let _ = sqlx::query(
                    "INSERT INTO tt_soon_idle_nearmiss
                       (ytno, container, jobtype, observed_at, nearest_rtg_m, business_date, shift)
                     VALUES ($1,$2,$3,now(),$4,$5,$6)
                     ON CONFLICT (ytno, container, observed_at) DO NOTHING",
                )
                .bind(&r.ytno)
                .bind(&r.container)
                .bind(&r.jobtype)
                .bind(r.nearest_rtg_m)
                .bind(bd)
                .bind(sh.label())
                .execute(&pool)
                .await;
            }
            for r in &to_insert {
                if let Err(e) = sqlx::query(
                    "INSERT INTO tt_soon_idle_pred
                       (ytno, container, jobtype, qc, topos, predicted_at, source, gps_would_fire,
                        nearest_rtg_m, reason, business_date, shift)
                     VALUES ($1,$2,$3,$4,$5,now(),$6,$7,$8,$9,$10,$11)
                     ON CONFLICT (ytno, container, predicted_at) DO NOTHING",
                )
                .bind(&r.ytno)
                .bind(&r.container)
                .bind(&r.jobtype)
                .bind(&r.qc)
                .bind(&r.topos)
                .bind(r.source)
                .bind(r.gps_would_fire)
                .bind(r.nearest_rtg_m)
                .bind(&r.reason)
                .bind(bd)
                .bind(sh.label())
                .execute(&pool)
                .await
                {
                    tracing::warn!(error = %e, ytno = %r.ytno, "tt_soon_idle_pred insert failed");
                }
            }
            tracing::info!(logged = to_insert.len(), "soon_idle predictions");
        }
    });
}

/// free_in training + verification (mig 0072). Every 60s snapshot each BUSY truck (a state where free_in
/// applies) with its features + our current prediction + soon-idle flag → free_in_sample. Every ~10 min,
/// BACKFILL the actual free moment (the truck's NEXT drop from tt_cycle_log) → actual_remaining_s, which is
/// both the training LABEL and the verification ("predicted soon-idle → actually freed N s later").
///
/// ⚠⚠ THIS LABEL IS BIASED. DO NOT USE IT TO SCORE A NON-GPS PREDICTOR. ⚠⚠
///
/// `actual_free_at` comes from `tt_cycle_log.dropped_at`, which is a GPS-DERIVED drop event. The TOS
/// crane handover (`tt_move_log.free_ts`) is the authoritative one, and the two do not see the same
/// world. Measured 2026-07-31 over 7 days:
///   · TOS free events 139,783 (twin legs collapsed) vs GPS drops 92,959 → **GPS misses ~33%**
///   · per TOS free event, a GPS drop within 60s exists only 37.3% of the time (DS 32.6 / LD 43.4),
///     and 26.0% have NO GPS drop within ±30 minutes at all
///   · where both DO see the event the medians agree (+28s), which is what mig 0092's "validated
///     ±27–44s vs GPS" recorded — that number is a MEDIAN-ONLY statement about the matched subset
/// The misses are not random: the device only reports on movement, so it goes quiet exactly when a
/// truck is parked waiting for a crane — i.e. the moment "about to be free" matters most.
///
/// Consequence, learned the hard way: an offline head-to-head that scores the GPS free_in estimate
/// against THIS column is scoring it against its own training target, on a population that already
/// dropped the events it would have got wrong. It will conclude GPS wins. It did (2026-07-31) and
/// the conclusion was withdrawn. Score against `tt_move_log.free_ts`, or against real dispatch
/// outcomes (deadline misses, reassignment rate, arrival times) via the A/B harness.
///
/// Kept as-is because the GPS states are still what SELECTS candidates, and this table is the
/// training set for that selection. What was retired (decision recorded, not yet implemented) is
/// the DURATION estimate this label feeds — see the `soon_idle | approaching | wait_rtg` arm in
/// spawn_stage2_shadow.
pub fn spawn_free_in_logger(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        let mut n = 0u64;
        loop {
            ticker.tick().await;
            let now = Utc::now().timestamp_millis();
            let ts = Utc::now();
            let rows: Vec<(String, String, Option<String>, Option<String>, Option<String>, Option<i32>, Option<f64>, i32, bool)> = {
                let devices = lm.devices.read().await;
                let plc = lm.plc.read().await;
                let centroids = lm.centroids.read().await;
                let assigned = lm.assigned_pool.read().await;
                let rtgs: Vec<(f64, f64)> = devices.values()
                    .filter(|p| p.cls == "RTG" && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|p| (p.lat, p.lon)).collect();
                let cranes: HashMap<String, (f64, f64)> = devices.iter()
                    .filter(|(_, p)| matches!(p.cls.as_str(), "C" | "M" | "Z") && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|(id, p)| (id.clone(), (p.lat, p.lon))).collect();
                let cranes = {
                    let line = *lm.quay_line.read().await;
                    let g = lm.crane_wp.read().await;
                    resolve_crane_wp(&line, &g, &cranes)
                };
                let mut out = Vec::new();
                for (id, p) in devices.iter() {
                    if p.cls != "TT" || (now - p.last_seen_ms) / 1000 > STALE_AFTER_S {
                        continue;
                    }
                    let aj = assigned.get(id);
                    let cl = classify_tt(p, aj, &rtgs, &plc, &cranes, &centroids, now);
                    let jt = p.jobtype.clone().or_else(|| p.latched_jobtype.clone());
                    let (pred, _) = free_in(cl.state, jt.as_deref());
                    let Some(pred) = pred else { continue }; // only states where free_in applies
                    let container = p.container1.clone().filter(|s| !s.is_empty())
                        .or_else(|| p.latched_container.clone())
                        .or_else(|| aj.and_then(|a| a.contno.clone()));
                    // cycle elapsed from OPEN (stable per-cycle anchor; carry_since_ms resets mid-journey)
                    let secs_in_cycle = if p.v2.opened_ms > 0 { Some(((now - p.v2.opened_ms) / 1000) as i32) } else { None };
                    let qc = aj.and_then(|a| a.qc.clone()).or_else(|| p.latched_topos.clone().filter(|t| is_crane_code(t)));
                    out.push((id.clone(), cl.state.to_string(), jt, qc, container, secs_in_cycle, cl.nearest_rtg_m, pred as i32, cl.state == "soon_idle"));
                }
                out
            };
            if !rows.is_empty() {
                let ytnos: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
                let states: Vec<String> = rows.iter().map(|r| r.1.clone()).collect();
                let jts: Vec<Option<String>> = rows.iter().map(|r| r.2.clone()).collect();
                let qcs: Vec<Option<String>> = rows.iter().map(|r| r.3.clone()).collect();
                let conts: Vec<Option<String>> = rows.iter().map(|r| r.4.clone()).collect();
                let carry: Vec<Option<i32>> = rows.iter().map(|r| r.5).collect();
                let rtgm: Vec<Option<f64>> = rows.iter().map(|r| r.6).collect();
                let preds: Vec<i32> = rows.iter().map(|r| r.7).collect();
                let soons: Vec<bool> = rows.iter().map(|r| r.8).collect();
                let _ = sqlx::query(
                    "INSERT INTO free_in_sample (ts, ytno, state, jobtype, qc, container, secs_in_cycle, nearest_rtg_m, pred_free_in_s, soon_idle)
                     SELECT $1::timestamptz, u.ytno, u.state, u.jt, u.qc, u.cont, u.carry, u.rtgm, u.pred, u.soon
                       FROM unnest($2::text[], $3::text[], $4::text[], $5::text[], $6::text[], $7::int[], $8::float8[], $9::int[], $10::bool[])
                         AS u(ytno, state, jt, qc, cont, carry, rtgm, pred, soon)
                     ON CONFLICT (ytno, ts) DO NOTHING",
                )
                .bind(ts).bind(&ytnos).bind(&states).bind(&jts).bind(&qcs).bind(&conts).bind(&carry).bind(&rtgm).bind(&preds).bind(&soons)
                .execute(&pool).await;
            }
            n += 1;
            if n % 10 == 0 {
                // backfill: actual free = the truck's NEXT drop after the snapshot (its current carry's drop)
                let _ = sqlx::query(
                    "UPDATE free_in_sample s
                        SET actual_free_at = nd.d, actual_remaining_s = extract(epoch FROM nd.d - s.ts)::int
                       FROM (SELECT fs.ytno, fs.ts,
                                    (SELECT min(c.dropped_at) FROM tt_cycle_log c
                                      WHERE c.ytno = fs.ytno AND c.dropped_at > fs.ts AND c.dropped_at < fs.ts + interval '2 hours') d
                               FROM free_in_sample fs
                              WHERE fs.actual_free_at IS NULL AND fs.ts < now() - interval '2 minutes') nd
                      WHERE s.ytno = nd.ytno AND s.ts = nd.ts AND nd.d IS NOT NULL",
                ).execute(&pool).await;
                crate::db::prune(&pool, "free_in_sample", "DELETE FROM free_in_sample WHERE ts < now() - interval '30 days'").await;
            }
        }
    });
}

/// Load persisted learned topos centroids (block work-point coords) back into memory on
/// startup so accumulation survives restarts. var resets to 0 (spread re-accumulates).
pub async fn load_centroids(lm: &Arc<LiveMap>, pool: &PgPool) {
    let rows: Vec<(String, f64, f64, i32, i64, Option<f64>)> = sqlx::query_as(
        "SELECT topos, lat, lon, n, obs, spread_m FROM learn_topos_point",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let mut c = lm.centroids.write().await;
    for (topos, lat, lon, n, obs, spread_m) in rows {
        // reconstruct EWMA variance from persisted spread_m (isotropic approx) so precision
        // survives restart — without it spread resets to 0 and the precision curve craters.
        let half = spread_m.unwrap_or(0.0) / std::f64::consts::SQRT_2; // per-axis meters
        let cos = lat.to_radians().cos().abs().max(1e-6);
        let var_lat = (half / 111_320.0).powi(2);
        let var_lon = (half / (111_320.0 * cos)).powi(2);
        c.insert(
            topos,
            Centroid { lat, lon, n: n.max(0) as u32, obs: obs.max(0) as u64, var_lat, var_lon },
        );
    }
    tracing::info!(count = c.len(), "loaded learned topos centroids");
}

/// Load persisted learned lane cells (③) back into memory on startup. Reconstruction is
/// exact: (heading, directionality, mean_speed, passes) → (sum_cos, sum_sin, sum_speed).
pub async fn load_lanes(lm: &Arc<LiveMap>, pool: &PgPool) {
    let rows: Vec<(i32, i32, i64, Option<f64>, Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT lat_idx, lon_idx, passes, heading_deg, directionality, mean_speed FROM learn_lane_cell",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let mut l = lm.lanes.write().await;
    for (li, lj, passes, heading, dir, spd) in rows {
        let passes = passes.max(0) as u64;
        let res = dir.unwrap_or(0.0) * passes as f64; // resultant length = R · n
        let h = heading.unwrap_or(0.0).to_radians();
        l.insert(
            (li, lj),
            LaneCell {
                passes,
                sum_cos: res * h.cos(),
                sum_sin: res * h.sin(),
                sum_speed: spd.unwrap_or(0.0) * passes as f64,
            },
        );
    }
    tracing::info!(count = l.len(), "loaded learned lane cells");
}

// ── live map-matching SHADOW ───────────────────────────────────────────────────────────
// GPS is noisy (7% of samples jump >100m) and gappy (14% >30s apart), so cycle decomposition
// misses arrivals (DS pickup ~29% missed). A truck's OD is known, so we route its expected path
// and project the GPS onto it: even when GPS jumps/drops, the furthest-along on-route point tells
// us how close to the destination it got. This task runs it as a SHADOW — per leg it logs the
// route-progress vs the current geofence/ARRIVED signal so we can measure the gain. No live change.
const MM_GATE_M: f64 = 80.0; // a GPS sample farther than this from the route is off-route → ignored

#[derive(Default)]
struct MapMatch {
    dest: String,
    dest_xy: (f64, f64),
    rxy: Vec<(f64, f64)>, // route polyline in local metres
    arc: Vec<f64>,        // cumulative arc-length
    total_m: f64,
    routed: bool,
    progress_m: f64,
    started_ms: i64,
    last_upd_ms: i64,
    last_gps_ms: i64,
    max_gap_s: f64,
    min_dest_m: f64,
    max_jump_m: f64,
    last_xy: Option<(f64, f64)>,
    saw_arrived: bool,
    is_crane: bool,
}
impl MapMatch {
    fn set_route(&mut self, rp: crate::roadgraph::RoutePath) {
        let mut acc = 0.0;
        for (i, &(la, lo)) in rp.pts.iter().enumerate() {
            let p = to_local(la, lo);
            if i > 0 {
                let (px, py) = self.rxy[i - 1];
                acc += (p.0 - px).hypot(p.1 - py);
            }
            self.rxy.push(p);
            self.arc.push(acc);
        }
        self.total_m = acc;
        self.routed = self.total_m > 1.0 && self.rxy.len() >= 2;
    }
    fn project(&mut self, lat: f64, lon: f64) {
        let g = to_local(lat, lon);
        let dd = (g.0 - self.dest_xy.0).hypot(g.1 - self.dest_xy.1);
        if dd < self.min_dest_m {
            self.min_dest_m = dd;
        }
        if !self.routed {
            return;
        }
        let mut best = (f64::INFINITY, 0.0);
        for i in 0..self.rxy.len() - 1 {
            let (p0, p1) = (self.rxy[i], self.rxy[i + 1]);
            let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
            let l2 = dx * dx + dy * dy;
            if l2 < 1e-6 {
                continue;
            }
            let t = (((g.0 - p0.0) * dx + (g.1 - p0.1) * dy) / l2).clamp(0.0, 1.0);
            let d = (g.0 - (p0.0 + t * dx)).hypot(g.1 - (p0.1 + t * dy));
            if d < best.0 {
                best = (d, self.arc[i] + t * (self.arc[i + 1] - self.arc[i]));
            }
        }
        if best.0 <= MM_GATE_M {
            self.progress_m = self.progress_m.max(best.1);
        }
    }
}

struct MmRow {
    ytno: String,
    dest: String,
    is_crane: bool,
    dur_s: i32,
    route_m: f32,
    prog_frac: f32,
    min_dest_m: f32,
    saw_arrived: bool,
    max_gap_s: f32,
    max_jump_m: f32,
}
fn mm_finalize(ytno: &str, mm: &MapMatch) -> Option<MmRow> {
    if !mm.routed || mm.started_ms == 0 || mm.total_m <= 0.0 {
        return None;
    }
    let dur = ((mm.last_upd_ms - mm.started_ms) / 1000) as i32;
    if dur < 20 {
        return None; // too short to be a meaningful leg
    }
    Some(MmRow {
        ytno: ytno.to_string(),
        dest: mm.dest.clone(),
        is_crane: mm.is_crane,
        dur_s: dur,
        route_m: mm.total_m as f32,
        prog_frac: (mm.progress_m / mm.total_m).clamp(0.0, 1.0) as f32,
        min_dest_m: if mm.min_dest_m.is_finite() { mm.min_dest_m as f32 } else { -1.0 },
        saw_arrived: mm.saw_arrived,
        max_gap_s: mm.max_gap_s as f32,
        max_jump_m: mm.max_jump_m as f32,
    })
}

/// SHADOW map-matcher: every 5s, project each TT's GPS onto its expected road route (cached per leg)
/// and, at each leg transition, log route-progress vs the current arrival signal → `mm_arrival_shadow`.
pub fn spawn_mapmatch_shadow(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        let mut graph = crate::roadgraph::RoadGraph::load(&pool).await;
        let mut age_s: i64 = 0;
        let mut state: HashMap<String, MapMatch> = HashMap::new();
        let mut ticker = tokio::time::interval(Duration::from_secs(5));
        loop {
            ticker.tick().await;
            age_s += 5;
            if graph.is_none() || age_s >= 3600 {
                if let Some(gg) = crate::roadgraph::RoadGraph::load(&pool).await {
                    graph = Some(gg);
                }
                age_s = 0;
            }
            let Some(g) = graph.as_ref() else { continue };
            let now = Utc::now().timestamp_millis();
            let (tts, cents): (
                Vec<(String, f64, f64, Option<String>, bool, i64)>,
                HashMap<String, (f64, f64)>,
            ) = {
                let d = lm.devices.read().await;
                let c = lm.centroids.read().await;
                let tts = d
                    .iter()
                    .filter(|(_, p)| p.cls == "TT" && (now - p.last_seen_ms) / 1000 <= 600)
                    .map(|(id, p)| {
                        (id.clone(), p.lat, p.lon, p.topos1.clone(), p.arrival.as_deref() == Some("ARRIVED"), p.last_seen_ms)
                    })
                    .collect();
                let cents = c.iter().map(|(k, v)| (k.clone(), (v.lat, v.lon))).collect();
                (tts, cents)
            };
            let mut rows: Vec<MmRow> = Vec::new();
            let mut present: HashSet<String> = HashSet::new();
            for (id, lat, lon, topos1, arrived, last_seen) in tts {
                present.insert(id.clone());
                let dest_t = topos1.filter(|t| !t.is_empty() && t != "0");
                let entry = state.entry(id.clone()).or_default();
                let mut active = false;
                match dest_t.as_deref() {
                    Some(dt) if dt == entry.dest && entry.started_ms != 0 => {
                        active = true;
                    }
                    Some(dt) => {
                        if let Some(r) = mm_finalize(&id, entry) {
                            rows.push(r);
                        }
                        let dest_pos = cents.get(dt).or_else(|| cents.get(block_prefix(dt))).copied();
                        *entry = MapMatch {
                            dest: dt.to_string(),
                            is_crane: is_crane_code(dt),
                            started_ms: now,
                            last_upd_ms: now,
                            min_dest_m: f64::INFINITY,
                            ..Default::default()
                        };
                        if let Some((dla, dlo)) = dest_pos {
                            entry.dest_xy = to_local(dla, dlo);
                            if let Some(rp) = g.route_path(lat, lon, dla, dlo) {
                                entry.set_route(rp);
                            }
                        }
                        active = true;
                    }
                    None => {
                        if let Some(r) = mm_finalize(&id, entry) {
                            rows.push(r);
                        }
                        *entry = MapMatch::default();
                    }
                }
                if active && entry.routed {
                    if entry.last_gps_ms > 0 && last_seen > entry.last_gps_ms {
                        let gap = (last_seen - entry.last_gps_ms) as f64 / 1000.0;
                        if gap > entry.max_gap_s {
                            entry.max_gap_s = gap;
                        }
                    }
                    if let Some((lx, ly)) = entry.last_xy {
                        let (nx, ny) = to_local(lat, lon);
                        let jmp = (nx - lx).hypot(ny - ly);
                        if jmp > entry.max_jump_m {
                            entry.max_jump_m = jmp;
                        }
                    }
                    entry.project(lat, lon);
                    if arrived {
                        entry.saw_arrived = true;
                    }
                    entry.last_gps_ms = last_seen;
                    entry.last_xy = Some(to_local(lat, lon));
                    entry.last_upd_ms = now;
                }
            }
            let gone: Vec<String> = state.keys().filter(|k| !present.contains(k.as_str())).cloned().collect();
            for k in &gone {
                if let Some(mm) = state.get(k) {
                    if let Some(r) = mm_finalize(k, mm) {
                        rows.push(r);
                    }
                }
                state.remove(k);
            }
            for r in &rows {
                let _ = sqlx::query(
                    "INSERT INTO mm_arrival_shadow (ytno,dest_topos,is_crane,leg_dur_s,route_m,progress_frac,min_dest_m,saw_arrived,max_gap_s,max_jump_m)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
                )
                .bind(&r.ytno).bind(&r.dest).bind(r.is_crane).bind(r.dur_s)
                .bind(r.route_m).bind(r.prog_frac).bind(r.min_dest_m).bind(r.saw_arrived)
                .bind(r.max_gap_s).bind(r.max_jump_m)
                .execute(&pool).await;
            }
        }
    });
}

/// Every 5 min, persist in-memory learned topos centroids → `learn_topos_point` (the block
/// work-point coordinate model). Hourly, snapshot model quality → `learn_topos_metric`
/// (coverage·precision over time = the "model improving" curve).
pub fn spawn_learn_persist(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(300));
        loop {
            ticker.tick().await;
            let snap: Vec<(String, Centroid)> = {
                let c = lm.centroids.read().await;
                c.iter().map(|(k, v)| (k.clone(), *v)).collect()
            };
            let line = *lm.quay_line.read().await;
            let cwp: HashMap<String, CraneWp> = lm.crane_wp.read().await.clone();
            for (topos, c) in &snap {
                if c.obs == 0 {
                    continue;
                }
                // QC: replace the swing-smeared all-time centroid with the recency handover
                // centroid projected onto the learned quay line (falls back to the plain
                // centroid when there's no recent handover / the line isn't fitted yet).
                let (plat, plon, pspread) = if is_crane_code(topos) {
                    match cwp.get(topos).and_then(|w| w.centroid()) {
                        Some((cx, cy)) => {
                            let (px, py) = line.project(cx, cy);
                            let (la, lo) = from_local(px, py);
                            (la, lo, 15.0_f64)
                        }
                        None => (c.lat, c.lon, c.spread_m()),
                    }
                } else {
                    (c.lat, c.lon, c.spread_m())
                };
                if let Err(e) = sqlx::query(
                    "INSERT INTO learn_topos_point (topos, is_crane, lat, lon, n, obs, spread_m, updated_at)
                       VALUES ($1,$2,$3,$4,$5,$6,$7, now())
                     ON CONFLICT (topos) DO UPDATE SET
                       is_crane=$2, lat=$3, lon=$4, n=$5, obs=$6, spread_m=$7, updated_at=now()",
                )
                .bind(topos)
                .bind(is_crane_code(topos))
                .bind(plat)
                .bind(plon)
                .bind(c.n as i32)
                .bind(c.obs as i64)
                .bind(pspread)
                .execute(&pool)
                .await
                {
                    tracing::warn!(error = %e, topos = %topos, "learn_topos_point upsert failed");
                    break; // DB hiccup — retry next tick
                }
            }
            // model-quality snapshot every tick (5 min), only when there are points
            // (HAVING skips empty restart snapshots); powers the "model improving" curve.
            let _ = sqlx::query(
                "INSERT INTO learn_topos_metric
                   (captured_at, distinct_topos, confident_topos, total_obs, median_spread_m)
                 SELECT now(), count(*), count(*) FILTER (WHERE n >= 30),
                        coalesce(sum(obs), 0)::bigint,
                        percentile_cont(0.5) WITHIN GROUP (ORDER BY spread_m) FILTER (WHERE n >= 30)
                   FROM learn_topos_point
                  HAVING count(*) > 0
                 ON CONFLICT (captured_at) DO NOTHING",
            )
            .execute(&pool)
            .await;
            crate::db::prune(&pool, "learn_topos_metric", "DELETE FROM learn_topos_metric WHERE captured_at < now() - interval '30 days'").await;

            // ── soon-idle accuracy snapshot (④): precision/lead per (jobtype,source) + recall ──
            // per jobtype (source='ALL'), over a 24h window. Powers the GPS-vs-TOS improvement
            // curve. GPS-FIRST ground truth (matches /api/learn/soon-idle): idle = the truck's own
            // cycle close (tt_cycle_v2.dropped_at, physical free, broad coverage), TOS label fallback
            // (LD=dis_ts·DS=comp_ts) on a GPS gap. Recall (below) stays on TOS authoritative truth
            // (container-keyed for DS). HAVING skips empty windows; 30d retention.
            let _ = sqlx::query(
                "INSERT INTO tt_soon_idle_metric
                   (captured_at, jobtype, source, window_h, predictions, matched, precision_pct, lead_p10_s, lead_p50_s, lead_p90_s)
                 SELECT now(), m.jobtype, m.source, 24, count(*), count(m.idle_ts),
                        (100.0*count(m.idle_ts)/nullif(count(*),0))::float8,
                        percentile_cont(0.1) WITHIN GROUP (ORDER BY m.lead_s) FILTER (WHERE m.lead_s >= 0),
                        percentile_cont(0.5) WITHIN GROUP (ORDER BY m.lead_s) FILTER (WHERE m.lead_s >= 0),
                        percentile_cont(0.9) WITHIN GROUP (ORDER BY m.lead_s) FILTER (WHERE m.lead_s >= 0)
                   FROM (
                     SELECT j.jobtype, j.source, j.idle_ts, EXTRACT(EPOCH FROM (j.idle_ts - j.predicted_at)) AS lead_s FROM (
                       SELECT p.jobtype, p.source, p.predicted_at,
                              coalesce(g.dropped_at, CASE WHEN p.jobtype = 'LD' THEN coalesce(t.dis_ts, t.comp_ts) ELSE t.comp_ts END) AS idle_ts
                         FROM tt_soon_idle_pred p
                         LEFT JOIN LATERAL (
                           SELECT dropped_at FROM tt_cycle_v2 c
                            WHERE c.ytno = p.ytno AND c.jobtype = p.jobtype
                              AND c.dropped_at >= p.predicted_at - interval '90 seconds'
                              AND c.dropped_at <  p.predicted_at + interval '20 minutes'
                            ORDER BY c.dropped_at LIMIT 1
                         ) g ON true
                         LEFT JOIN LATERAL (
                           SELECT dis_ts, comp_ts FROM tos_handover_label h
                            WHERE h.ytno = p.ytno AND (p.jobtype <> 'DS' OR h.contno = p.container)
                              AND h.comp_ts >= p.predicted_at - interval '60 seconds'
                              AND h.comp_ts <  p.predicted_at + interval '20 minutes'
                            ORDER BY abs(EXTRACT(EPOCH FROM (h.comp_ts - p.predicted_at))) LIMIT 1
                         ) t ON true
                        WHERE p.predicted_at > now() - interval '24 hours'
                     ) j
                   ) m
                  GROUP BY m.jobtype, m.source
                 HAVING count(*) > 0
                 ON CONFLICT DO NOTHING",
            )
            .execute(&pool)
            .await;
            let _ = sqlx::query(
                "INSERT INTO tt_soon_idle_metric
                   (captured_at, jobtype, source, window_h, predictions, matched, recall_pct)
                 SELECT now(), t.jobtype, 'ALL', 24, count(*), count(t.pid),
                        (100.0*count(t.pid)/nullif(count(*),0))::float8
                   FROM (
                     SELECT h.jobtype, p.id AS pid
                       FROM tos_handover_label h
                       LEFT JOIN LATERAL (
                         SELECT id FROM tt_soon_idle_pred p
                          WHERE p.ytno = h.ytno AND (h.jobtype <> 'DS' OR p.container = h.contno)
                            AND p.predicted_at BETWEEN h.comp_ts - interval '60 minutes' AND h.comp_ts + interval '60 seconds'
                          ORDER BY abs(EXTRACT(EPOCH FROM (h.comp_ts - p.predicted_at))) LIMIT 1
                       ) p ON true
                      WHERE h.comp_ts > now() - interval '24 hours'
                        AND h.comp_ts < now() - interval '180 seconds'
                        AND h.comp_ts > (SELECT min(predicted_at) FROM tt_soon_idle_pred) + interval '5 minutes'
                   ) t
                  GROUP BY t.jobtype
                 HAVING count(*) > 0
                 ON CONFLICT DO NOTHING",
            )
            .execute(&pool)
            .await;
            crate::db::prune(&pool, "tt_soon_idle_metric", "DELETE FROM tt_soon_idle_metric WHERE captured_at < now() - interval '30 days'").await;

            // ── lanes (③): persist grid cells (skip the 1-2 pass noise tail) + quality ──
            let lsnap: Vec<((i32, i32), LaneCell)> = {
                let l = lm.lanes.read().await;
                l.iter().map(|(k, v)| (*k, *v)).collect()
            };
            for ((li, lj), c) in &lsnap {
                if c.passes < 3 {
                    continue;
                }
                if let Err(e) = sqlx::query(
                    "INSERT INTO learn_lane_cell
                       (lat_idx, lon_idx, lat, lon, passes, heading_deg, directionality, mean_speed, updated_at)
                       VALUES ($1,$2,$3,$4,$5,$6,$7,$8, now())
                     ON CONFLICT (lat_idx, lon_idx) DO UPDATE SET
                       lat=$3, lon=$4, passes=$5, heading_deg=$6, directionality=$7, mean_speed=$8, updated_at=now()",
                )
                .bind(li)
                .bind(lj)
                .bind(*li as f64 * LANE_CELL_DEG)
                .bind(*lj as f64 * LANE_CELL_DEG)
                .bind(c.passes as i64)
                .bind(c.heading_deg())
                .bind(c.directionality())
                .bind(c.mean_speed())
                .execute(&pool)
                .await
                {
                    tracing::warn!(error = %e, "learn_lane_cell upsert failed");
                    break;
                }
            }
            let _ = sqlx::query(
                "INSERT INTO learn_lane_metric (captured_at, cells, road_cells, total_passes, oneway_frac)
                 SELECT now(), count(*), count(*) FILTER (WHERE passes >= 20),
                        coalesce(sum(passes), 0)::bigint,
                        (count(*) FILTER (WHERE passes >= 20 AND directionality >= 0.8))::float8
                          / nullif(count(*) FILTER (WHERE passes >= 20), 0)
                   FROM learn_lane_cell
                  HAVING count(*) > 0
                 ON CONFLICT (captured_at) DO NOTHING",
            )
            .execute(&pool)
            .await;
            crate::db::prune(&pool, "learn_lane_metric", "DELETE FROM learn_lane_metric WHERE captured_at < now() - interval '30 days'").await;
        }
    });
}

/// Every 5 min, harvest TT travel-time labels (①) from validated cycles: for each pair of
/// consecutive legs, (origin→dest) travel = depart(left) → arrive(arrived). Distance from
/// learned topos coords (②). Idempotent (PK ytno,dropped_at,leg_ord). DB→DB; no LiveMap.
/// Every 60s, snapshot live TT density per cell at 4 grid sizes (50/100/150/200m) → zone_density.
/// Internal (reads in-memory positions; no external call). Rolling buffer pruned to 4 days. Feeds
/// the travel-time congestion feature (which resolution predicts trip time best — TBD). DB→DB.
pub fn spawn_density_sampler(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        const SIZES: [i32; 4] = [50, 100, 150, 200];
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        let mut tick = 0u64;
        loop {
            ticker.tick().await;
            tick += 1;
            let now = Utc::now().timestamp_millis();
            let mut counts: HashMap<(i32, i32, i32), i32> = HashMap::new(); // (grid_m, cx, cy) -> n
            {
                let devices = lm.devices.read().await;
                for p in devices.values() {
                    if p.cls != "TT" || (now - p.last_seen_ms) / 1000 > STALE_AFTER_S {
                        continue;
                    }
                    for &m in &SIZES {
                        let deg = m as f64 / 111320.0;
                        let cx = (p.lat / deg).round() as i32;
                        let cy = (p.lon / deg).round() as i32;
                        *counts.entry((m, cx, cy)).or_insert(0) += 1;
                    }
                }
            }
            if counts.is_empty() {
                continue;
            }
            let ts = Utc::now();
            let (mut gm, mut cxs, mut cys, mut ns) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for ((m, cx, cy), n) in &counts {
                gm.push(*m);
                cxs.push(*cx);
                cys.push(*cy);
                ns.push(*n);
            }
            let _ = sqlx::query(
                "INSERT INTO zone_density (ts, grid_m, cx, cy, n)
                 SELECT $1, g, x, y, c
                   FROM UNNEST($2::int4[], $3::int4[], $4::int4[], $5::int4[]) AS t(g, x, y, c)
                 ON CONFLICT (ts, grid_m, cx, cy) DO NOTHING",
            )
            .bind(ts)
            .bind(&gm)
            .bind(&cxs)
            .bind(&cys)
            .bind(&ns)
            .execute(&pool)
            .await;
            if tick % 10 == 0 {
                crate::db::prune(&pool, "zone_density", "DELETE FROM zone_density WHERE ts < now() - interval '4 days'").await;
            }
        }
    });
}

/// K_QC_TT_WAIT_GPS history: every 30s snapshot live QC starvation (QC PLC idle + no truck) into
/// qc_wait_sample, logged TWO ways for reliability comparison — topos1-code based (current live
/// signal) vs GPS-distance based (no fresh TT within CRANE_ARRIVE_M of the crane). If the topos
/// count runs well above the GPS count, the live value is inflated by dropped topos1 fields.
pub fn spawn_qc_wait_logger(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(30));
        let mut tick = 0u64;
        let mut smooth: VecDeque<(i64, Option<i64>)> = VecDeque::new(); // last 6 ticks: (real, wait_avg)
        loop {
            ticker.tick().await;
            tick += 1;
            if !lm.connected.load(Ordering::Relaxed) {
                continue;
            }
            // QCs that have pending work — gates out no-work idle. (A stricter "mid active queue"
            // gate to also strip hatch/bay transitions was tried but live_workqueue.comp_qty/seq
            // don't reliably identify the crane's CURRENT queue — out-of-order/lagging comp made it
            // over-exclude genuine starvation, so we keep the reliable pending-work gate.)
            let pending: std::collections::HashSet<String> = sqlx::query_scalar::<_, String>(
                "SELECT qc FROM live_workqueue WHERE total_qty > comp_qty GROUP BY qc",
            )
            .fetch_all(&pool)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
            // a crane only genuinely needs a truck if it is ACTUALLY working now — i.e. has in-flight
            // moves in the work pool. "has future queued work" (pending) wrongly flags not-yet-started
            // cranes (lots of slack) as starving; require active moves to keep the alarm honest.
            let active: std::collections::HashSet<String> = sqlx::query_scalar::<_, String>(
                "SELECT DISTINCT qc FROM live_workpool WHERE qc IS NOT NULL AND qc <> ''",
            )
            .fetch_all(&pool)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
            // best-effort "next block" each QC is working/waiting on = its lowest-seq incomplete
            // queue (per-container is impossible; MSNSEQ is 100% NULL). For the per-QC starvation log.
            let next_q: HashMap<String, (Option<String>, Option<String>)> =
                sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
                    "SELECT DISTINCT ON (qc) qc, vessel, queuename FROM live_workqueue
                      WHERE total_qty > comp_qty ORDER BY qc, seq",
                )
                .fetch_all(&pool)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(qc, v, q)| (qc, (v, q)))
                .collect();
            let ts = Utc::now(); // single timestamp shared by all per-QC rows this tick
            // per-QC starvation rows collected in the loop below: (qc, idle_s, no_truck_gps,
            // no_truck_topos, pending, starving_real, near_idle_tt, next_vessel, next_queuename)
            let mut qc_rows: Vec<(String, i64, bool, bool, bool, bool, i32, Option<String>, Option<String>, bool)> =
                Vec::new();
            let now = Utc::now().timestamp_millis();
            let (working, st, wt, sg, wg, sb, sr, wr, pos_known) = {
                let map = lm.devices.read().await;
                let plc = lm.plc.read().await;
                let centroids = lm.centroids.read().await;
                // crane live GPS positions (C/M/Z, fresh)
                let cranes: HashMap<&str, (f64, f64)> = map
                    .iter()
                    .filter(|(_, p)| matches!(p.cls.as_str(), "C" | "M" | "Z") && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|(id, p)| (id.as_str(), (p.lat, p.lon)))
                    .collect();
                // topos1-based "this crane has a truck" (current live logic)
                let cranes_with_tt: std::collections::HashSet<&str> = map
                    .values()
                    .filter(|p| p.cls == "TT" && p.arrival.as_deref() == Some("ARRIVED"))
                    .filter_map(|p| p.topos1.as_deref())
                    .filter(|t| is_crane_code(t))
                    .collect();
                // fresh TT positions for the GPS-distance check
                let tt_pos: Vec<(f64, f64)> = map
                    .values()
                    .filter(|p| p.cls == "TT" && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|p| (p.lat, p.lon))
                    .collect();
                // genuinely AVAILABLE trucks = fresh + empty (no container) + unassigned (no topos1
                // target). Excludes loaded/en-route/assigned trucks committed elsewhere, so
                // near_idle_tt counts trucks that could actually have served this crane (location
                // control: "no truck was free nearby" = Stage-2, vs "free truck nearby but not sent
                // in time" = Stage-1). NOT `arrival != ARRIVED`, which counts committed trucks.
                let tt_free: Vec<(f64, f64)> = map
                    .values()
                    .filter(|p| p.cls == "TT" && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S
                        && p.container1.as_deref().map(|c| c.trim().is_empty()).unwrap_or(true)
                        && p.topos1.as_deref().map(|t| t.trim().is_empty()).unwrap_or(true))
                    .map(|p| (p.lat, p.lon))
                    .collect();
                let (mut working, mut st, mut wt, mut sg, mut wg, mut sb, mut sr, mut wr, mut pos_known) =
                    (0i64, 0i64, 0i64, 0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
                for (id, c) in plc.iter() {
                    let is_working = c.moves.iter().any(|&t| now - t <= MOVE_WINDOW_MS);
                    let fresh = (now - c.last_seen_ms) / 1000 <= STALE_AFTER_S;
                    if !is_working || !fresh || c.last_move_ms == 0 {
                        continue;
                    }
                    working += 1;
                    // crane position: prefer live crane GPS (current gantry), else learned centroid.
                    // Only cranes with a known position are scored, so topos vs gps share a denominator.
                    let Some(cp) = cranes.get(id.as_str()).copied().or_else(|| centroids.get(id).map(|c| (c.lat, c.lon))) else {
                        continue;
                    };
                    pos_known += 1;
                    let idle_s = (now - c.last_move_ms) / 1000;
                    // (1) topos1-based starvation = current live signal (no TT assigned to this crane)
                    let topos_starv = !cranes_with_tt.contains(id.as_str());
                    // (2) GPS-distance starvation = no fresh TT within CRANE_ARRIVE_M of the crane
                    let gps_starv = !tt_pos.iter().any(|&t| dist_m(cp, t) <= CRANE_ARRIVE_M);
                    // per-QC time series (validation): log EVERY working+positioned crane — not only
                    // starving ones — so episode start/end (gps_starv true→false = truck arrived) and
                    // idle duration are reconstructable. Collect BEFORE the idle gate below.
                    let pend = pending.contains(id.as_str());
                    let working = active.contains(id.as_str()); // has in-flight moves = actually working
                    let near_idle_tt = tt_free.iter().filter(|&&t| dist_m(cp, t) <= NEAR_TT_M).count() as i32;
                    let (nv, nq) = next_q.get(id.as_str()).cloned().unwrap_or((None, None));
                    // genuine starvation: idle past threshold, no truck, AND actually mid-work (not just
                    // future queued work) — excludes not-yet-started cranes that merely have slack.
                    let starv = idle_s > QCQ_IDLE_S && gps_starv && pend && working;
                    qc_rows.push((
                        id.clone(), idle_s, gps_starv, topos_starv, pend,
                        starv, near_idle_tt, nv, nq,
                        starv && near_idle_tt == 0, // genuine: starving AND no free truck nearby
                    ));
                    if idle_s <= QCQ_IDLE_S {
                        continue;
                    }
                    if topos_starv {
                        st += 1;
                        wt += idle_s;
                    }
                    if gps_starv {
                        sg += 1;
                        wg += idle_s;
                    }
                    if topos_starv && gps_starv {
                        sb += 1;
                    }
                    // real TT-starvation: GPS no-truck AND the crane has pending work
                    if gps_starv && pending.contains(id.as_str()) {
                        sr += 1;
                        wr += idle_s;
                    }
                }
                (working, st, wt, sg, wg, sb, sr, wr, pos_known)
            };
            // per-QC starvation time series (dispatch validation) — bulk insert this tick's rows
            if !qc_rows.is_empty() {
                let qa: Vec<String> = qc_rows.iter().map(|r| r.0.clone()).collect();
                let ia: Vec<i32> = qc_rows.iter().map(|r| r.1 as i32).collect();
                let nga: Vec<bool> = qc_rows.iter().map(|r| r.2).collect();
                let nta: Vec<bool> = qc_rows.iter().map(|r| r.3).collect();
                let pea: Vec<bool> = qc_rows.iter().map(|r| r.4).collect();
                let sra: Vec<bool> = qc_rows.iter().map(|r| r.5).collect();
                let nia: Vec<i32> = qc_rows.iter().map(|r| r.6).collect();
                let nva: Vec<Option<String>> = qc_rows.iter().map(|r| r.7.clone()).collect();
                let nqa: Vec<Option<String>> = qc_rows.iter().map(|r| r.8.clone()).collect();
                let ga: Vec<bool> = qc_rows.iter().map(|r| r.9).collect();
                let _ = sqlx::query(
                    "INSERT INTO qc_wait_qc_sample
                       (ts, qc, idle_s, no_truck_gps, no_truck_topos, pending, starving_real, near_idle_tt, next_vessel, next_queuename, genuine)
                     SELECT $1, * FROM unnest($2::text[], $3::int[], $4::bool[], $5::bool[], $6::bool[], $7::bool[], $8::int[], $9::text[], $10::text[], $11::bool[])
                     ON CONFLICT (ts, qc) DO NOTHING",
                )
                .bind(ts)
                .bind(&qa).bind(&ia).bind(&nga).bind(&nta).bind(&pea).bind(&sra).bind(&nia).bind(&nva).bind(&nqa).bind(&ga)
                .execute(&pool)
                .await;
            }
            let wt_avg = (st > 0).then(|| wt / st);
            let wg_avg = (sg > 0).then(|| wg / sg);
            let wr_avg = (sr > 0).then(|| wr / sr);
            let _ = sqlx::query(
                "INSERT INTO qc_wait_sample (ts, working_qc, starving_topos, wait_topos_s, starving_gps, wait_gps_s, starving_both, pos_known_qc, starving_real, wait_real_s)
                 VALUES (now(), $1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (ts) DO NOTHING",
            )
            .bind(working)
            .bind(st)
            .bind(wt_avg)
            .bind(sg)
            .bind(wg_avg)
            .bind(sb)
            .bind(pos_known)
            .bind(sr)
            .bind(wr_avg)
            .execute(&pool)
            .await;
            // smoothed live value (rolling mean over the last ~3 min) for the positions endpoint
            smooth.push_back((sr, wr_avg));
            while smooth.len() > 6 {
                smooth.pop_front();
            }
            let n = smooth.len().max(1) as f64;
            let avg_count = (smooth.iter().map(|(c, _)| *c).sum::<i64>() as f64 / n).round() as usize;
            let waits: Vec<i64> = smooth.iter().filter_map(|(_, w)| *w).collect();
            let avg_wait = (!waits.is_empty()).then(|| waits.iter().sum::<i64>() / waits.len() as i64);
            *lm.qc_wait_live.write().await = Some((avg_count, avg_wait));
            if tick % 20 == 0 {
                crate::db::prune(&pool, "qc_wait_sample", "DELETE FROM qc_wait_sample WHERE ts < now() - interval '14 days'").await;
                // 21d to match dispatch_pred_sample so the join window isn't truncated
                crate::db::prune(&pool, "qc_wait_qc_sample", "DELETE FROM qc_wait_qc_sample WHERE ts < now() - interval '21 days'").await;
            }
        }
    });
}

/// Persist the GPS-confirmed QC truck-wait signal as a daily/shift KPI (K_QC_TT_WAIT_GPS = avg
/// concurrent cranes waiting for a truck). Aggregates the 30s qc_wait_sample ticks (starving_real)
/// by terminal business-date + shift every 5 min and upserts kpi_daily/kpi_shift, so it survives
/// the sample table's 14-day prune and gets history/trend like the Oracle-derived KPIs.
pub fn spawn_qc_wait_kpi(pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(300));
        let off = tt_core::shift::terminal_offset();
        loop {
            ticker.tick().await;
            let rows: Vec<(DateTime<Utc>, i32)> = sqlx::query_as(
                "SELECT ts, starving_real FROM qc_wait_sample WHERE starving_real IS NOT NULL",
            )
            .fetch_all(&pool)
            .await
            .unwrap_or_default();
            if rows.is_empty() {
                continue;
            }
            let today = tt_core::shift::current(tt_core::shift::terminal_now().naive_local()).0;
            let mut daily: HashMap<chrono::NaiveDate, (i64, i64)> = HashMap::new(); // bd → (sum, n)
            let mut byshift: HashMap<(chrono::NaiveDate, &'static str), (i64, i64)> = HashMap::new();
            for (ts, sr) in &rows {
                let (bd, sh) = tt_core::shift::current(ts.with_timezone(&off).naive_local());
                let d = daily.entry(bd).or_insert((0, 0));
                d.0 += *sr as i64;
                d.1 += 1;
                let s = byshift.entry((bd, sh.label())).or_insert((0, 0));
                s.0 += *sr as i64;
                s.1 += 1;
            }
            for (bd, (sum, n)) in &daily {
                let avg = *sum as f64 / *n as f64;
                let _ = sqlx::query(
                    "INSERT INTO kpi_daily (kpi_key, snapshot_date, value, sample_n, unit, source_grain, is_provisional, computed_at)
                     VALUES ('K_QC_TT_WAIT_GPS', $1, $2, $3, 'QC', 'live-gps-30s', $4, now())
                     ON CONFLICT (kpi_key, snapshot_date) DO UPDATE SET
                       value=EXCLUDED.value, sample_n=EXCLUDED.sample_n, is_provisional=EXCLUDED.is_provisional, computed_at=now()",
                )
                .bind(bd)
                .bind(avg)
                .bind(*n as i32)
                .bind(*bd >= today)
                .execute(&pool)
                .await;
            }
            for ((bd, sh_label), (sum, n)) in &byshift {
                let avg = *sum as f64 / *n as f64;
                let Some(sh) = tt_core::shift::Shift::from_label(sh_label) else { continue };
                let window_start = tt_core::shift::terminal_to_utc(tt_core::shift::window(*bd, sh).0);
                let _ = sqlx::query(
                    "INSERT INTO kpi_shift (business_date, shift, kpi_key, value, sample_n, unit, as_of_ts, window_start, computed_at)
                     VALUES ($1, $2, 'K_QC_TT_WAIT_GPS', $3, $4, 'QC', now(), $5, now())
                     ON CONFLICT (business_date, shift, kpi_key) DO UPDATE SET
                       value=EXCLUDED.value, sample_n=EXCLUDED.sample_n, as_of_ts=now()",
                )
                .bind(bd)
                .bind(*sh_label)
                .bind(avg)
                .bind(*n as i32)
                .bind(window_start)
                .execute(&pool)
                .await;
            }
        }
    });
}

pub fn spawn_travel_aggregator(pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(300));
        loop {
            ticker.tick().await;
            let _ = sqlx::query(
                "WITH legs AS (
                   SELECT v2.ytno, v2.dropped_at, e.ord,
                          e.val->>'target' AS target,
                          (e.val->>'lat')::float8 AS lat,
                          (e.val->>'lon')::float8 AS lon,
                          (e.val->>'left')::bigint AS left_ms,
                          (e.val->>'arrived')::bigint AS arr_ms
                     FROM tt_cycle_v2 v2,
                          jsonb_array_elements(v2.legs) WITH ORDINALITY e(val, ord)
                    WHERE v2.dropped_at > now() - interval '15 minutes'
                 )
                 INSERT INTO learn_travel_sample
                   (ytno, dropped_at, leg_ord, origin, dest, travel_s, dist_m, hour,
                    origin_zone, dest_zone, dow, congestion,
                    origin_lat, origin_lon, dest_lat, dest_lon, trip_ts)
                 SELECT a.ytno, a.dropped_at, a.ord, a.target, b.target,
                        ((b.arr_ms - a.left_ms) / 1000)::int,
                        CASE WHEN po.topos IS NOT NULL AND pd.topos IS NOT NULL THEN
                          sqrt( power((pd.lat - po.lat) * 111320.0, 2)
                              + power((pd.lon - po.lon) * 111320.0 * cos(radians((po.lat + pd.lat) / 2)), 2) )
                        END,
                        extract(hour FROM to_timestamp(b.arr_ms / 1000.0))::int,
                        travel_zone(a.target, a.lat, a.lon),
                        travel_zone(b.target, b.lat, b.lon),
                        extract(dow FROM to_timestamp(b.arr_ms / 1000.0))::int,
                        (SELECT count(*) FROM tt_cycle_v2 cc
                          WHERE cc.opened_at <= to_timestamp(b.arr_ms / 1000.0)
                            AND cc.dropped_at >= to_timestamp(b.arr_ms / 1000.0))::int,
                        a.lat, a.lon, b.lat, b.lon,
                        to_timestamp((a.left_ms + b.arr_ms) / 2.0 / 1000.0)
                   FROM legs a
                   JOIN legs b ON a.ytno = b.ytno AND a.dropped_at = b.dropped_at AND b.ord = a.ord + 1
                   LEFT JOIN learn_topos_point po ON po.topos = a.target
                   LEFT JOIN learn_topos_point pd ON pd.topos = b.target
                  WHERE a.left_ms > 0 AND b.arr_ms > 0 AND a.target <> b.target
                    AND (b.arr_ms - a.left_ms) BETWEEN 10000 AND 7200000
                 ON CONFLICT (ytno, dropped_at, leg_ord) DO NOTHING",
            )
            .execute(&pool)
            .await;
            // empty trip: origin = previous cycle's drop topos (lag per truck), dest = this
            // cycle's pickup topos, time = empty_travel_start → empty_arrived. leg_ord=0
            // (distinct from laden leg ordinals ≥1). Only cycles with a clean empty-travel start.
            let _ = sqlx::query(
                "WITH cyc AS (
                   SELECT v2.ytno, v2.dropped_at, v2.empty_travel_start_at, v2.empty_arrived_at,
                          (v2.legs->0->>'target') AS pickup_topos,
                          (v2.legs->0->>'lat')::float8 AS pickup_lat,
                          (v2.legs->0->>'lon')::float8 AS pickup_lon,
                          (v2.legs->(jsonb_array_length(v2.legs)-1)->>'target') AS drop_topos,
                          (v2.legs->(jsonb_array_length(v2.legs)-1)->>'lat')::float8 AS drop_lat,
                          (v2.legs->(jsonb_array_length(v2.legs)-1)->>'lon')::float8 AS drop_lon
                     FROM tt_cycle_v2 v2
                    WHERE v2.dropped_at > now() - interval '30 minutes'
                      AND jsonb_array_length(v2.legs) >= 1
                 ),
                 wp AS (
                   SELECT *,
                          lag(drop_topos) OVER w AS prev_drop,
                          lag(drop_lat)   OVER w AS prev_drop_lat,
                          lag(drop_lon)   OVER w AS prev_drop_lon
                     FROM cyc
                   WINDOW w AS (PARTITION BY ytno ORDER BY dropped_at)
                 )
                 INSERT INTO learn_travel_sample
                   (ytno, dropped_at, leg_ord, origin, dest, travel_s, dist_m, hour,
                    origin_zone, dest_zone, dow, congestion,
                    origin_lat, origin_lon, dest_lat, dest_lon, trip_ts)
                 SELECT wp.ytno, wp.dropped_at, 0, wp.prev_drop, wp.pickup_topos,
                        extract(epoch FROM wp.empty_arrived_at - wp.empty_travel_start_at)::int,
                        CASE WHEN po.topos IS NOT NULL AND pd.topos IS NOT NULL THEN
                          sqrt( power((pd.lat - po.lat) * 111320.0, 2)
                              + power((pd.lon - po.lon) * 111320.0 * cos(radians((po.lat + pd.lat) / 2)), 2) )
                        END,
                        extract(hour FROM wp.empty_arrived_at)::int,
                        travel_zone(wp.prev_drop, wp.prev_drop_lat, wp.prev_drop_lon),
                        travel_zone(wp.pickup_topos, wp.pickup_lat, wp.pickup_lon),
                        extract(dow FROM wp.empty_arrived_at)::int,
                        (SELECT count(*) FROM tt_cycle_v2 cc
                          WHERE cc.opened_at <= wp.empty_arrived_at
                            AND cc.dropped_at >= wp.empty_arrived_at)::int,
                        wp.prev_drop_lat, wp.prev_drop_lon, wp.pickup_lat, wp.pickup_lon,
                        wp.empty_travel_start_at + (wp.empty_arrived_at - wp.empty_travel_start_at) * 0.5
                   FROM wp
                   LEFT JOIN learn_topos_point po ON po.topos = wp.prev_drop
                   LEFT JOIN learn_topos_point pd ON pd.topos = wp.pickup_topos
                  WHERE wp.prev_drop IS NOT NULL AND wp.pickup_topos IS NOT NULL
                    AND wp.prev_drop <> wp.pickup_topos
                    AND wp.empty_travel_start_at IS NOT NULL AND wp.empty_arrived_at IS NOT NULL
                    AND wp.empty_arrived_at > wp.empty_travel_start_at
                    AND extract(epoch FROM wp.empty_arrived_at - wp.empty_travel_start_at) BETWEEN 10 AND 7200
                 ON CONFLICT (ytno, dropped_at, leg_ord) DO NOTHING",
            )
            .execute(&pool)
            .await;
            // per-trip corridor density (4 grid sizes) for samples missing it: 5 points along the
            // O→D line, each point's cell density from the zone_density snapshot in the trip's
            // minute, averaged → density_{50..200}. Only within zone_density coverage; bounded.
            let _ = sqlx::query(
                "WITH todo AS (
                   SELECT ytno, dropped_at, leg_ord, origin_lat, origin_lon, dest_lat, dest_lon, trip_ts
                     FROM learn_travel_sample
                    WHERE density_150 IS NULL AND trip_ts IS NOT NULL
                      AND origin_lat IS NOT NULL AND dest_lat IS NOT NULL
                      AND trip_ts >= (SELECT min(ts) FROM zone_density)
                    LIMIT 5000
                 ),
                 cells AS (
                   SELECT t.ytno, t.dropped_at, t.leg_ord, t.trip_ts, g.m AS grid_m,
                          round((t.origin_lat + f*(t.dest_lat - t.origin_lat)) / (g.m/111320.0))::int AS cx,
                          round((t.origin_lon + f*(t.dest_lon - t.origin_lon)) / (g.m/111320.0))::int AS cy
                     FROM todo t,
                          unnest(ARRAY[0, 0.25, 0.5, 0.75, 1.0]::float8[]) AS f,
                          unnest(ARRAY[50, 100, 150, 200]) AS g(m)
                 ),
                 dens AS (
                   SELECT c.ytno, c.dropped_at, c.leg_ord, c.grid_m, avg(COALESCE(zd.n, 0))::real AS d
                     FROM cells c
                     LEFT JOIN zone_density zd
                       ON zd.grid_m = c.grid_m AND zd.cx = c.cx AND zd.cy = c.cy
                      AND zd.ts >= date_trunc('minute', c.trip_ts)
                      AND zd.ts <  date_trunc('minute', c.trip_ts) + interval '1 minute'
                    GROUP BY c.ytno, c.dropped_at, c.leg_ord, c.grid_m
                 ),
                 w AS (
                   SELECT ytno, dropped_at, leg_ord,
                          max(d) FILTER (WHERE grid_m = 50)  AS d50,
                          max(d) FILTER (WHERE grid_m = 100) AS d100,
                          max(d) FILTER (WHERE grid_m = 150) AS d150,
                          max(d) FILTER (WHERE grid_m = 200) AS d200
                     FROM dens GROUP BY ytno, dropped_at, leg_ord
                 )
                 UPDATE learn_travel_sample s
                    SET density_50 = w.d50, density_100 = w.d100, density_150 = w.d150, density_200 = w.d200
                   FROM w
                  WHERE s.ytno = w.ytno AND s.dropped_at = w.dropped_at AND s.leg_ord = w.leg_ord",
            )
            .execute(&pool)
            .await;
            crate::db::prune(&pool, "learn_travel_sample", "DELETE FROM learn_travel_sample WHERE captured_at < now() - interval '30 days'").await;
            // [dropped mig 0112] the 225m realized zone summary (learn_travel_zone225) and its lookup
            // function used to be refreshed here every 5 minutes at 2.3s a go — with no reader
            // anywhere: no code, no view, and the one DB function that read it had no callers either.
            // Dispatch cost comes from the road-network route curve (roadgraph::RouteCost) since
            // mig 0082. Refreshing it cost ~11 minutes of DB work a day to keep 68MB nobody looked at.
            // [pure-driving OD pipeline removed] drive_sample / zone225_pure / topos_sample /
            // topos_pure dropped in mig 0073. ⚠ The line that used to sit here said the dispatch cost
            // "now uses REALIZED learn_travel_zone225" — that stopped being true at mig 0082 and stayed
            // in the file for a month, so every reader had to go and check whether the grid still fed
            // the matcher. It does not: the cost is the road-network route curve (roadgraph::RouteCost).
            let _ = sqlx::query(
                "INSERT INTO learn_travel_metric
                   (captured_at, samples, od_pairs, confident_pairs, median_speed_kmh,
                    zone_pairs, confident_zone_pairs, quay_zoned_samples)
                 SELECT now(), count(*), count(DISTINCT (origin, dest)),
                        (SELECT count(*) FROM (SELECT 1 FROM learn_travel_sample GROUP BY origin, dest HAVING count(*) >= 10) q),
                        percentile_cont(0.5) WITHIN GROUP (
                          ORDER BY (dist_m / 1000.0) / nullif(travel_s / 3600.0, 0)
                        ) FILTER (WHERE dist_m IS NOT NULL AND travel_s > 0),
                        count(DISTINCT (origin_zone, dest_zone)) FILTER (WHERE origin_zone IS NOT NULL AND dest_zone IS NOT NULL),
                        (SELECT count(*) FROM (SELECT 1 FROM learn_travel_sample WHERE origin_zone IS NOT NULL AND dest_zone IS NOT NULL GROUP BY origin_zone, dest_zone HAVING count(*) >= 10) q),
                        count(*) FILTER (WHERE origin_zone LIKE 'Q%' OR dest_zone LIKE 'Q%')
                   FROM learn_travel_sample
                  HAVING count(*) > 0
                 ON CONFLICT (captured_at) DO NOTHING",
            )
            .execute(&pool)
            .await;
            crate::db::prune(&pool, "learn_travel_metric", "DELETE FROM learn_travel_metric WHERE captured_at < now() - interval '30 days'").await;
        }
    });
}

// [spawn_leg_decomp retired — mig 0081] The empty-leg drive/stop GPS decomposition (learn_leg_decomp)
// was removed. The Stage-2 cost = 순수주행 구간시간 now comes from learn_travel_sample empty trips
// (mig 0080), and its matview refresh moved into spawn_travel_aggregator above.

/// Every ~30s, refresh the authoritative per-truck assignment cache from the work pool
/// (`live_assigned_tt` = any active job, all types; enriched with the latest `live_workpool`
/// row per truck for DS/LD job metadata). A truck present here is "assigned" even when its
/// GPS shows it empty+stationary — that's the signal the live idle classifier uses to mark it
/// `staging` instead of `idle`, and that the cycle machine uses for job metadata.
pub fn spawn_assignment_refresh(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(30));
        loop {
            ticker.tick().await;
            let rows = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<bool>)>(
                "SELECT DISTINCT ON (a.ytno) a.ytno, w.jobtype, w.vessel, w.voyage, w.contno, w.qc, w.twintandem, w.rtg_active
                   FROM live_assigned_tt a
                   LEFT JOIN LATERAL (
                       SELECT jobtype, vessel, voyage, contno, qc, twintandem, (actv_ts IS NOT NULL) AS rtg_active
                         FROM live_workpool w
                        WHERE w.ytno = a.ytno
                        ORDER BY w.as_of_ts DESC, w.id DESC
                        LIMIT 1
                   ) w ON true
                  WHERE a.as_of_ts > now() - interval '5 minutes'
                  ORDER BY a.ytno, a.as_of_ts DESC",
            )
            .fetch_all(&pool)
            .await;
            match rows {
                Ok(rows) => {
                    let mut next: HashMap<String, AssignedJob> = HashMap::with_capacity(rows.len());
                    for (ytno, jobtype, vessel, voyage, contno, qc, twintandem, rtg_active) in rows {
                        next.insert(ytno, AssignedJob { jobtype, vessel, voyage, contno, qc, twintandem, rtg_active: rtg_active.unwrap_or(false) });
                    }
                    *lm.assigned_pool.write().await = next;
                }
                Err(e) => tracing::warn!(error = %e, "assignment refresh query failed"),
            }
        }
    });
}

/// Every ~30s, drain completed cycles from the in-memory buffer into `tt_cycle_log`. Idempotent
/// (`ON CONFLICT (ytno, dropped_at) DO NOTHING`) so a restart can't double-write. Mirrors
/// `spawn_util_sampler`.
pub fn spawn_cycle_flusher(lm: Arc<LiveMap>, pool: PgPool) {
    let to_ts = |ms: i64| (ms > 0).then(|| DateTime::from_timestamp_millis(ms)).flatten();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(30));
        loop {
            ticker.tick().await;
            let batch: Vec<CompletedCycle> = {
                let mut buf = lm.cycle_log.lock().await;
                buf.drain(..).collect()
            };
            if batch.is_empty() {
                continue;
            }
            let (bd, sh) = tt_core::shift::current(tt_core::shift::terminal_now().naive_local());
            let mut written = 0u32;
            for c in &batch {
                let dropped = match to_ts(c.dropped_at_ms) { Some(t) => t, None => continue };
                let dur_s = |from: i64| (from > 0).then(|| ((c.dropped_at_ms - from) / 1000) as i32);
                let r = sqlx::query(
                    "INSERT INTO tt_cycle_log
                       (ytno, business_date, shift, jobtype, vessel, voyage, container, qc, twintandem,
                        assigned_at, pickup_arrived_at, pickup_left_at, pickup_at, arrived_at, dropped_at,
                        idle_before_s, empty_leg_s, empty_leg_m, laden_leg_s, laden_leg_m, cycle_s,
                        movement_ok, container_to_container,
                        pickup_arrived_crane_at, arrived_crane_at, crane_arr_method)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,true,$22,$23,$24,$25)
                     ON CONFLICT (ytno, dropped_at) DO NOTHING",
                )
                .bind(&c.ytno)
                .bind(bd)
                .bind(sh.label())
                .bind(&c.jobtype)
                .bind(&c.vessel)
                .bind(&c.voyage)
                .bind(&c.container)
                .bind(&c.qc)
                .bind(&c.twintandem)
                .bind(to_ts(c.assigned_at_ms))
                .bind(to_ts(c.pickup_arrived_at_ms))
                .bind(to_ts(c.pickup_left_at_ms))
                .bind(to_ts(c.pickup_at_ms))
                .bind(to_ts(c.arrived_at_ms))
                .bind(dropped)
                .bind((c.idle_before_ms / 1000) as i32)
                .bind((c.empty_leg_ms / 1000) as i32)
                .bind(c.empty_leg_m)
                .bind((c.laden_leg_ms / 1000) as i32)
                .bind(c.laden_leg_m)
                .bind(dur_s(c.assigned_at_ms).unwrap_or((c.laden_leg_ms / 1000) as i32))
                .bind(c.container_to_container)
                .bind(to_ts(c.pickup_arrived_crane_ms))
                .bind(to_ts(c.arrived_crane_ms))
                .bind(c.crane_arr_method)
                .execute(&pool)
                .await;
                match r {
                    Ok(_) => written += 1,
                    Err(e) => tracing::warn!(error = %e, ytno = %c.ytno, "tt_cycle_log insert failed"),
                }
            }
            tracing::debug!(written, batch = batch.len(), "flushed TT cycles");

            // ── v2 SHADOW rows → tt_cycle_v2 (crane-side PLC pairing happens here, where
            // the full edge history is available without touching the ingest hot path) ──
            let batch2: Vec<CompletedV2> = {
                let mut b = lm.cycle_v2.lock().await;
                b.drain(..).collect()
            };
            if batch2.is_empty() {
                continue;
            }
            for c in &batch2 {
                let dropped = match to_ts(c.dropped_ms) { Some(t) => t, None => continue };
                // pickup side = the leading run of legs of the jobtype's pickup kind
                // (DS picks at the crane, LD/MI/MO at a block); drop = the first leg after it.
                let pickup_is_crane = matches!(c.jobtype.as_deref(), Some("DS"));
                let mut split = c.legs.iter().position(|l| l.crane != pickup_is_crane);
                if split.is_none() && c.legs.len() >= 2 {
                    split = Some(c.legs.len() - 1); // block→block jobs: last leg is the drop
                }
                let split = split.unwrap_or(c.legs.len());
                let (pickup_legs, rest) = c.legs.split_at(split);
                let drop_leg = rest.first();

                let p_arr = pickup_legs.iter().find(|l| l.arrived_ms > 0);
                let mut p_left = pickup_legs.last().map(|l| l.left_ms).unwrap_or(0);
                if p_left == 0 {
                    p_left = drop_leg.map(|d| d.assigned_ms).unwrap_or(0); // the flip bounds the pickup
                }
                let legs_json = serde_json::json!(c.legs.iter().map(|l| serde_json::json!({
                    "target": l.target, "crane": l.crane, "assigned": l.assigned_ms,
                    "arrived": l.arrived_ms, "arr_src": l.arr_src, "left": l.left_ms,
                    "lat": (l.arrived_lat != 0.0).then_some(l.arrived_lat),
                    "lon": (l.arrived_lon != 0.0).then_some(l.arrived_lon),
                })).collect::<Vec<_>>());
                let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
                // v2.4: backfill a block arrival the leg model missed (or correct a coarse
                // pre_positioned approximation) from v1's continuous-tracker arrival for this
                // same (ytno, dropped_at). Keeps v2 capture ≥ v1 without touching the leg model.
                // Bounded by the drop instant; a final clamp preserves pickup ≤ drop (G4).
                let mut p_arr_ms = p_arr.map(|l| l.arrived_ms).unwrap_or(0);
                let mut p_arr_src = p_arr.map(|l| l.arr_src).unwrap_or("");
                let mut d_arr_ms = drop_leg.map(|d| d.arrived_ms).unwrap_or(0);
                let mut d_arr_src = drop_leg.map(|d| d.arr_src).unwrap_or("");
                if c.v1_drop_arrived_ms > 0 && c.v1_drop_arrived_ms <= c.dropped_ms
                    && (d_arr_ms == 0 || d_arr_src == "pre_positioned")
                {
                    d_arr_ms = c.v1_drop_arrived_ms;
                    d_arr_src = "v1";
                }
                if c.v1_pickup_arrived_ms > 0 && c.v1_pickup_arrived_ms <= c.dropped_ms
                    && (p_arr_ms == 0 || p_arr_src == "pre_positioned" || p_arr_src == "container1")
                {
                    p_arr_ms = c.v1_pickup_arrived_ms;
                    p_arr_src = "v1";
                }
                // enforce the monotonic chain empty_arrived ≤ pickup_left ≤ laden_arrived,
                // dropping the stale value the backfill exposed. Arrival-vs-arrival first
                // (keep the leg-derived one), then the leg-derived departure against both —
                // a split (mode2) cycle's pickup_left can predate the accurate v1 arrival, so
                // NULL the unreliable departure rather than the arrival (preserves capture).
                if p_arr_ms > 0 && d_arr_ms > 0 && p_arr_ms > d_arr_ms {
                    if p_arr_src == "v1" {
                        p_arr_ms = 0;
                        p_arr_src = "";
                    } else if d_arr_src == "v1" {
                        d_arr_ms = 0;
                        d_arr_src = "";
                    }
                }
                if p_left > 0 && p_arr_ms > 0 && p_arr_ms > p_left {
                    p_left = 0;
                }
                if p_left > 0 && d_arr_ms > 0 && p_left > d_arr_ms {
                    p_left = 0;
                }
                // 공차이동시작은 픽업 도착보다 앞서야 한다. 백필/소급(arr_dtime·v1) 도착이 더 이른
                // 분할·이월 사이클에선 첫 움직임이 이 트립 것이 아니므로 NULL 처리.
                let mut ets_ms = c.empty_travel_start_ms;
                if ets_ms > 0 && p_arr_ms > 0 && ets_ms > p_arr_ms {
                    ets_ms = 0;
                }
                let r = sqlx::query(
                    "INSERT INTO tt_cycle_v2
                       (ytno, dropped_at, opened_at, jobtype,
                        empty_travel_start_at, empty_arrived_at, pickup_left_at,
                        laden_arrived_at, arr_src_pickup, arr_src_drop, legs)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
                     ON CONFLICT (ytno, dropped_at) DO NOTHING",
                )
                .bind(&c.ytno)
                .bind(dropped)
                .bind(to_ts(c.opened_ms))
                .bind(&c.jobtype)
                .bind(to_ts(ets_ms))
                .bind(to_ts(p_arr_ms))
                .bind(to_ts(p_left))
                .bind(to_ts(d_arr_ms))
                .bind(opt(p_arr_src))
                .bind(opt(d_arr_src))
                .bind(legs_json)
                .execute(&pool)
                .await;
                if let Err(e) = r {
                    tracing::warn!(error = %e, ytno = %c.ytno, "tt_cycle_v2 insert failed");
                }
            }
        }
    });
}

pub fn spawn(lm: Arc<LiveMap>) {
    // pruner: drop fixes older than LOST_AFTER_S so the maps can't grow unbounded.
    {
        let lm = lm.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let cutoff = Utc::now().timestamp_millis() - LOST_AFTER_S * 1000;
                lm.devices.write().await.retain(|_, p| p.last_seen_ms >= cutoff);
                lm.plc.write().await.retain(|_, c| c.last_seen_ms >= cutoff);
            }
        });
    }
    let url = std::env::var("LIVEMAP_WS_URL").unwrap_or_else(|_| "ws://127.0.0.1:9986".into());
    let identify = std::env::var("LIVEMAP_IDENTIFY").unwrap_or_else(|_| "clt_digitaltwin1".into());
    let username = std::env::var("LIVEMAP_USERNAME").unwrap_or_else(|_| "digitaltwin".into());
    let user = std::env::var("LIVEMAP_USER").unwrap_or_else(|_| "clt_digitaltwin1".into());

    // GPS zone (wpt_gps) — primary feed.
    {
        let (lm, url, identify, username, user) =
            (lm.clone(), url.clone(), identify.clone(), username.clone(), user.clone());
        tokio::spawn(async move {
            let mut backoff = 2u64;
            loop {
                match serve_gps(&lm, &url, &identify, &username, &user).await {
                    Ok(()) => backoff = 2,
                    Err(e) => {
                        lm.connected.store(false, Ordering::Relaxed);
                        *lm.last_error.write().await = Some(format!("{e}"));
                        tracing::warn!(error = %e, backoff_s = backoff, "livemap gps ws disconnected");
                    }
                }
                lm.reconnects.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(30);
            }
        });
    }

    // ctab zone (crane PLC) — secondary feed, identify only (no checkin).
    tokio::spawn(async move {
        let mut backoff = 2u64;
        loop {
            match serve_ctab(&lm, &url, &identify).await {
                Ok(()) => backoff = 2,
                Err(e) => {
                    lm.plc_connected.store(false, Ordering::Relaxed);
                    tracing::warn!(error = %e, backoff_s = backoff, "livemap ctab ws disconnected");
                }
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    });
}

async fn serve_gps(
    lm: &Arc<LiveMap>,
    url: &str,
    identify: &str,
    username: &str,
    user: &str,
) -> anyhow::Result<()> {
    let (ws, _resp) = tokio_tungstenite::connect_async(url).await?;
    let (mut tx, mut rx) = ws.split();
    tracing::info!(%url, "livemap gps ws connected");

    // wpt_gps zone handshake: identify -> wait 2s -> checkin.
    let identify_msg = serde_json::json!({"command":{"identify": identify, "zone":"wpt_gps"}});
    tx.send(Message::Text(identify_msg.to_string())).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let checkin_msg = serde_json::json!({"checkin":{"username": username, "user": user}});
    tx.send(Message::Text(checkin_msg.to_string())).await?;

    lm.connected.store(true, Ordering::Relaxed);
    lm.connected_since_ms.store(Utc::now().timestamp_millis() as u64, Ordering::Relaxed);
    *lm.last_error.write().await = None;

    // The source never pongs our pings (reference disables them); detect a dead socket
    // by a receive timeout instead.
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(60), rx.next()).await?;
        let Some(msg) = msg else { break }; // stream ended
        match msg? {
            Message::Text(t) => ingest_text(lm, &t).await,
            Message::Binary(b) => {
                if let Ok(t) = String::from_utf8(b) {
                    ingest_text(lm, &t).await;
                }
            }
            Message::Ping(p) => {
                let _ = tx.send(Message::Pong(p)).await;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    lm.connected.store(false, Ordering::Relaxed);
    Ok(())
}

/// ctab zone — crane PLC. Handshake is identify ONLY (no checkin, per the reference).
async fn serve_ctab(lm: &Arc<LiveMap>, url: &str, identify: &str) -> anyhow::Result<()> {
    let (ws, _resp) = tokio_tungstenite::connect_async(url).await?;
    let (mut tx, mut rx) = ws.split();
    tracing::info!(%url, "livemap ctab ws connected");

    let identify_msg = serde_json::json!({"command":{"identify": identify, "zone":"ctab"}});
    tx.send(Message::Text(identify_msg.to_string())).await?;
    lm.plc_connected.store(true, Ordering::Relaxed);

    loop {
        let msg = tokio::time::timeout(Duration::from_secs(60), rx.next()).await?;
        let Some(msg) = msg else { break };
        match msg? {
            Message::Text(t) => ingest_ctab(lm, &t).await,
            Message::Binary(b) => {
                if let Ok(t) = String::from_utf8(b) {
                    ingest_ctab(lm, &t).await;
                }
            }
            Message::Ping(p) => {
                let _ = tx.send(Message::Pong(p)).await;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    lm.plc_connected.store(false, Ordering::Relaxed);
    Ok(())
}

/// Parse a ctab `plc_data` frame:
/// `{"data":{"id":"plc_C39_...","zone":"ctab","datas":"{\"plc_data\":{\"crane\":\"C39\",
///   \"load\":0,\"lock\":\"False\",\"land\":\"False\",\"hpos\":\"6.77\",\"tpos\":\"69.35\"}}"}}`.
/// Other ctab kinds (checkin / session_* / rps_*) are ignored.
async fn ingest_ctab(lm: &Arc<LiveMap>, text: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return };
    let Some(datas) = v.get("data").and_then(|d| d.get("datas")).and_then(|x| x.as_str()) else {
        return;
    };
    let Ok(inner) = serde_json::from_str::<serde_json::Value>(datas) else { return };
    let Some(g) = inner.get("plc_data") else { return };
    let Some(crane) = g.get("crane").and_then(|x| x.as_str()).filter(|s| !s.is_empty()) else {
        return;
    };
    let now = Utc::now().timestamp_millis();
    let load = g.get("load").and_then(num);
    // hysteresis: laden ≥1.0t, empty <0.5t; otherwise keep prior state
    let mut map = lm.plc.write().await;
    let e = map.entry(crane.to_string()).or_default();
    let now_laden = match load {
        Some(t) if t >= PLC_LADEN_T => true,
        Some(t) if t < PLC_EMPTY_T => false,
        _ => e.laden,
    };
    if !e.laden && now_laden {
        // empty→laden = a pickup. Count it as a move unless it's a flicker within one
        // cycle of the last counted move.
        let since_last = if e.last_move_ms == 0 { i64::MAX } else { now - e.last_move_ms };
        if since_last >= MIN_MOVE_GAP_MS {
            e.moves.push_back(now);
            e.last_move_ms = now;
            while e.moves.front().is_some_and(|&f| now - f > MOVE_WINDOW_MS) {
                e.moves.pop_front();
            }
        }
    }
    e.laden = now_laden;
    e.load_t = load;
    e.lock = g.get("lock").and_then(parse_bool);
    e.land = g.get("land").and_then(parse_bool);
    e.hpos = g.get("hpos").and_then(num);
    e.tpos = g.get("tpos").and_then(num);
    e.last_seen_ms = now;
    drop(map);
    lm.plc_messages.fetch_add(1, Ordering::Relaxed);
}

/// "True"/"False" (any case) or 1/0 → bool.
fn parse_bool(v: &serde_json::Value) -> Option<bool> {
    if let Some(b) = v.as_bool() {
        return Some(b);
    }
    if let Some(n) = v.as_f64() {
        return Some(n != 0.0);
    }
    match v.as_str()?.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

/// Parse one frame. GPS frames:
/// `{"data":{"id":"TT1074","zone":"wpt_gps","datas":"<stringified gps_update json>"}}`.
/// `{"disconnect":...}` churn frames are ignored (positions age out by `last_seen`).
async fn ingest_text(lm: &Arc<LiveMap>, text: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return };
    let Some(data) = v.get("data") else { return };
    let Some(id) = data.get("id").and_then(|x| x.as_str()) else { return };
    let Some(datas) = data.get("datas").and_then(|x| x.as_str()) else { return };
    let Ok(inner) = serde_json::from_str::<serde_json::Value>(datas) else { return };
    let Some(g) = inner.get("gps_update") else { return };

    let (Some(lat), Some(lon)) = (g.get("lat").and_then(num), g.get("lon").and_then(num)) else {
        return;
    };
    if lat == 0.0 && lon == 0.0 {
        return; // no fix
    }
    let cls: String = id.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    // UPSTREAM COORDINATE GATE — see TT_MAX_R_M. Runs before ANY consumer sees the fix, but it
    // only DROPS for populations we have actually measured (TT) plus non-finite values.
    match gate_fix(&cls, lat, lon) {
        FixGate::Keep => {}
        FixGate::KeepButFar(d) => {
            let n = lm.far_unmeasured.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_power_of_two() {
                tracing::warn!(
                    id, cls = %cls, lat, lon, km = (d / 1000.0).round(), total = n,
                    "fix far from the terminal from a class with no measured footprint — KEPT"
                );
            }
        }
        FixGate::Drop(why) => {
            let n = lm.rejected_far.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_power_of_two() {
                tracing::warn!(id, cls = %cls, lat, lon, why, total = n, "GPS fix dropped by the coordinate gate");
            }
            return;
        }
    }
    let speed = g
        .get("speed")
        .and_then(|x| x.as_str())
        .map(|s| s.trim_end_matches(|c: char| c.is_alphabetic()).trim())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    let engine = match g.get("engine_on").and_then(|x| x.as_str()) {
        Some(s) if s.to_ascii_uppercase().contains("ON") => 1,
        _ => 0,
    };
    let now = Utc::now().timestamp_millis();

    let mut pos = Pos {
        cls,
        lat,
        lon,
        speed,
        engine,
        last_seen_ms: now,
        jobtype: opt_str(g, "jobtype"),
        vslname: opt_str(g, "vslname"),
        container1: opt_str(g, "container1"),
        container2: opt_str(g, "container2"),
        cur_loc: opt_str(g, "cur_loc"),
        topos1: opt_str(g, "topos1"),
        arrival: opt_str(g, "arrival"),
        fuel: g.get("fuel_level").and_then(num),
        accuracy: g.get("accuracy").and_then(num),
        userid: opt_str(g, "userid").map(|s| clean_driver(&s)),
        batt: opt_str(g, "batt"),
        nett: opt_str(g, "nett"),
        dtime: opt_str(g, "dtime"),
        distance: g.get("distance").and_then(num),
        ..Default::default()
    };
    // v2 SHADOW: the feed's own arrival timestamp ("HH:MM:SS", persists while arrived) —
    // gives the exact arrival time even when the ARRIVED rising edge was missed.
    if let Some(ad) = opt_str(g, "arr_dtime") {
        pos.arr_dtime_ms = parse_arr_dtime(&ad, now).unwrap_or(0);
    }

    // learn block/bay AND crane positions from TTs observed ARRIVED at a topos — feeds the
    // empty-TT "remaining distance to pickup" used for swap-worthiness (crane fallback when a
    // crane isn't broadcasting GPS).
    if pos.cls == "TT" && pos.arrival.as_deref() == Some("ARRIVED") {
        if let Some(t) = pos.topos1.as_deref() {
            if !t.is_empty() {
                if is_crane_code(t) {
                    // QC: ungated fallback centroid (a crane legitimately travels the quay) + the
                    // quay work-line + per-crane recency work-point (swing-free handover point).
                    lm.centroids.write().await.entry(t.to_string()).or_default().push(lat, lon);
                    let (x, y) = to_local(lat, lon);
                    lm.quay_line.write().await.push(x, y);
                    lm.crane_wp.write().await.entry(t.to_string()).or_default().push(x, y, now);
                } else {
                    // block/bay: OUTLIER-GATED push — a stale-topos1 sample landing in another
                    // block is dropped so it can't drag the mean / blow up the spread.
                    let (full, pre) = (t.to_string(), block_prefix(t).to_string());
                    let mut c = lm.centroids.write().await;
                    c.entry(full).or_default().push_gated(lat, lon, CENTROID_GATE_M);
                    if pre != t {
                        c.entry(pre).or_default().push_gated(lat, lon, CENTROID_GATE_M);
                    }
                }
            }
        }
        // learn wharf/quay segment positions from the truck's cur_loc (e.g. WHARF_14_C) — same
        // running-centroid mechanism, separate keys; ARRIVED gate filters in-transit mislabels.
        // Surfaced via /api/livemap/wharf + the live-map overlay (persisted in learn_topos_point).
        if let Some(cl) = pos.cur_loc.as_deref() {
            if cl.starts_with("WHARF") && !cl.is_empty() {
                lm.centroids.write().await.entry(cl.to_string()).or_default().push_gated(lat, lon, CENTROID_GATE_M);
            }
        }
    }
    // learn driving lanes (③): a moving TT's grid cell + bearing(prev→cur) + speed. The
    // prev read-lock is released before the devices write lock below (no two locks held).
    if pos.cls == "TT" && speed >= LANE_MIN_SPEED_KMH {
        let prev_ll = lm.devices.read().await.get(id).map(|p| (p.lat, p.lon));
        if let Some(prev) = prev_ll {
            let d = dist_m(prev, (lat, lon));
            if d >= LANE_MIN_M && d <= LANE_MAX_M {
                let cell = ((lat / LANE_CELL_DEG).round() as i32, (lon / LANE_CELL_DEG).round() as i32);
                let br = bearing_deg(prev, (lat, lon));
                lm.lanes.write().await.entry(cell).or_default().push(br, speed);
            }
        }
    }
    // TT cycle: carry per-truck tracking across fixes; a delivery = container1 changing
    // away from a non-empty value (→empty OR →another container). Record a fleet delivery
    // (for throughput λ) — always — and, between two of a truck's deliveries, a capped
    // cycle-interval sample (for the median). Fleet-delivery and cycle-sample are separate
    // (the first delivery has no predecessor, so it feeds λ but not the median).
    let mut fleet_drop = false;
    let mut cycle_sample_s: Option<i64> = None;
    let mut artifact = false;
    let mut artifact_near = false;
    // authoritative assignment (any job type) + its work-pool metadata for this truck
    let aj = lm.assigned_pool.read().await.get(id).cloned();
    let assigned_now = aj.is_some();
    // SHADOW crane-arrival: this message's destination crane PLC (last pickup / freshness),
    // read BEFORE the devices write lock so we never hold two locks. Keyed by topos1.
    let crane_plc: Option<(i64, i64)> = match pos.topos1.as_deref().filter(|t| is_crane_code(t)) {
        Some(t) => lm.plc.read().await.get(t).map(|c| (c.last_move_ms, c.last_seen_ms)),
        None => None,
    };
    let mut completed: Option<CompletedCycle> = None;
    let mut completed_v2: Option<CompletedV2> = None;
    {
        let mut devmap = lm.devices.write().await;
        let prev_c1 = devmap.get(id).and_then(|p| p.container1.clone());
        let prev_c2 = devmap.get(id).and_then(|p| p.container2.clone());
        let prev_arrived = devmap.get(id).is_some_and(|p| p.arrival.as_deref() == Some("ARRIVED"));
        let prev_topos = devmap.get(id).and_then(|p| p.latched_topos.clone());
        if let Some(prev) = devmap.get(id) {
            pos.carry_since_ms = prev.carry_since_ms;
            pos.last_drop_ms = prev.last_drop_ms;
            // accumulate path length driven since the carry began (jitter-guarded). This is
            // the evidence used to tell a real delivery (truck drove the box) from a TOS
            // re-assignment artifact (container1 rewritten while the truck sits still).
            pos.carry_trip_m = prev.carry_trip_m;
            // carry the cycle-machine state forward
            pos.empty_since_ms = prev.empty_since_ms;
            pos.empty_trip_m = prev.empty_trip_m;
            pos.empty_arrived_ms = prev.empty_arrived_ms;
            pos.cycle_open = prev.cycle_open.clone();
            // latch job fields: keep the previous latched value when this fix omits the field,
            // so an intermittent feed doesn't drop the truck's job/container mid-cycle.
            pos.latched_container = pos.container1.clone().or_else(|| prev.latched_container.clone());
            pos.latched_jobtype = pos.jobtype.clone().or_else(|| prev.latched_jobtype.clone());
            pos.latched_vessel = pos.vslname.clone().or_else(|| prev.latched_vessel.clone());
            pos.latched_topos = pos.topos1.clone().or_else(|| prev.latched_topos.clone());
            // v2 SHADOW carries
            pos.latched_topos2 = opt_str(g, "topos2").or_else(|| prev.latched_topos2.clone());
            if pos.arr_dtime_ms == 0 { pos.arr_dtime_ms = prev.arr_dtime_ms; }
            pos.v2 = prev.v2.clone();
            if pos.cls == "TT" && prev.lat != 0.0 && pos.lat != 0.0 {
                let step = dist_m((prev.lat, prev.lon), (pos.lat, pos.lon));
                if step.is_finite() && step <= MAX_FIX_STEP_M {
                    pos.carry_trip_m += step;
                    // accumulate the empty-leg path while not carrying (assignment→pickup drive)
                    if pos.container1.as_deref().unwrap_or("").is_empty() {
                        pos.empty_trip_m += step;
                    }
                }
            }
        } else {
            // first sight of this device: seed the latches from this fix
            pos.latched_container = pos.container1.clone();
            pos.latched_jobtype = pos.jobtype.clone();
            pos.latched_vessel = pos.vslname.clone();
            pos.latched_topos = pos.topos1.clone();
            pos.latched_topos2 = opt_str(g, "topos2");
        }
        pos.assigned = assigned_now;
        if pos.cls == "TT" {
            // owned copies so the cycle helpers can take `&pos` without borrow conflicts
            let new_c1 = pos.container1.clone().unwrap_or_default();
            let old_c1 = prev_c1.clone().unwrap_or_default();
            // SLOT SWAP guard (twin/tandem carry): the feed sometimes just reorders the two
            // box numbers between container1/container2 without any physical drop or pickup
            // (verified live: ~2 swaps / 25s among ~52 twin carriers). A bare container1 edge
            // would then look like a delivery. Detect it as "the non-empty {c1,c2} set is
            // unchanged" and treat it as a no-op: no drop, and crucially no carry-state reset
            // (otherwise every swap restarts a twin's laden trip-distance/-time measurement).
            let slot_swap = new_c1 != old_c1 && !new_c1.is_empty() && {
                let new_c2 = pos.container2.clone().unwrap_or_default();
                let old_c2 = prev_c2.clone().unwrap_or_default();
                let mut a = [old_c1.as_str(), old_c2.as_str()]; a.sort_unstable();
                let mut b = [new_c1.as_str(), new_c2.as_str()]; b.sort_unstable();
                a == b
            };
            // ARRIVED handling for the open cycle. container1 is ASSIGNMENT-driven (the next
            // box is pre-assigned at the previous drop — verified live: LD trucks sit ARRIVED
            // at their pickup block with container1 already set), so the physical pickup is
            // NOT a container1 edge. Recover it by classifying each ARRIVED rising edge by
            // WHICH side it hit: LD loads at a block & unloads at the crane, DS the reverse,
            // MI/MO are block→block (first arrival = pickup, a later one = drop). The truck
            // speeding up again after the pickup arrival = pickup departure (laden start).
            let arrived_now = pos.arrival.as_deref() == Some("ARRIVED");
            let topos_now = pos.topos1.clone().or_else(|| pos.latched_topos.clone()).unwrap_or_default();
            if let Some(oc) = pos.cycle_open.as_mut() {
                if arrived_now && !prev_arrived {
                    let at_crane = is_crane_code(&topos_now);
                    let drop_side = match pos.latched_jobtype.as_deref().unwrap_or("") {
                        "LD" => at_crane,
                        "DS" => !at_crane,
                        "MI" | "MO" | "LC" => oc.pickup_arrived_at_ms != 0,
                        _ => at_crane,
                    };
                    if drop_side {
                        if oc.arrived_at_ms == 0 { oc.arrived_at_ms = now; }
                    } else if oc.pickup_arrived_at_ms == 0 && oc.arrived_at_ms == 0 {
                        oc.pickup_arrived_at_ms = now;
                    }
                } else if !arrived_now
                    && oc.pickup_arrived_at_ms != 0
                    && oc.pickup_left_at_ms == 0
                    && pos.speed >= IDLE_SPEED_KMH
                {
                    oc.pickup_left_at_ms = now; // under way again → laden travel begins
                }
            }
            // pickup-side ARRIVED on a true empty leg (before the next cycle opens)
            if arrived_now && !prev_arrived && new_c1.is_empty() && pos.empty_arrived_ms == 0 {
                pos.empty_arrived_ms = now;
            }
            // ── SHADOW crane-arrival (observational; does NOT touch the live phases above) ──
            // When the truck's assigned destination is a quay crane, the ARRIVED flag is
            // unreliable, so estimate the crane arrival from GPS proximity to that crane OR the
            // crane PLC actively handling while the truck is stopped. Record the FIRST such
            // detection per open cycle into the shadow fields, routed by job side (LD crane =
            // drop, DS crane = pickup). For validation against the live columns only.
            if is_crane_code(&topos_now) {
                let near_crane = devmap.get(&topos_now).is_some_and(|cr| {
                    cr.lat != 0.0
                        && (now - cr.last_seen_ms) / 1000 <= STALE_AFTER_S
                        && dist_m((pos.lat, pos.lon), (cr.lat, cr.lon)) <= CRANE_ARRIVE_M
                });
                let plc_active = crane_plc.is_some_and(|(last_move, last_seen)| {
                    (now - last_seen) / 1000 <= STALE_AFTER_S && last_move != 0 && now - last_move <= CRANE_PLC_ACTIVE_MS
                });
                let method = if arrived_now {
                    Some("arrived")
                } else if near_crane {
                    Some("gps")
                } else if plc_active && pos.speed < IDLE_SPEED_KMH {
                    Some("plc")
                } else {
                    None
                };
                if let (Some(m), Some(oc)) = (method, pos.cycle_open.as_mut()) {
                    let drop_side = match pos.latched_jobtype.as_deref().unwrap_or("") {
                        "LD" => true,
                        "DS" => false,
                        _ => oc.pickup_arrived_crane_ms != 0, // block→block jobs: 2nd crane hit = drop
                    };
                    if drop_side {
                        if oc.arrived_crane_ms == 0 {
                            oc.arrived_crane_ms = now;
                            oc.crane_arr_method = Some(m);
                        }
                    } else if oc.pickup_arrived_crane_ms == 0 {
                        oc.pickup_arrived_crane_ms = now;
                        oc.crane_arr_method = Some(m);
                    }
                }
            }
            if new_c1 != old_c1 && !slot_swap {
                let was_loaded = !old_c1.is_empty();
                if was_loaded {
                    // a delivery requires the box was carried ≥30s AND the truck actually
                    // drove it ≥150m. Carried-but-stationary = TOS re-assignment, not a
                    // delivery: rejected here on the movement signature (not on duration).
                    let held = if pos.carry_since_ms > 0 { now - pos.carry_since_ms } else { i64::MAX };
                    if held >= MIN_LOADED_MS {
                        if pos.carry_trip_m >= MIN_CARRY_TRIP_M {
                            fleet_drop = true;
                            if pos.last_drop_ms != 0 {
                                let iv = now - pos.last_drop_ms;
                                if (MIN_CYCLE_S * 1000..=MAX_CYCLE_S * 1000).contains(&iv) {
                                    cycle_sample_s = Some(iv / 1000);
                                }
                            }
                            pos.last_drop_ms = now;
                            // finalize the cycle this drop completes (→ tt_cycle_log)
                            completed = Some(finalize_cycle(id, &pos, now, !new_c1.is_empty()));
                            pos.cycle_open = None;
                            pos.latched_container = None;
                        } else {
                            artifact = true; // changed container1 without moving it
                            artifact_near = pos.carry_trip_m >= NEAR_TRIP_M; // possible short haul
                        }
                    }
                }
                // new carry (or empty): reset the trip accumulator for the next box
                pos.carry_since_ms = if new_c1.is_empty() { 0 } else { now };
                pos.carry_trip_m = 0.0;
                if new_c1.is_empty() {
                    // entering an empty leg: start measuring the next assignment→pickup drive
                    pos.empty_since_ms = now;
                    pos.empty_trip_m = 0.0;
                    pos.empty_arrived_ms = 0;
                } else {
                    // pickup: open a fresh cycle. A container→container pickup (was_loaded) has
                    // no empty gap, so zero the empty leg first.
                    if was_loaded { pos.empty_since_ms = now; pos.empty_trip_m = 0.0; }
                    pos.latched_container = pos.container1.clone();
                    pos.cycle_open = Some(open_cycle(now, &pos, aj.as_ref()));
                    pos.empty_arrived_ms = 0; // consumed into the cycle; ready for the next leg
                }
            }
            // ── v2 SHADOW leg tracker (design doc) — writes only tt_cycle_v2; the v1
            // machine above is untouched. A leg = one topos1 target. Order matters:
            // (i) progress the in-flight leg, (ii) a validated drop (v1 `completed`)
            // closes the v2 cycle, (iii) a topos1 transition assigns the next leg.
            {
                let stopped = pos.speed < IDLE_SPEED_KMH;
                // 공차이동시작: first movement after the cycle opens while still on the first
                // (empty→pickup) leg and before reaching the pickup. opened→here gap = the
                // post-assignment wait. NULL for pre-positioned trucks (no empty drive observed).
                if pos.v2.opened_ms > 0
                    && pos.v2.empty_travel_start_ms == 0
                    && pos.speed >= IDLE_SPEED_KMH
                    && pos.v2.legs.is_empty()
                    && pos.v2.cur.as_ref().map_or(true, |l| l.arrived_ms == 0)
                {
                    pos.v2.empty_travel_start_ms = now;
                }
                // (i) arrival (arr_dtime > ARRIVED edge > cur_loc match > crane GPS), departure
                if let Some(leg) = pos.v2.cur.as_mut() {
                    // v2.3 (A): cur_loc=WHARF latches ~200s early in the wharf queue and
                    // would lock out the accurate arr_dtime/ARRIVED edge that fires when the
                    // truck actually reaches the crane. So keep re-evaluating while the only
                    // latch we have is the coarse cur_loc, and UPGRADE it to the precise source.
                    let coarse = leg.arr_src == "cur_loc";
                    if leg.arrived_ms == 0 || coarse {
                        let mut upgraded = false;
                        if pos.arr_dtime_ms > 0 && pos.arr_dtime_ms >= leg.assigned_ms {
                            leg.arrived_ms = pos.arr_dtime_ms;
                            leg.arr_src = "arr_dtime";
                            upgraded = coarse;
                        } else if arrived_now && !prev_arrived {
                            leg.arrived_ms = now;
                            leg.arr_src = "arrived";
                            upgraded = coarse;
                        } else if leg.arrived_ms == 0 && stopped {
                            let cl = pos.cur_loc.as_deref().unwrap_or("");
                            let at = if leg.crane {
                                cl.starts_with("WHARF")
                            } else {
                                !cl.is_empty() && block_prefix(cl) == block_prefix(&leg.target)
                            };
                            if at {
                                leg.arrived_ms = now;
                                leg.arr_src = "cur_loc";
                            } else if leg.crane {
                                let near = devmap.get(&leg.target).is_some_and(|cr| {
                                    cr.lat != 0.0
                                        && (now - cr.last_seen_ms) / 1000 <= STALE_AFTER_S
                                        && dist_m((pos.lat, pos.lon), (cr.lat, cr.lon)) <= 60.0
                                });
                                if near {
                                    leg.arrived_ms = now;
                                    leg.arr_src = "gps";
                                }
                            }
                        }
                        if upgraded {
                            // departure derived from the early coarse arrival now predates the
                            // corrected time — drop it so it re-derives.
                            if leg.left_ms != 0 && leg.left_ms <= leg.arrived_ms {
                                leg.left_ms = 0;
                            }
                        }
                        // capture/refresh the handover coordinate at arrival (refresh on a
                        // coarse→precise upgrade) for quay-zone gridding (QC id ≠ fixed location)
                        if leg.arrived_ms > 0 && (leg.arrived_lat == 0.0 || upgraded) {
                            leg.arrived_lat = pos.lat;
                            leg.arrived_lon = pos.lon;
                        }
                    }
                    if leg.arrived_ms > 0
                        && leg.left_ms == 0
                        && pos.speed >= IDLE_SPEED_KMH
                        && now - leg.arrived_ms > 5_000
                    {
                        leg.left_ms = now;
                    }
                }
                // (i-b) 픽업 보장: container1 empty→non-empty 는 가장 신뢰도 높은 픽업 신호다.
                // topos1이 픽업 타깃으로 전이하지 않아 픽업 레그가 아예 안 생기던 "드롭전용 1-레그"
                // 사이클을 메운다 — 없으면 합성, 미도착이면 도착 마킹. 픽업 종류는 jobtype으로
                // 결정(DS=크레인, 그 외=블록), 정확한 도착 시각은 flush의 v1 백필이 보정한다.
                if old_c1.is_empty() && !new_c1.is_empty() {
                    if pos.v2.opened_ms == 0 {
                        pos.v2.opened_ms = now;
                        pos.v2.jobtype = pos.latched_jobtype.clone();
                    }
                    if let Some(leg) = pos.v2.cur.as_mut() {
                        if leg.arrived_ms == 0 {
                            leg.arrived_ms = now;
                            leg.arr_src = "container1";
                        }
                    } else {
                        let crane = pos.v2.jobtype.as_deref()
                            .or(pos.latched_jobtype.as_deref()) == Some("DS");
                        let tgt = pos.latched_topos.clone().filter(|t| !t.is_empty())
                            .or_else(|| pos.cur_loc.clone()).unwrap_or_default();
                        if !tgt.is_empty() {
                            pos.v2.cur = Some(V2Leg {
                                crane,
                                target: tgt,
                                assigned_ms: pos.v2.opened_ms,
                                arrived_ms: now,
                                arr_src: "container1",
                                left_ms: 0,
                                arrived_lat: pos.lat, // arrived at construction (container1 edge)
                                arrived_lon: pos.lon,
                            });
                        }
                    }
                }
                // (ii) the validated drop edge closes the v2 cycle. TOS often PRE-assigns
                // the next job's target mid-cycle (verified: most cycles were missing their
                // pickup leg because it had attached to the closing cycle, or no transition
                // fired at reopen since the latch already held the next target). So: an
                // un-arrived trailing leg is the NEXT cycle's pickup → carry it over with
                // its true assignment time; otherwise seed the new cycle's first leg from
                // the currently latched target.
                if completed.is_some() {
                    // jobtype of the cycle being closed (snapshot at open; latch is the next job)
                    let close_jobtype = pos.v2.jobtype.clone().or_else(|| pos.latched_jobtype.clone());
                    let pickup_is_crane = close_jobtype.as_deref() == Some("DS");
                    let known_jt = matches!(close_jobtype.as_deref(), Some("DS") | Some("LD"));
                    let mut legs = std::mem::take(&mut pos.v2.legs);
                    let mut carry: Option<V2Leg> = None;
                    if let Some(mut cur) = pos.v2.cur.take() {
                        // v2.3 (C): the v1 drop edge can fire early on a block c2c, splitting one
                        // physical trip into a pickup-only cycle + a drop-only cycle. If this cycle
                        // never reached a drop-kind leg, the current (arrived) pickup leg belongs to
                        // the continuing trip → carry it forward instead of burying it here.
                        let reached_drop = legs.iter().chain(std::iter::once(&cur))
                            .any(|l| l.crane != pickup_is_crane);
                        if cur.arrived_ms == 0 && !legs.is_empty() {
                            carry = Some(cur); // pre-assigned next-pickup leg → next cycle
                        } else if known_jt && !reached_drop && cur.crane == pickup_is_crane {
                            carry = Some(cur); // premature close: pickup leg → next cycle
                        } else {
                            if cur.arrived_ms > 0 && cur.left_ms == 0 {
                                cur.left_ms = now;
                            }
                            legs.push(cur);
                        }
                    }
                    if pos.v2.opened_ms > 0 && !legs.is_empty() {
                        completed_v2 = Some(CompletedV2 {
                            ytno: id.to_string(),
                            dropped_ms: now,
                            opened_ms: pos.v2.opened_ms,
                            empty_travel_start_ms: pos.v2.empty_travel_start_ms,
                            jobtype: pos.v2.jobtype.clone().or_else(|| pos.latched_jobtype.clone()),
                            legs,
                            v1_pickup_arrived_ms: completed.as_ref().map(|c| c.pickup_arrived_at_ms).unwrap_or(0),
                            v1_drop_arrived_ms: completed.as_ref().map(|c| c.arrived_at_ms).unwrap_or(0),
                        });
                    }
                    pos.v2 = V2State::default();
                    if !new_c1.is_empty() {
                        pos.v2.opened_ms = now; // c2c: the next box is pre-assigned right now
                        pos.v2.jobtype = pos.latched_jobtype.clone(); // by now = the NEXT job's type
                    }
                    if let Some(c) = carry {
                        if pos.v2.opened_ms == 0 {
                            pos.v2.opened_ms = c.assigned_ms.min(now);
                            pos.v2.jobtype = pos.latched_jobtype.clone();
                        }
                        pos.v2.cur = Some(c);
                    } else if pos.v2.opened_ms > 0 {
                        // latch already points at the next target (assignment event consumed
                        // earlier) — no transition will fire, so seed the leg here. Guard:
                        // never seed with the leg we JUST finished at (a drop frame without
                        // the new raw topos still latches the old target).
                        if let Some(t) = pos.latched_topos.clone() {
                            if prev_topos.as_deref() != Some(t.as_str()) {
                                let t_crane = is_crane_code(&t);
                                let prepos = prepositioned_arrival(
                                    t_crane, &t, stopped, pos.cur_loc.as_deref().unwrap_or(""));
                                pos.v2.cur = Some(V2Leg {
                                    crane: t_crane,
                                    target: t,
                                    assigned_ms: now,
                                    arrived_ms: if prepos { now } else { 0 },
                                    arr_src: if prepos { "pre_positioned" } else { "" },
                                    left_ms: 0,
                                    arrived_lat: if prepos { pos.lat } else { 0.0 },
                                    arrived_lon: if prepos { pos.lon } else { 0.0 },
                                });
                            }
                        }
                    }
                }
                // (iii) topos1 transition = the next leg's assignment (opens a cycle if none)
                let raw_tp = pos.topos1.as_deref().unwrap_or("");
                if !raw_tp.is_empty()
                    && prev_topos.as_deref() != Some(raw_tp)
                    // not a transition if the in-progress leg already targets it (e.g. the
                    // reopen-seed above on this same frame)
                    && pos.v2.cur.as_ref().map(|c| c.target.as_str()) != Some(raw_tp)
                {
                    if pos.v2.opened_ms == 0 {
                        pos.v2.opened_ms = now;
                        pos.v2.jobtype = pos.latched_jobtype.clone();
                    }
                    if let Some(mut cur) = pos.v2.cur.take() {
                        if cur.arrived_ms > 0 && cur.left_ms == 0 {
                            cur.left_ms = now;
                        }
                        if pos.v2.legs.len() >= V2_LEGS_MAX {
                            pos.v2.legs.remove(1); // keep the first (pickup) + recent legs
                        }
                        pos.v2.legs.push(cur);
                    }
                    let tp_crane = is_crane_code(raw_tp);
                    let prepos = prepositioned_arrival(
                        tp_crane, raw_tp, stopped, pos.cur_loc.as_deref().unwrap_or(""));
                    pos.v2.cur = Some(V2Leg {
                        target: raw_tp.to_string(),
                        crane: tp_crane,
                        assigned_ms: now,
                        arrived_ms: if prepos { now } else { 0 },
                        arr_src: if prepos { "pre_positioned" } else { "" },
                        left_ms: 0,
                        arrived_lat: if prepos { pos.lat } else { 0.0 },
                        arrived_lon: if prepos { pos.lon } else { 0.0 },
                    });
                } else if pos.v2.opened_ms == 0 && !new_c1.is_empty() && old_c1.is_empty() {
                    pos.v2.opened_ms = now; // assignment observed via container1 only
                    pos.v2.jobtype = pos.latched_jobtype.clone();
                }
            }
        }
        devmap.insert(id.to_string(), pos);
    }
    if let Some(c) = completed_v2 {
        let mut buf = lm.cycle_v2.lock().await;
        buf.push_back(c);
        while buf.len() > CYCLE_BUF_MAX {
            buf.pop_front();
        }
    }
    if let Some(c) = completed {
        let mut buf = lm.cycle_log.lock().await;
        buf.push_back(c);
        while buf.len() > CYCLE_BUF_MAX {
            buf.pop_front();
            tracing::warn!("tt_cycle_log buffer over capacity; dropped oldest cycle");
        }
    }
    if fleet_drop {
        let mut drops = lm.tt_drops.lock().await;
        drops.push_back(now);
        while drops.front().is_some_and(|&f| now - f > MOVE_WINDOW_MS) { drops.pop_front(); }
    }
    if artifact {
        let mut arts = lm.tt_artifacts.lock().await;
        arts.push_back(now);
        while arts.front().is_some_and(|&f| now - f > MOVE_WINDOW_MS) { arts.pop_front(); }
    }
    if artifact_near {
        let mut near = lm.tt_artifacts_near.lock().await;
        near.push_back(now);
        while near.front().is_some_and(|&f| now - f > MOVE_WINDOW_MS) { near.pop_front(); }
    }
    if let Some(s) = cycle_sample_s {
        let mut cyc = lm.tt_cycles.lock().await;
        cyc.push_back((now, s));
        while cyc.front().is_some_and(|&(t, _)| now - t > MOVE_WINDOW_MS) { cyc.pop_front(); }
    }
    lm.messages.fetch_add(1, Ordering::Relaxed);
    lm.last_msg_ms.store(now as u64, Ordering::Relaxed);
    lm.ring.lock().await.bump(now / 60_000);
}

/// Numbers arrive as JSON strings ("2.9207...") or bare numbers; accept either.
fn num(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
}

/// Parse the feed's `arr_dtime` ("HH:MM:SS", terminal local time MYT=UTC+8) into epoch ms.
/// The field carries no date: attach the terminal date, and if the result lands in the
/// future (just past midnight reading a pre-midnight arrival) roll back one day. Reject
/// anything older than 6h as stale/garbled (a dwell that long is outside cycle scope).
fn parse_arr_dtime(s: &str, now_ms: i64) -> Option<i64> {
    let mut it = s.trim().split(':');
    let (h, m, sec) = (
        it.next()?.parse::<i64>().ok()?,
        it.next()?.parse::<i64>().ok()?,
        it.next().unwrap_or("0").parse::<i64>().ok()?,
    );
    if !(0..24).contains(&h) || !(0..60).contains(&m) || !(0..60).contains(&sec) {
        return None;
    }
    let tod_ms = (h * 3600 + m * 60 + sec) * 1000;
    const DAY: i64 = 86_400_000;
    const TZ: i64 = 8 * 3_600_000; // terminal MYT
    let terminal_midnight = ((now_ms + TZ) / DAY) * DAY - TZ;
    let mut t = terminal_midnight + tod_ms;
    if t > now_ms + 300_000 {
        t -= DAY; // clock just rolled past terminal midnight
    }
    (now_ms - t <= 6 * 3_600_000).then_some(t)
}

/// Trim a string field, returning None for empty.
fn opt_str(g: &serde_json::Value, key: &str) -> Option<String> {
    g.get(key)
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// "P12345<br/>(0136…)" → "P12345 / 0136…" (strip HTML, tidy whitespace).
fn clean_driver(s: &str) -> String {
    let mut out = s.replace("<br/>", " / ").replace("<br>", " / ").replace("<br />", " / ");
    out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    out
}

// ── Stage-2 SHADOW optimal-matching recommender ──────────────────────────────────────────────
// anti-thrash: a vehicle keeps its previous-tick work bucket unless another is >= this many
// arrival-seconds cheaper. Damps reassignment from small OD/GPS noise.
const SWITCH_PENALTY_S: i64 = 180;
/// 설계③ 풀 여유 — 마감이 (지금 + 이 값) 안에 든 슬롯을 풀에 담는다. 보드 깔때기의
/// '마감 도래' 계수도 이 값을 써야 화면과 매처가 같은 숫자를 본다.
pub(crate) const POOL_MARGIN_S: i64 = 300;
// committed window (anti-thrash, TOS prefetch "early-decide, stable-execute"): once a truck's prior
// recommendation is on the verge of dispatch (its work-ETA within this window), switching it away
// costs COMMIT_LOCK_S — a near-lock so GPS jitter can't flip an about-to-go truck off its work.
const COMMIT_WINDOW_MS: i64 = 600_000; // 10 min
const COMMIT_LOCK_S: i64 = 1200;
// NOTE: urgency / starvation / load-balance are NOT cost-matrix terms — Stage 2 is pure empty-travel
// efficiency. Urgency is decided in Stage 1 (설계③ 마감 기준 풀 — see spawn_stage2_shadow).
// per-container QC handling time (used by the deadline-slot walk below, mig 0121→0133)
const DS_MOVE_S: i64 = 90;
// LD_MOVE_S was 110s. Measured 2026-08-03 on consecutive comp_ts of a continuously-working crane:
// DS p50 90s (the DS constant is exact), LD p50 132s — the LD constant ran 20% optimistic, and it
// scales the deadline spread term ((cap/2)*move_s), so LD deadlines were short by that much too.
const LD_MOVE_S: i64 = 132;

// ── 출항 역산 마감 → Stage-1 선택 티어 (SHADOW) ──────────────────────────────────────────────
// 마감을 '값'이 아니라 '계층'으로만 쓴다. work_eta에는 DS +600s(workpool.rs DS_WORK_ETA_BIAS_S)·
// 학습잔차(LD 평균 +780s)·교대정지가 들어 있고 deadline_ts에는 없어서(workpool.rs의 "UNAFFECTED"
// 주석 참조) 두 값의 산술 혼합은 부적절하다. 분기로 결합하면 보정 계통이 섞이지 않는다.
// 2026-08-06 — 이 티어는 더 이상 풀 순서를 바꾸지 않는다(레거시 풀 제거, mig 0133). 출항 마감
// 위험도 게이지(dep_slack/dep_tier)로만 계산·기록한다 — 풀 선정과는 별개 축(CLAUDE.md 참고).
const DEP_TIGHT_S: i64 = 1800;        // = workpool FINISH_BUFFER_S. 마감식이 이미 '출항 30분 전
                                      //   완료'를 버퍼로 잡았으므로, 여유가 버퍼 하나보다 작다
                                      //   = 버퍼를 먹기 시작했다는 물리적 진술(임계값 발명 아님).
const DEP_HYST_S: i64 = 300;          // 60초 틱 5회. 여유는 벽시계만으로 틱당 −60s 표류하므로
                                      //   상승(덜 급해짐) 방향에만 밴드를 건다. SWITCH_PENALTY_S
                                      //   (180s)와 같은 자릿수.

// The yard grid (blocks + roads) is rotated ~29.8° from north. Manhattan distance measured along the
// QUAY-ALIGNED axes (not lat/lon) tracks the real road detour (×1.18 of straight-line ≈ the road
// graph's ×1.15) at near-zero cost — far better than straight-line for the untrained-OD (L3)
// estimate, because trucks drive the grid, not diagonally. Speed calibrated to actual trips.
const GRID_COS: f64 = 0.86777; // cos(29.8°)
const GRID_SIN: f64 = 0.49697; // sin(29.8°)
// L3 fallback when the road-network router can't answer (no graph / endpoint doesn't snap / no
// directed path). The cost is 순수주행 = the drive-SEGMENT time (empty_travel_start → work-point
// arrival): en-route stops are still driving and stay IN the cost; only the post-arrival handover
// wait is excluded (it lives in the next dwell segment). The crane approach term was dropped —
// measured consistently its median is ~0 (the old 72s was a select-biased early-arriver subset).
// SEG_SPEED_MS = grid-Manhattan ÷ segment time (781m/237s ≈ 3.3 m/s = 13 km/h).
const SEG_SPEED_MS: f64 = 3.30;
pub(crate) fn quay_manhattan_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const M: f64 = 111_320.0;
    let dn = (lat2 - lat1) * M;
    let de = (lon2 - lon1) * M * ((lat1 + lat2) / 2.0).to_radians().cos();
    let u = dn * GRID_COS + de * GRID_SIN; // along-grid
    let v = -dn * GRID_SIN + de * GRID_COS; // cross-grid
    u.abs() + v.abs()
}

// Min-cost max-flow (SPFA successive-shortest-paths). Tiny graphs (≤ a few hundred nodes) — used to
// compute the OPTIMAL vehicle→work assignment cost as a benchmark for the greedy solver (phase-2
// shadow). Edges are added in forward/reverse pairs so `e ^ 1` is the residual twin.
struct Mcmf {
    to: Vec<usize>,
    cap: Vec<i64>,
    cost: Vec<i64>,
    head: Vec<Vec<usize>>,
}
impl Mcmf {
    fn new(n: usize) -> Self {
        Mcmf { to: Vec::new(), cap: Vec::new(), cost: Vec::new(), head: vec![Vec::new(); n] }
    }
    fn add(&mut self, u: usize, v: usize, cap: i64, cost: i64) {
        let e = self.to.len();
        self.to.push(v); self.cap.push(cap); self.cost.push(cost); self.head[u].push(e);
        self.to.push(u); self.cap.push(0); self.cost.push(-cost); self.head[v].push(e + 1);
    }
    fn run(&mut self, s: usize, t: usize) -> (i64, i64) {
        let n = self.head.len();
        let (mut total_cost, mut total_flow) = (0i64, 0i64);
        loop {
            let mut dist = vec![i64::MAX; n];
            let mut in_q = vec![false; n];
            let mut pe = vec![usize::MAX; n];
            dist[s] = 0;
            let mut q = std::collections::VecDeque::new();
            q.push_back(s);
            in_q[s] = true;
            while let Some(u) = q.pop_front() {
                in_q[u] = false;
                let du = dist[u];
                for &e in &self.head[u] {
                    if self.cap[e] > 0 {
                        let v = self.to[e];
                        let nd = du + self.cost[e];
                        if nd < dist[v] {
                            dist[v] = nd;
                            pe[v] = e;
                            if !in_q[v] {
                                in_q[v] = true;
                                q.push_back(v);
                            }
                        }
                    }
                }
            }
            if dist[t] == i64::MAX {
                break;
            }
            let mut f = i64::MAX;
            let mut v = t;
            while v != s {
                let e = pe[v];
                f = f.min(self.cap[e]);
                v = self.to[e ^ 1];
            }
            let mut v = t;
            while v != s {
                let e = pe[v];
                self.cap[e] -= f;
                self.cap[e ^ 1] += f;
                v = self.to[e ^ 1];
            }
            total_cost += f * dist[t];
            total_flow += f;
        }
        (total_cost, total_flow)
    }
}

/// Optimal min-cost assignment (PURE efficiency), three layers: source → truck (cap 1) → bucket
/// (cost = empty-travel edge) → sink (cap = the bucket's Stage-1-capped demand). The per-crane cap is
/// already baked into the bucket demand by Stage 1, so no QC layer is needed here. Returns the chosen
/// (truck, bucket-pos) pairs.
fn optimal_assign(
    n_trucks: usize,
    bucket_caps: &[i64],
    edges: &[(usize, usize, i64)],
) -> Vec<(usize, usize)> {
    let b = bucket_caps.len();
    if n_trucks == 0 || b == 0 {
        return Vec::new();
    }
    let trucks0 = 1usize;
    let buckets0 = 1 + n_trucks;
    let t = 1 + n_trucks + b;
    let mut g = Mcmf::new(t + 1);
    for i in 0..n_trucks {
        g.add(0, trucks0 + i, 1, 0);
    }
    for &(u, v, c) in edges {
        g.add(trucks0 + u, buckets0 + v, 1, c);
    }
    for (j, &cap) in bucket_caps.iter().enumerate() {
        if cap > 0 {
            g.add(buckets0 + j, t, cap, 0);
        }
    }
    g.run(0, t);
    // extract assignment: a truck→bucket forward edge that carried flow has residual cap 0
    let mut assign = Vec::new();
    for truck in 0..n_trucks {
        for &e in &g.head[trucks0 + truck] {
            let v = g.to[e];
            if v >= buckets0 && v < t && g.cap[e] == 0 {
                assign.push((truck, v - buckets0));
            }
        }
    }
    assign
}

/// Every 60s, recommend vehicle→work matches and log them (SHADOW; never drives live dispatch).
/// Candidates = idle + soon-free TTs. Work = Stage-1 unassigned demand (build_workpool) with its
/// QC's work-ETA + pickup coord (LD=block centroid, DS=QC GPS). Cost = time-to-free + OD travel
/// (travel_cost_lookup layer, loaded once). Greedy: urgent work first gets its n cheapest feasible
/// vehicles. Logs arrival, conservative deadline slack, feasibility, OD tier → stage2_match_shadow.
const QC_DOCK_M: f64 = 35.0; // a docked LD truck sits within this of the QC workpoint (queued trucks are farther)

/// SHADOW (mig 0087): persist QC hook-load empty→laden rising edges (pickups) attributed to the docked
/// LD truck, to validate "handover-start = truck free" on OUR data before wiring into classify_tt. For
/// LD the pickup instant ≈ the truck-free instant, so edge→free residual should be near zero (vs the
/// current arrival-based soon_idle: 248s median / 837s p90 = almost all pre-pickup wait). Attribution:
/// the closest loaded LD truck within QC_DOCK_M of the crane workpoint; n_arrived flags queue ambiguity.
/// Only edges from the last ~25s are logged so the docked truck is still the one that was picked. NOT
/// wired into dispatch.
pub fn spawn_qc_handover_logger(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        let mut last_edge: HashMap<String, i64> = HashMap::new();
        let mut ticker = tokio::time::interval(Duration::from_secs(10));
        struct Edge { crane: String, ts: i64, ytno: Option<String>, container: Option<String>, jobtype: Option<String>, dist: Option<f64>, n: i32, land: Option<bool> }
        loop {
            ticker.tick().await;
            let now = Utc::now().timestamp_millis();
            let edges: Vec<Edge> = {
                let devices = lm.devices.read().await;
                let plc = lm.plc.read().await;
                let cranes_gps: HashMap<String, (f64, f64)> = devices.iter()
                    .filter(|(_, p)| matches!(p.cls.as_str(), "C" | "M" | "Z") && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|(id, p)| (id.clone(), (p.lat, p.lon))).collect();
                let crane_wp = { let line = *lm.quay_line.read().await; let g = lm.crane_wp.read().await; resolve_crane_wp(&line, &g, &cranes_gps) };
                let mut out: Vec<Edge> = Vec::new();
                for (crane, e) in plc.iter() {
                    let Some(&cpos) = crane_wp.get(crane) else { continue };
                    let seen = *last_edge.get(crane).unwrap_or(&0);
                    for &ts in e.moves.iter() {
                        if ts <= seen || now - ts > 25_000 {
                            continue;
                        }
                        // docked loaded LD trucks within QC_DOCK_M of the workpoint, closest first
                        let mut cand: Vec<(f64, &String, &Pos)> = devices.iter()
                            .filter(|(_, p)| p.cls == "TT" && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                            .filter(|(_, p)| {
                                let jt = p.jobtype.as_deref().or(p.latched_jobtype.as_deref());
                                let loaded = p.container1.as_deref().is_some_and(|s| !s.is_empty())
                                    || p.latched_container.as_deref().is_some_and(|s| !s.is_empty());
                                jt == Some("LD") && loaded
                            })
                            .map(|(id, p)| (dist_m((p.lat, p.lon), cpos), id, p))
                            .filter(|(d, _, _)| *d <= QC_DOCK_M)
                            .collect();
                        cand.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                        let n = cand.len() as i32;
                        let (ytno, container, jobtype, dist) = match cand.first() {
                            Some((d, id, p)) => (
                                Some((*id).clone()),
                                p.container1.clone().filter(|s| !s.is_empty()).or_else(|| p.latched_container.clone()),
                                p.jobtype.clone().or_else(|| p.latched_jobtype.clone()),
                                Some(*d),
                            ),
                            None => (None, None, None, None),
                        };
                        out.push(Edge { crane: crane.clone(), ts, ytno, container, jobtype, dist, n, land: e.land });
                    }
                }
                out
            };
            if edges.is_empty() {
                continue;
            }
            let (bd, sh) = tt_core::shift::current(tt_core::shift::terminal_now().naive_local());
            for ed in &edges {
                last_edge.insert(ed.crane.clone(), ed.ts.max(*last_edge.get(&ed.crane).unwrap_or(&0)));
                let _ = sqlx::query(
                    "INSERT INTO qc_handover_edge
                       (crane, edge_ts, ytno, container, jobtype, truck_dist_m, n_arrived, land, business_date, shift)
                     VALUES ($1, to_timestamp($2::float8/1000.0), $3,$4,$5,$6,$7,$8,$9,$10)
                     ON CONFLICT (crane, edge_ts) DO NOTHING",
                )
                .bind(&ed.crane).bind(ed.ts).bind(&ed.ytno).bind(&ed.container).bind(&ed.jobtype)
                .bind(ed.dist).bind(ed.n).bind(ed.land).bind(bd).bind(sh.label())
                .execute(&pool).await;
            }
        }
    });
}

const TIGHT_WP_M: f64 = 30.0; // truck within this of its OWN drop work-point (slot/bay) + stopped = tight-arrived
const AHEAD_R_M: f64 = 35.0;  // other stopped loaded same-jobtype trucks within this of the work-point = queue ahead

/// SHADOW (mig 0088): the redesign test. Per trip, log the FIRST TIGHT arrival at the truck's own drop
/// work-point (QC slot / RTG bay, ≤TIGHT_WP_M + stopped) plus the GPS ahead-count (stopped loaded
/// same-jobtype trucks clustered at that work-point). The user's model: work-point arrival = handover
/// start, and free ≈ crane_cycle × (1 + ahead) — bounded (QC 2 slots → ahead 0-1; RTG gantry-fixed →
/// bay arrival = committed). Validates offline vs tt_cycle_v2.dropped_at before wiring into classify_tt.
pub fn spawn_wp_arrival_logger(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        let mut open: HashSet<(String, String)> = HashSet::new(); // (ytno, container) already logged this trip
        let mut ticker = tokio::time::interval(Duration::from_secs(5));
        struct Arr { ytno: String, container: String, jobtype: String, wp_code: String, dist: f64, ahead: i32 }
        loop {
            ticker.tick().await;
            let now = Utc::now().timestamp_millis();
            let (cur_trip, arrivals): (HashSet<(String, String)>, Vec<Arr>) = {
                let devices = lm.devices.read().await;
                let centroids = lm.centroids.read().await;
                let cranes_gps: HashMap<String, (f64, f64)> = devices.iter()
                    .filter(|(_, p)| matches!(p.cls.as_str(), "C" | "M" | "Z") && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|(id, p)| (id.clone(), (p.lat, p.lon))).collect();
                let crane_wp = { let line = *lm.quay_line.read().await; let g = lm.crane_wp.read().await; resolve_crane_wp(&line, &g, &cranes_gps) };
                // work-point position for a loaded truck's drop target (topos1)
                let wp_of = |topos: &str, jt: &str| -> Option<(f64, f64)> {
                    if jt == "LD" { crane_wp.get(topos).copied().or_else(|| centroids.get(topos).map(|c| (c.lat, c.lon))) }
                    else { centroids.get(topos).or_else(|| centroids.get(block_prefix(topos))).map(|c| (c.lat, c.lon)) }
                };
                // loaded LD/DS trucks with a known work-point + their tight-arrival state (for ahead-count)
                struct T { id: String, container: String, jt: String, code: String, wp: (f64, f64), dist: f64, tight: bool }
                let mut trucks: Vec<T> = Vec::new();
                let mut cur: HashSet<(String, String)> = HashSet::new();
                for (id, p) in devices.iter() {
                    if p.cls != "TT" || (now - p.last_seen_ms) / 1000 > STALE_AFTER_S { continue; }
                    let jt = match p.jobtype.as_deref().or(p.latched_jobtype.as_deref()) { Some(j @ ("LD" | "DS")) => j.to_string(), _ => continue };
                    let container = match p.container1.clone().filter(|s| !s.is_empty()).or_else(|| p.latched_container.clone()) { Some(c) => c, None => continue };
                    let code = match p.topos1.clone().filter(|s| !s.is_empty()).or_else(|| p.latched_topos.clone()) { Some(t) => t, None => continue };
                    // only the DROP work-point frees the truck (classify_tt drop-side): LD at the quay
                    // crane, DS at a block bay. Skip the pickup-side arrival (LD's block, DS's crane).
                    let is_cr = is_crane_code(&code);
                    if !(if jt == "LD" { is_cr } else { !is_cr }) { continue; }
                    let Some(wp) = wp_of(&code, &jt) else { continue };
                    let dist = dist_m((p.lat, p.lon), wp);
                    cur.insert((id.clone(), container.clone()));
                    trucks.push(T { id: id.clone(), container, jt, code, wp, dist, tight: dist <= TIGHT_WP_M && p.speed < IDLE_SPEED_KMH });
                }
                // first tight arrival per trip; ahead-count = OTHER stopped loaded same-jt trucks near this wp
                let mut out: Vec<Arr> = Vec::new();
                for t in trucks.iter().filter(|t| t.tight) {
                    if open.contains(&(t.id.clone(), t.container.clone())) { continue; }
                    let ahead = trucks.iter().filter(|o| o.tight && o.id != t.id && o.jt == t.jt && dist_m(o.wp, t.wp) <= AHEAD_R_M).count() as i32;
                    out.push(Arr { ytno: t.id.clone(), container: t.container.clone(), jobtype: t.jt.clone(), wp_code: t.code.clone(), dist: t.dist, ahead });
                }
                (cur, out)
            };
            // release ended trips so a truck's next trip can log again
            open.retain(|k| cur_trip.contains(k));
            if arrivals.is_empty() { continue; }
            let (bd, sh) = tt_core::shift::current(tt_core::shift::terminal_now().naive_local());
            for a in &arrivals {
                open.insert((a.ytno.clone(), a.container.clone()));
                let _ = sqlx::query(
                    "INSERT INTO tt_wp_arrival (ytno, container, jobtype, wp_code, arrived_at, wp_dist_m, ahead_n, business_date, shift)
                     VALUES ($1,$2,$3,$4, now(), $5,$6,$7,$8) ON CONFLICT (ytno, container, arrived_at) DO NOTHING",
                )
                .bind(&a.ytno).bind(&a.container).bind(&a.jobtype).bind(&a.wp_code).bind(a.dist).bind(a.ahead).bind(bd).bind(sh.label())
                .execute(&pool).await;
            }
        }
    });
}

/// ⑤⑥ self-cal refresh (mig 0084): every ~15min REFRESH the two learned MVs and load them into the
/// live dispatch path — learn_free_in_bias → lm.free_in_bias, learn_soon_idle_gate → SOON_IDLE_GATE_MM.
/// Mirrors learn_work_eta_bias (⑦): the correction is measured from realized idle outcomes and fed
/// back, so both predictions self-recalibrate as conditions drift (7-day window in the MVs).
pub fn spawn_selfcal_refresh(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        loop {
            let _ = sqlx::query("REFRESH MATERIALIZED VIEW CONCURRENTLY learn_free_in_bias").execute(&pool).await;
            let _ = sqlx::query("REFRESH MATERIALIZED VIEW CONCURRENTLY learn_soon_idle_gate").execute(&pool).await;
            let _ = sqlx::query("REFRESH MATERIALIZED VIEW CONCURRENTLY learn_free_in_stationary").execute(&pool).await;
            // 정차 앵커: jobtype → (median, p90) seconds-to-free from the GPS-stationary moment.
            if let Ok(rows) = sqlx::query_as::<_, (String, i32, i32)>(
                "SELECT jobtype, med_s, p90_s FROM learn_free_in_stationary WHERE med_s IS NOT NULL AND n >= 100",
            ).fetch_all(&pool).await {
                let map: HashMap<String, (i64, i64)> = rows.into_iter()
                    .map(|(jt, m, p)| (jt, ((m as i64).clamp(30, 3600), (p as i64).clamp(30, 3600))))
                    .collect();
                if !map.is_empty() { *lm.stationary_free.write().await = map; }
            }
            // ⑥ free-in residual: learned median seconds-to-idle for soon_idle, per (jobtype, dist_bin).
            if let Ok(rows) = sqlx::query_as::<_, (String, String, i32, i32)>(
                "SELECT state, jobtype, dist_bin, med_rem_s FROM learn_free_in_bias WHERE med_rem_s IS NOT NULL AND n >= 50",
            ).fetch_all(&pool).await {
                let map: HashMap<(String, String, i16), i64> = rows.into_iter()
                    .map(|(st, jt, bin, med)| ((st, jt, bin as i16), (med as i64).clamp(30, 3600)))
                    .collect();
                if !map.is_empty() {
                    *lm.free_in_bias.write().await = map;
                }
            }
            // ⑤ soon-idle gate: learned DS RTG-distance cutoff holding precision ≥0.82.
            if let Ok(Some((gate_m,))) = sqlx::query_as::<_, (f32,)>(
                "SELECT gate_m FROM learn_soon_idle_gate WHERE jobtype = 'DS' AND n >= 200",
            ).fetch_optional(&pool).await {
                let mm = ((gate_m as f64) * 1000.0).round().clamp(30_000.0, 90_000.0) as u64;
                SOON_IDLE_GATE_MM.store(mm, Ordering::Relaxed);
            }
            tokio::time::sleep(std::time::Duration::from_secs(900)).await;
        }
    });
}

// ★ proactive pre-assignment: a soon-free truck goes SILENT while it waits (stopped = the GPS unit
// reports on movement only). ~25% of trucks are stale right before they free. We MUST keep them as
// soon-free candidates so a recommendation is always ready before TOS assigns them at the free moment.
// (2026-08-19) 종전 `SILENT_HOLD_S=1200`("20분 침묵이면 퇴근으로 본다")은 폐기 — 30분+ 침묵 트럭이
// 요청의 8.8%를 낸다. 퇴근 판정은 GPS 가 아니라 TOS 활동(3h 창)으로 하고, 침묵 트럭의 위치는
// truck_pos_hist 마지막 행으로 보충한다(정지한 트럭은 그 자리에 있다).
const HELD_NEAR_DROP_M: f64 = 120.0; // 옛 held 가지(앵커 없을 때만): 마지막 위치가 드랍 지점 이 안이면 곧 빔

/// pull 구조 후보 풀(mig 0154)의 "곧 빌 트럭" 지평(초). 예측 자유까지 시간이 이 값 이하인 배차 중 트럭만
/// 후보에 넣는다. 정답(TOS 자유시각) 가정 실측: 1분이면 요청 트럭 100%가 잡히나 우리 예측 오차가 4~5분이라
/// 15분에서 시작한다(풀 ~260대). 환경변수 `POOL_FREE_HORIZON_S` 로 덮어쓴다. 6시간 재현율 98.7%로 유지 확정.
const POOL_FREE_HORIZON_S: i64 = 900;
/// 후보의 위치로 인정하는 최대 나이(초). 장치 목록에 없는 트럭은 `truck_pos_hist` 마지막 픽스를 쓰는데, 그 표는
/// 2일치를 담아 상한이 없으면 34시간 전 좌표까지 비용행렬에 들어간다(2026-08-19 리뷰 실측). 정지한 트럭은 그
/// 자리에 있지만, 몇 시간이면 정지가 아니라 단말 사망/명단 이탈이다.
///
/// 값은 근무 판정의 창("TOS 활동 ≤3h")과 **같은 3시간**으로 뒀다. 실측 곡선(2026-08-21·1,018 요청·상한만 바꾼
/// 반사실): 3600s 94.8% · 7200s 95.5% · **10800s 96.3%** · 무제한 99.5%. 무제한과의 차이는 **GPS 단말이 죽었는데
/// TOS 는 계속 배차하는 트럭**이다(위치 나이 중앙 4.3h·최대 34h).
///
/// ⚠**이 값을 재현율만 보고 정하면 안 된다**(2026-08-21 2차 리뷰). 재현율은 "풀에 있었나"만 보므로 상한을 올리면
/// 항상 오르고 위치 오차에 대한 벌점이 없다. 실측한 위치 오차(그 트럭의 다음 픽스까지 거리·중앙): 10분 이내 229m ·
/// 30~60분 712m · 1~3h 974m. 그래서 아래에 **낡은 위치로 나간 추천 비율**을 계기로 같이 낸다 — 그 계기가 쌓이기
/// 전까지 이 상수는 "재현율 기준을 넘는 가장 보수적인 값"이지 검증된 값이 아니다.
const POS_MAX_AGE_S: i64 = 10800;
/// 풀 규칙 판(stage2_pool_truck_shadow.pool_ver). 1 = 첫 배포(2026-08-19 12:57 MYT) · 2 = 픽업 가드 + 앵커 status
/// 필터 제거(15:09 KST~) · 3 = 리뷰 반영(적하 GPS 우선 복구 · 위치 나이 상한 · asg 창 분리 · tos_sig 실패 시 GPS
/// 갈래 차단, 2026-08-21 09:01) · 4 = 적하 앵커를 '값'에서만 미루고 '풀 소속'은 유지(같은 날 10:30 — 3판이
/// 커버리지까지 버려 재현율 98.7→87.7% 회귀) · 5 = 위치 상한을 명단 창과 같은 3시간으로(같은 날 11:30 — 3600s
/// 는 재현율 94.7%로 기준 미달, 놓친 53건 중 44건이 이 상한이었다).
/// 재현율은 반드시 이 값으로 가른다 — 판이 다르면 모집단이 다르다.
const POOL_VER: i16 = 5;

/// 트럭 한 대가 **빈 채 대기 중**인가 — TOS 신호 네 개(자유·픽업·배차·배차목록 등재)만 보는 순수 판정.
///
/// 라이브에서 두 방향을 다 시험하려면 운영 표를 건드려야 하므로(같은 파일 `workpool_stale_reason` 선례) 판정만
/// 떼어 테스트로 고정한다. 느슨하면 일하는 트럭에 추천이 나가고, 빡빡하면 풀이 무너진다.
///
/// - `free` 가 없으면 빈 트럭이 아니다(3시간 창 안에 자유 사건이 없음).
/// - `picked > free` → 그 뒤 크레인이 상자를 실어줬다 = 싣고 가는 중. **적하 작업은 야드 픽업 직후 작업목록의
///   A/Q 에서 사라지므로** 이 가드가 없으면 싣고 가는 트럭이 빈 트럭으로 보인다(2026-08-19 실증).
/// - `dis > free` → 새 배차가 붙었다.
/// - `listed_at >= free` → 자유 **이후** 스냅샷에서 전 유형 배차 목록에 있다(`live_workpool` 은 A + (Q ∧ 트럭 없음)
///   만 담아 Q 로 배차된 트럭이 안 보인다). 자유가 스냅샷보다 새로우면 스냅샷이 낡은 것이라 자유를 믿는다.
fn is_free_tos(sig: &TosSig) -> bool {
    let Some(f) = sig.free else { return false };
    if sig.picked.is_some_and(|p| p > f) { return false; }
    if sig.dis.is_some_and(|d| d > f) { return false; }
    if sig.listed_at.is_some_and(|a| a >= f) { return false; }
    true
}

/// 트럭 한 대에 대한 TOS 쪽 신호 (후보 풀 판정용).
struct TosSig {
    free: Option<DateTime<Utc>>,      // 원천 드랍 로그의 마지막 자유(3h 창)
    dis: Option<DateTime<Utc>>,       // 작업목록(live_workpool)의 마지막 배차 시각 — A 상태 vessel 작업만 보인다
    jobtype: Option<String>,          // **지금 하는 일**의 유형(배차행 > 픽업 로그) — 운행 중 트럭용
    free_jt: Option<String>,          // **방금 끝낸 일**의 유형(자유 사건) — 빈 트럭용
    topos: Option<String>,            // 그 배차의 목적지 코드
    listed_at: Option<DateTime<Utc>>, // 전 유형 배차 목록(live_assigned_tt·A+Q)에 마지막으로 실린 스냅샷 시각
    picked: Option<DateTime<Utc>>,    // 크레인이 이 트럭에 상자를 실어준 마지막 시각(3h 창) — 자유보다 뒤면 싣고 있다
}

/// 이번 틱 후보 풀의 트럭 한 대 (stage2_pool_truck_shadow 한 행).
struct PoolRow {
    ytno: String,
    reason: &'static str,
    free_in_s: i64,
    pos_src: &'static str,
    gps_age_s: Option<i64>,
    jobtype: Option<String>, // 직전/진행 중 작업유형 (mig 0156) — DS/LD 로 사유를 가르기 위한 것
}

/// 배차 모드 (mig 0142). `DISPATCH_MODE=active` 일 때만 자기 추천 이력이 풀에서 작업을
/// 제외한다(추천→TOS 추출 반영 사이의 재추천 방지). 기본 shadow = 계상만(게이지).
/// 실배차 전환의 **운영 상태**이지 실험 레버가 아니다 — 값은 유닛 파일에 있다.
static DISPATCH_ACTIVE: AtomicBool = AtomicBool::new(false);

/// 작업목록(Oracle 미러)이 이보다 오래되면 매칭을 돌리지 않는다.
///
/// 정상 대역은 **12~62초 톱니**다(`tt-workpool` 타이머가 매분 :55 에 1회 — 2026-08-11 실측
/// 10초 간격 9회: 61·12·22·32·42·52·62·12·22). 300초는 5회 연속 결손에 해당해 한두 번의
/// Oracle 지연(같은 질의 2.0~15.2초 관측)으로는 걸리지 않는다. 프론트가 화면을 FROZEN 으로
/// 판정하는 임계와 같은 값이라, 화면이 "멈춤"이라고 말하는 순간과 매칭이 서는 순간이 갈리지 않는다.
const WORKPOOL_MAX_AGE_S: i64 = 300;

/// 작업목록 신선도 **판정** — 조회 결과를 받아 건너뛸 이유를 낸다(`None` = 신선).
///
/// 부수효과 없는 순수 함수로 떼어낸 이유: 이 판정이 과민하면 배차가 통째로 서고, 둔감하면
/// 낡은 목록으로 지시를 낸다. 라이브에서 두 방향을 다 시험하려면 운영 표를 건드려야 하므로
/// 판정만 떼어 테스트로 고정한다.
///
/// 인자 = `(추출이 마지막으로 성공한 뒤 경과초, 표 나이초)`, 조회 자체가 실패하면 `Err`.
/// **판정 불능은 전부 "낡음"으로 닫는다** — 모를 때는 멈추는 쪽이 안전하다.
fn workpool_stale_reason(res: Result<(Option<i64>, Option<i64>), String>) -> Option<String> {
    match res {
        Ok((Some(age), _)) if age <= WORKPOOL_MAX_AGE_S => None,
        Ok((Some(age), table_age)) => {
            let rows = table_age.map_or("빈 표".to_string(), |a| format!("{a}초"));
            Some(format!(
                "작업목록 추출이 {age}초째 성공하지 못했다 (임계 {WORKPOOL_MAX_AGE_S}초, 표 나이 {rows})"
            ))
        }
        Ok((None, _)) => {
            Some("작업목록 신선도 기록이 없다 (data_freshness 에 WORKPOOL 행 없음)".to_string())
        }
        Err(e) => Some(format!("작업목록 신선도 조회 실패: {e}")),
    }
}

/// 신선도·착지 시각을 한 번에 읽는 질의. 대기 루프와 게이트가 **같은 결과**를 쓴다.
///
/// 셋을 한 왕복에 담는 이유: 대기 루프가 마지막으로 본 값이 곧 그 틱의 게이트 입력이자
/// `workpool_age_s` 게이지다. 따로 읽으면 그 사이에 착지가 끼어들어 **깨어난 근거와 기록한
/// 나이가 다른 순간**을 가리킨다.
const SQL_WORKPOOL_FRESHNESS: &str = "
    SELECT (SELECT EXTRACT(epoch FROM now() - last_success_at)::int8
              FROM data_freshness WHERE kpi_key = 'WORKPOOL'),
           (SELECT EXTRACT(epoch FROM now() - max(as_of_ts))::int8 FROM live_workpool),
           (SELECT last_success_at FROM data_freshness WHERE kpi_key = 'WORKPOOL')";

/// 착지를 기다리며 신선도를 다시 보는 간격.
///
/// 실측 비용: 이 질의는 `data_freshness` PK 인덱스 히트라 **0.109ms · buffers 3**(2026-08-12
/// EXPLAIN ANALYZE). 2초 간격이면 시간당 1,800회 ≈ DB 시간 0.2초 — 로컬 Postgres 기준으로도
/// 무시할 수준이다. **Oracle 에는 닿지 않는다**(이 크레이트에는 Oracle 접근 수단이 없다).
const WAKE_POLL_MS: u64 = 2_000;

/// 새 목록이 안 와도 이만큼 지나면 깨어난다(폴백 = 하트비트).
///
/// **이것은 정상 운전에서 울리면 안 되는 값이다.** 처음에 60초로 뒀다가 라이브에서 틀렸음이
/// 드러났다(2026-08-12 실측 2시간): 착지 간격이 중앙 66초라 60초 폴백이 **착지 6초 전에**
/// 먼저 터져 틱의 42%가 폴백이 됐고(시간당 58 → 82.5틱), 그 폴백이 만든 낡은 판단이
/// 0.2~58초 뒤 신선한 판단을 **안티스래시로 밀어냈다** — `prev` 창(150초)이 직전 틱을
/// 기준점으로 삼고, 그걸 뒤집으려면 `SWITCH_PENALTY_S`(180초), 마감 임박이면
/// `COMMIT_LOCK_S`(1200초)를 물어야 하기 때문이다. `DISPATCH_MODE=active` 에서는 더 직접적이다:
/// 폴백 틱이 집은 상자가 자기 추천 커버(180초)에 걸려 신선 틱의 후보풀에서 빠진다.
/// ⇒ **낡은 목록이 신선한 목록을 선점한다.**
///
/// 150초의 근거는 아래 두 경계다(테스트 `폴백_대기는_관측된_착지_간격보다_뒤에_있다` 가 고정):
/// - **아래로**: 착지 간격 실측(중앙 66초·p90 ~140초)보다 뒤여야 정상 운전에서 착지가 이긴다.
/// - **위로**: 신선도 게이트(`WORKPOOL_MAX_AGE_S` 300초)보다 앞이어야 추출이 죽었을 때
///   게이트가 닫히기 전에 하트비트 틱이 최소 한 번은 남는다.
///
/// 앵커를 "마지막 착지 나이"로 옮기는 안은 기각했다 — 추출이 죽으면 나이가 이미 임계를
/// 넘어 있어 폴링 간격마다 도는 **새 폭주**가 된다. 여기서 재는 것은 "직전 틱 이후 경과"라
/// 그 자체가 속도 제한이다.
///
/// 폴백 틱에서도 매칭을 **거르지 않고 돌린다** — 작업목록이 그대로여도 트럭 GPS 가 바뀌므로
/// 매칭 결과는 달라진다.
const WAKE_MAX_WAIT_MS: u64 = 150_000;

/// 안티스래시가 "직전에 이 트럭에 뭘 시켰나"를 찾는 창(초).
///
/// ⚠ **하트비트보다 반드시 커야 한다.** 종전에는 이 값이 150초 리터럴로 SQL 안에 박혀 있었고,
/// 하트비트를 150초로 올린 첫 판에서 **두 값이 같아졌다.** 그러면 하트비트 틱에서는 직전 틱의
/// 행이 항상 창 밖이라(틱의 `ts` 는 본체 끝에 찍히고 하트비트는 그 뒤부터 센다) `prev` 가 비고,
/// `switched` 가 전부 false 가 되어 `SWITCH_PENALTY_S`·`COMMIT_LOCK_S` 가 통째로 꺼진다
/// ⇒ **가장 낡은 목록으로 도는 그 틱이 유일하게 제동 없이 전 트럭을 재배정하는 틱**이 된다.
/// 2026-08-12 2차 리뷰에서 잡혔다. 테스트 `안티스래시_창은_하트비트보다_넓다` 가 고정한다.
///
/// 하트비트 + 정상 틱 1회분(≈60초) 여유. 옛 주석의 "~2.5 틱"이라는 의도는 그대로다
/// (착지 간격 실측 중앙 60~66초 기준 ≈3.2틱).
const PREV_WINDOW_S: u64 = WAKE_MAX_WAIT_MS / 1000 + 60;

/// 매칭 틱이 깨어난 이유 — `stage2_solver_shadow.wake_src` 에 그대로 적는다 (mig 0153).
///
/// 값으로 사후 추정하지 않고 원천에 적는 이유: `landing` 과 `fallback` 을 `workpool_age_s`
/// 크기로 가르면 **결과에서 파생된 변수로 층화**하는 것이라, 뒤이어 "착지 틱은 나이가 작다"고
/// 보고하는 순간 동어반복이 된다(2026-08-03 에 같은 함정을 한 번 밟았다).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WakeSrc {
    /// 프로세스 기동 직후 첫 회. 목록 나이가 임의라 `landing` 집계에 섞으면 안 된다.
    Startup,
    /// 작업목록이 새로 착지했다 — 원하는 경로.
    Landing,
    /// 최대 대기를 채웠다. 목록은 직전 틱과 같다.
    Fallback,
}

impl WakeSrc {
    fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Landing => "landing",
            Self::Fallback => "fallback",
        }
    }
}

/// 대기 루프의 한 번의 판정.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WakeStep {
    Wake(WakeSrc),
    KeepWaiting,
}

/// 지금 깨어날 것인가 — 대기 루프의 **순수 함수**.
///
/// 종전(`phase_delay_ms`)에서 배운 것을 그대로 적용한다: 라이브에서 위상을 시험하려면
/// 프로세스를 재시작해야 하고, 틀려도 조용히 낡은 목록을 쓸 뿐이라 증상이 안 보인다.
/// 판정만 떼어 테스트로 고정한다.
///
/// 인자 = `(직전 틱이 쓴 착지 시각, 방금 조회한 착지 시각, 기다린 시간)`.
/// 앞의 둘이 같은 타입이라 뒤바꿀 수 있지만, 뒤바꾸면 "새 착지가 왔는데도 안 깨어난다" 쪽으로
/// 무너져 테스트가 잡는다(`새_착지가_오면_깨어난다`).
///
/// ⚠ **착지 시각을 모를 때(`None`)는 깨우지 않는다** — 조회가 실패했거나 `data_freshness` 에
///   행이 없는 경우다. 그대로 폴백까지 기다리면 신선도 게이트가 "판정 불능 = 낡음"으로 닫는다.
fn should_wake(
    seen: Option<DateTime<Utc>>,
    landed: Option<DateTime<Utc>>,
    waited: Duration,
) -> WakeStep {
    match (seen, landed) {
        // 기동 직후: 기준선이 없다. 지금 있는 목록으로 한 번 돌고 그것을 기준선으로 삼는다.
        //
        // ⚠ **`landed` 가 있을 때만** 깨운다. `(None, None)` 에서 깨우면 호출부가 `seen` 을
        //   전진시킬 값이 없어 영원히 `None` 으로 남고, 이 함수는 매 회차 첫 폴에서 즉시
        //   Wake 를 내며, 바깥 루프의 두 `continue` 경로(GPS 미연결·목록 낡음)에는 sleep 이
        //   없다 ⇒ **sleep 없는 폭주**가 된다. 도달 가능한 상태다: `last_success_at` 은
        //   nullable 이고 추출기가 첫 실행 실패 시 NULL 로 넣는다(`extractor/src/db.rs`).
        //   여기서 깨우지 않으면 아래 폴백까지 기다렸다가 신선도 게이트가 정상적으로 닫는다.
        (None, Some(_)) => WakeStep::Wake(WakeSrc::Startup),
        (Some(seen), Some(landed)) if landed > seen => WakeStep::Wake(WakeSrc::Landing),
        _ if waited >= Duration::from_millis(WAKE_MAX_WAIT_MS) => WakeStep::Wake(WakeSrc::Fallback),
        _ => WakeStep::KeepWaiting,
    }
}

/// 작업목록이 새로 착지할 때까지 기다린다. 반환 = `(깨어난 이유, 마지막 신선도 조회 결과)`.
///
/// 마지막 조회 결과를 그대로 돌려주므로 **틱당 추가 질의는 없다** — 게이트와 게이지가 이 값을
/// 재사용한다.
///
/// `seen` 전진 규칙과 그 대가:
/// - 착지를 **봤으면** 깨어난 이유와 무관하게 전진시킨다. 바깥 루프가 GPS 미연결이나 낡은
///   목록으로 `continue` 해 그 틱을 버리더라도 마찬가지다. ⚠ **대가**: 버려진 틱이 착지
///   하나를 흔적 없이 소모하므로, 기동 직후 첫 매칭이 최대 한 착지 간격만큼 늦을 수 있다.
///   그래도 전진시키는 쪽이 옳다 — 안 그러면 다음 회차가 이미 쓸모없어진 같은 착지로 즉시
///   깨어나 폴링 간격마다 도는 루프가 된다.
/// - **못 봤으면**(조회 실패 등) 그대로 둔다. 다음 성공 조회가 그 착지를 정상적으로 잡는다.
///   이 비대칭은 의도된 것이다.
async fn wait_for_workpool_landing(
    pool: &PgPool,
    seen: &mut Option<DateTime<Utc>>,
) -> (WakeSrc, Result<(Option<i64>, Option<i64>), String>) {
    let start = Instant::now();
    loop {
        let row = sqlx::query_as::<_, (Option<i64>, Option<i64>, Option<DateTime<Utc>>)>(
            SQL_WORKPOOL_FRESHNESS,
        )
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string());
        let landed = row.as_ref().ok().and_then(|&(_, _, at)| at);
        let gate_input = row.map(|(age, table_age, _)| (age, table_age));

        match should_wake(*seen, landed, start.elapsed()) {
            WakeStep::Wake(src) => {
                if landed.is_some() {
                    *seen = landed;
                }
                return (src, gate_input);
            }
            WakeStep::KeepWaiting => {
                tokio::time::sleep(Duration::from_millis(WAKE_POLL_MS)).await;
            }
        }
    }
}

pub fn spawn_stage2_shadow(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        DISPATCH_ACTIVE.store(
            std::env::var("DISPATCH_MODE").unwrap_or_default() == "active",
            Ordering::Relaxed,
        );
        let mut tick = 0u64;
        // 추천 생산 0(트럭·작업은 있는데) 연속 틱 수 — 3틱이면 경보 (mig 0142)
        let mut zero_streak: u32 = 0;
        // 작업목록이 낡아 건너뛴 연속 틱 수 (경보·로그를 한 줄로 접기 위한 것)
        let mut stale_streak: u32 = 0;
        // 티어 히스테리시스 상태(틱마다 현재 키 집합으로 통째 교체 = 누수 없음)
        let mut prev_tier: HashMap<(String, String, String), u8> = HashMap::new();
        // 직전 틱이 쓴 작업목록의 착지 시각. 이 값이 앞으로 가는 것이 곧 "새 목록이 왔다"다.
        let mut seen_landing: Option<DateTime<Utc>> = None;
        loop {
            // ── 깨어나기: 고정 초가 아니라 **작업목록 착지**를 기다린다 (2026-08-12, C안) ──
            //
            // 종전에는 매분 :15 에 돌았다. 그 위상의 전제는 "착지가 :00~:09 에 몰린다"(6시간
            // 표본, 착지 지연 최대 +14초)였는데 **하루 만에 깨졌다**: tt-workpool 실행이
            // 60초를 넘기면(실측 p90 65초·최대 84초) systemd 가 곧바로 다음 회차를 시작해
            // 착지 초가 자유주행한다(실측 분포 :15~:59). 그러면 매칭은 한 세대 낡은 목록을
            // 쓰는데, 나이가 ~70초라 300초 게이트에 안 걸린다 — 조용한 퇴화다.
            //
            // 고정 초를 어디로 옮겨도 자유주행을 따라갈 수 없으므로 **상수를 없앤다**.
            // 이제 tt-workpool.timer 의 초와 짝을 맞출 필요도 없다.
            let (wake_src, freshness) = wait_for_workpool_landing(&pool, &mut seen_landing).await;
            tick += 1;
            if !lm.connected.load(Ordering::Relaxed) {
                continue;
            }
            // 작업목록 신선도 게이트 — 위의 GPS 게이트와 짝이다. 종전에는 위치 피드만 검사하고
            // **Oracle 미러가 낡은 것은 아무도 보지 않았다**: 추출 타이머가 죽어도 매칭은 옛
            // 목록으로 추천을 계속 냈다. 지금은 그림자라 기록 오염에 그치지만, 실배차로 올리면
            // 그대로 "이미 끝난 상자로 트럭을 부르는" 지시가 된다.
            //
            // 신선도 출처가 data_freshness(WORKPOOL)인 이유: 이 표는 추출이 **성공적으로 돌았는지**
            // 를 담는다. live_workpool 의 max(as_of_ts) 로 재면 표가 빌 때 NULL 이라 "진짜로 일이
            // 없다"와 "추출이 죽었다"가 한 값으로 뭉개진다. 표 나이는 진단용으로만 함께 읽는다.
            // (data_freshness.is_stale 컬럼은 쓰지 않는다 — 5일 묵은 행도 f 로 남아 있어 관리되지
            //  않는 값이다. 2026-08-11 확인.)
            //
            // 조회는 위 대기 루프가 **이미 했다**(`freshness`). 여기서 다시 읽지 않는 이유는
            // 비용이 아니라 정합이다: 다시 읽으면 깨어난 근거와 게이트가 본 값이 서로 다른
            // 순간을 가리킬 수 있다.
            //
            // ★게이지로 남긴다 (mig 0150). 이 값은 게이트가 이미 재놓고 **버리던** 숫자다.
            //
            // ⚠ 이 값은 "목록이 담은 터미널 상태의 나이"가 **아니다**. 추출기는 Oracle 조회
            //   **전에** as_of 를 찍고 조회에 평균 ~20초가 든다(`extractor/src/workpool.rs`).
            //   즉 여기서 재는 것은 **착지 이후 경과**이고, 내용의 나이는 그보다 ~20초 많다.
            //   재고 싶은 것(= 우리가 착지를 얼마나 빨리 따라가는가)에는 이게 맞는 축이다.
            //
            // 정상 대역: `wake_src='landing'` 이면 **0~3초**(착지 신호 → 폴링 간격 2초 안에
            // 깨어난다), `fallback` 이면 150초 이상이다. **두 모집단을 섞어서 평균 내지 말 것**
            // — 반드시 `wake_src` 로 먼저 가른다(mig 0153). 종전 고정 위상(:15) 구간의
            // 대역은 6~15초였다.
            // ⚠낡아서 건너뛴 틱은 아래에서 continue 하므로 행 자체가 안 남는다 — 즉 이 컬럼은
            //   "매칭이 실제로 쓴 목록의 나이"만 담는다(그게 재고 싶은 값이다).
            let workpool_age_s: Option<i32> =
                freshness.as_ref().ok().and_then(|(age, _)| *age).map(|a| a as i32);
            let stale_why = workpool_stale_reason(freshness);
            if let Some(why) = stale_why {
                stale_streak += 1;
                // 첫 틱과 이후 10틱마다만 남긴다 — 장애가 길어져도 로그가 한 줄씩만 는다.
                // ⚠ 여기서 10틱은 **10분이 아니다**: 추출이 죽으면 이 구간의 틱은 전부
                //   하트비트(150초)라 10틱 ≈ **25분**이다. 최초 탐지는 첫 틱이라 영향 없고,
                //   늘어나는 것은 후속 반복 로그와 ops_alert.last_ts 갱신 간격뿐이다.
                if stale_streak == 1 || stale_streak % 10 == 0 {
                    tracing::warn!(streak = stale_streak, why = %why, "작업목록이 낡아 매칭을 건너뛴다");
                    crate::db::alert(
                        &pool,
                        "stage2_reco",
                        "stale_workpool",
                        "crit",
                        "작업목록이 낡아 배차 추천을 중단했다",
                        Some(&why),
                    )
                    .await;
                }
                // 건너뛰면 stage2_solver_shadow 에 이번 틱 행이 남지 않는다. 장기화되면 그 표의
                // DEADMAN(30분)이 백스톱으로 받고, 화면은 마지막 계산 나이로 이미 회색이 된다.
                continue;
            }
            if stale_streak > 0 {
                tracing::info!(skipped = stale_streak, "작업목록 신선도 회복 — 매칭 재개");
                stale_streak = 0;
            }
            let now = Utc::now().timestamp_millis();
            // previous-tick recommendation per vehicle (ytno → work bucket key) for anti-thrash.
            // ts-based (restart-safe), latest per vehicle within PREV_WINDOW_S.
            let prev: HashMap<String, (String, String, String)> = sqlx::query_as::<_, (String, String, String, String)>(
                &format!(
                    "SELECT DISTINCT ON (ytno) ytno, coalesce(qc,''), coalesce(vessel,''), coalesce(queuename,'')
                       FROM stage2_match_shadow WHERE ts > now() - interval '{PREV_WINDOW_S} seconds' ORDER BY ytno, ts DESC"
                ),
            )
            .fetch_all(&pool).await.unwrap_or_default()
            .into_iter().map(|(yt, q, v, qn)| (yt, (q, v, qn))).collect();
            // 자기 추천 이력 (mig 0142): 최근 180초 안에 추천한 상자 키. TTL 180초는
            // 추천→TOS 추출 반영 지연(~1-2분)을 덮는 값 — active 모드에서 이 집합에 든
            // 작업은 풀에서 제외되고, TTL이 지나도록 실배차 확인이 안 오면 다시 들어온다.
            let self_recent: std::collections::HashSet<(String, String, String)> =
                sqlx::query_as::<_, (String, String, String)>(
                    "SELECT DISTINCT vessel, queuename, contno FROM stage2_match_shadow
                      WHERE ts > now() - interval '180 seconds' AND contno IS NOT NULL",
                )
                .fetch_all(&pool).await.unwrap_or_default()
                .into_iter().collect();
            // mig 0116 — seconds between REACHING THE PICKUP POINT and the crane handover that our
            // travel model does not count. For DS the pickup point IS the crane, so this is small
            // (~74s of queue). For LD the pickup point is the YARD BLOCK while the deadline is the
            // QC at the quay, so the entire laden leg (RTG service + block→quay drive + quay queue)
            // was missing — measured ~1,014s. Comparing "time to reach the block" against "when the
            // QC needs it" is what drove LD feasibility to 30.9% while real crane starvation was
            // only 6–16%. Feasibility ONLY — never the edge cost (Stage-2 stays pure empty travel).
            let lead_extra: HashMap<String, i64> = sqlx::query_as::<_, (String, i32)>(
                "SELECT jobtype, extra_s FROM learn_dispatch_lead",
            )
            .fetch_all(&pool).await.unwrap_or_default()
            .into_iter().map(|(j, e)| (j, (e as i64).clamp(0, 2400))).collect();
            // ── move-log in-flight anchor ────────────────────────────────────────────────────
            // A truck is provably still working the moment a crane hands it a box: DS pickup is a
            // qc_move_log row, LD pickup an rtg_move_log row, and the trip closes with tt_move_log
            // .free_ts. That chain is TOS-authoritative, so it sees trucks the GPS cannot.
            //
            // Measured 2026-08-03 — this is ONLY used where GPS is blind, and the numbers say why.
            // Scored head-to-head against the one authoritative truth (tt_move_log.free_ts), on the
            // SAME snapshots, "how many seconds until this truck frees":
            //     DS  GPS |err| 424s (bias +288)   move-log |err| 311s (bias −106)
            //     LD  GPS |err| 240s (bias −240)   move-log |err| 659s (bias −641)
            // So the note further down — "retire the GPS duration and take it from the move-log
            // predictor" — is right for DS and WRONG for LD: a blanket swap would make the LD
            // estimate ~2.7× worse. GPS re-estimates every snapshot; the move-log prediction is
            // fixed at pickup and cannot track a truck that is running late.
            //
            // What GPS actually lacks is COVERAGE, not accuracy: 30.5% of in-flight DS trucks and
            // 35.4% of LD ones have had no fix for over 10 minutes (the units only report on
            // movement, so a truck queueing at a crane goes quiet). Those are exactly the trucks
            // about to free. So: GPS where it can see, this anchor where it cannot.
            let inflight: HashMap<String, i64> = sqlx::query_as::<_, (String, i64)>(
                "WITH pick AS (
                   -- ★status 로 거르지 않는다(2026-08-19). 종전 status='F' 는 빈 컨테이너(M·픽업의 ~35%) 트립에
                   --   앵커를 안 줘 그 트럭이 곧 빌 트럭 풀에서 빠졌다(실측 재현율 F 95~100% vs M 54~55%).
                   SELECT trk_id AS ytno, 'DS'::text jt, comp_ts pk FROM qc_move_log
                    WHERE jobtype='DS' AND comp_ts > now()-interval '3 hours' AND trk_id IS NOT NULL
                   UNION ALL
                   SELECT trk_id, 'LD', comp_ts FROM rtg_move_log
                    WHERE jobtype='LD' AND comp_ts > now()-interval '3 hours' AND trk_id IS NOT NULL
                 ), latest AS (
                   SELECT DISTINCT ON (ytno) ytno, jt, pk FROM pick ORDER BY ytno, pk DESC
                 ), freed AS (
                   -- ★원천 드랍 로그(2026-08-19). 종전 tt_move_log.free_ts 는 5분 조립 배치라 같은 값이
                   --   185초(p90 305) 늦게 보였다 — 원천은 33초(p90 57). 적하 자유 = QC 가 상자를 들어감
                   --   (qc_move_log LD) · 양하 자유 = 야드 인계(tos_handover_label DS). status 로 거르지
                   --   않는다(빈 컨테이너 M 이 30%). 실측 일치 100%/100%(6h·tt_move_log 대비).
                   SELECT ytno, max(f) f FROM (
                     SELECT trk_id ytno, comp_ts f FROM qc_move_log
                      WHERE jobtype='LD' AND comp_ts > now()-interval '3 hours' AND trk_id IS NOT NULL
                     UNION ALL
                     SELECT ytno, comp_ts FROM tos_handover_label
                      WHERE jobtype='DS' AND comp_ts > now()-interval '3 hours' AND ytno IS NOT NULL
                   ) u GROUP BY 1
                 )
                 SELECT l.ytno,
                        GREATEST(0, lr.remaining_p50 - EXTRACT(epoch FROM now() - l.pk))::int8
                   FROM latest l
                   LEFT JOIN freed fr ON fr.ytno = l.ytno
                   JOIN learn_cycle_remaining lr
                     ON lr.jobtype = l.jt AND lr.n_containers = 1 AND lr.dest_inflight_bucket = -1
                  WHERE fr.f IS NULL OR fr.f < l.pk",
            )
            .fetch_all(&pool).await
            .inspect_err(|e| tracing::warn!(error = %e, "in-flight anchor query failed — silent trucks fall back to the constant"))
            .unwrap_or_default()
            .into_iter().map(|(y, s)| (y, s.clamp(0, 3600))).collect();
            if tick % 10 == 0 {
                tracing::info!(n = inflight.len(), "move-log in-flight anchor");
            }
            // ── 후보 트럭 풀 — pull 구조 재정의 (2026-08-19 · mig 0154 · pool_ver 1) ────────────
            // 현장은 차량이 작업을 고른다: 트럭이 비면 TOS 에 요청하고 그 순간 받는다(자유→배차 중앙 15/38초,
            // 요청의 51.6%가 빈 뒤 30초 안). 우리 답은 트럭이 묻기 **전에** 있어야 하고, 그러려면
            //   (a) 이미 빈 채 서 있는 트럭은 GPS 라벨·침묵과 무관하게 전부 풀에 있어야 하고
            //   (b) 곧 빌 트럭은 예측 자유까지 시간 ≤ H 이면 들어와야 한다.
            // 종전 풀(GPS 상태 idle/soon_idle/wait_rtg 만)은 요청 트럭의 ~20%만 담았다 — 놓친 건의 90%가
            // "풀에 없어서", 그중 대부분이 실제로는 빈 트럭(GPS 라벨 delivering/staging = latched 잔류,
            // 또는 정지→침묵). 실측은 docs/cycles/2026-08-19-pull-coverage-findings.md.
            //
            // 규칙(우선순위 순):
            //   free_tos     : 원천 드랍 로그(적하 qc_move_log LD · 양하 tos_handover_label DS)에 자유가 찍혔고
            //                  그 뒤 새 배차(live_workpool.yt_dis_ts) 없음 → 자유까지 0. GPS 상태 무시.
            //   inflight_*   : 새 배차가 자유보다 뒤(= 작업 중) → 예측 자유까지 시간 ≤ POOL_FREE_HORIZON_S 이면 포함.
            //                  예측 = 무브로그 픽업 앵커(inflight) > GPS 상태 학습값(soon_idle/wait_rtg) > 옛 held 가지.
            //                  그 밖("명백히 작업 중")은 제외 — 함대의 35~50%.
            //   gps_free     : TOS 에 최근 3h 기록이 없는데 GPS 가 신선하고 빈 차로 보임(신규 투입 등) → 0.
            // 위치: 신선 GPS > 장치 목록의 낡은 픽스(≤600s) > truck_pos_hist 마지막 행(정지한 트럭은 그 자리) >
            //       옛 held 가지의 드랍 지점. 명단 규칙("TOS 활동 3h ∪ GPS 30분")은 위 3h 창과 GPS 조건에 녹아 있다.
            //       "20분 침묵 = 퇴근"(SILENT_HOLD_S) 가정은 폐기 — 30분+ 침묵 트럭이 요청의 8.8%를 낸다.
            // ⚠**풀 크기는 Stage-1 슬롯 수를 직접 정한다**(아래 `let truck_n = vehicles.len()`): 풀 63→253 으로 틱당
            //   발행 추천이 47.9→74.3(+55%)이 됐다. 2026-08-21 사용자 결정으로 **그대로 둔다** — pull 구조에서 곧 빌
            //   트럭에도 다음 일을 미리 정해두는 것이 맞다고 봤다. 다만 "이른 발신 → 트럭이 크레인 앞에서 기다림"
            //   위험은 재현율 잣대로 안 보이므로 반사실 대기 재측정이 pull 2/2 의 숙제다.
            // 이 풀은 stage2_pool_truck_shadow 에 틱마다 남는다(pool_ver 로 판을 가른다).
            let pool_h_s: i64 = std::env::var("POOL_FREE_HORIZON_S").ok().and_then(|v| v.parse().ok()).unwrap_or(POOL_FREE_HORIZON_S);
            // TOS 쪽 신호: 트럭별 마지막 자유(원천) · 마지막 배차(작업목록 yt_dis_ts) · 그 배차의 작업유형/목적지 ·
            // 전 작업유형 배차 목록(live_assigned_tt·A+Q)에 실린 마지막 스냅샷 시각.
            // ⚠live_workpool 은 "A + (Q ∧ 트럭 없음)" 만 담는다 — Q(대기) 상태로 배차된 트럭(~80대)이 거기 없어
            //   첫 배포 틱에서 free_tos 247 중 67대가 실은 배차 중이었다. live_assigned_tt 로 막는다.
            let tos_sig_rows =
                sqlx::query_as::<_, (String, Option<DateTime<Utc>>, Option<DateTime<Utc>>, Option<String>, Option<String>, Option<String>, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
                    "WITH freed AS (
                       -- 자유 사건은 그 트럭이 **방금 끝낸 작업의 유형**도 알려준다(적하 자유 = QC 가 배에 실음,
                       -- 양하 자유 = 야드 인계). free_tos 트럭은 작업목록에서 이미 사라져 배차행 유형이 없으므로
                       -- 유형은 여기서만 온다(mig 0156 의 jobtype 컬럼).
                       SELECT DISTINCT ON (ytno) ytno, f, jt FROM (
                         SELECT trk_id ytno, comp_ts f, 'LD'::text jt FROM qc_move_log
                          WHERE jobtype='LD' AND comp_ts > now()-interval '3 hours' AND trk_id IS NOT NULL
                         UNION ALL
                         SELECT ytno, comp_ts, 'DS' FROM tos_handover_label
                          WHERE jobtype='DS' AND comp_ts > now()-interval '3 hours' AND ytno IS NOT NULL
                       ) u ORDER BY ytno, f DESC
                     ), disp AS (
                       SELECT DISTINCT ON (ytno) ytno, yt_dis_ts d, jobtype, coalesce(nullif(to_pos,''), nullif(yt_topos,'')) topos
                         FROM live_workpool WHERE ytno IS NOT NULL AND ytno <> '' AND yt_dis_ts IS NOT NULL
                        ORDER BY ytno, yt_dis_ts DESC
                     ), asg AS (
                       -- ⚠창을 신선도 게이트(WORKPOOL_MAX_AGE_S=300)와 같은 값으로 두면 안 된다(2026-08-19 리뷰):
                       --   추출기는 Oracle 조회 **전에** as_of 를 찍어 now-as_of ≈ age+20s 이므로, age 280~300s 구간에서
                       --   틱은 돌면서 listed_at 이 전부 NULL 이 되고 Q 배차 트럭이 다시 빈 트럭으로 샌다(f2af6a2 회귀).
                       --   판정은 창이 아니라 아래 listed_at >= free 비교가 한다 — 창은 넉넉히 준다.
                       SELECT ytno, max(as_of_ts) asof FROM live_assigned_tt
                        WHERE as_of_ts > now() - interval '30 minutes' AND ytno IS NOT NULL GROUP BY 1
                     ), picked AS (
                       -- 크레인이 이 트럭에 상자를 실어준 마지막 시각(픽업). 자유보다 뒤면 지금 싣고 있다.
                       -- ⚠적하 작업은 야드 픽업 직후 A/Q 에서 사라져(싣고 가는 동안 작업목록·배차목록에 없다)
                       --   자유·배차 신호만 보면 빈 트럭으로 오판한다(첫 배포 12:57 TT1272 실증) — 이 가드가 막는다.
                       -- ★픽업은 **지금 하는 일의 유형**도 알려준다(양하 픽업=QC, 적하 픽업=RTG). 적하 운행 중에는
                       --   작업목록에 배차행이 없어(실측: 적하 픽업 206대 중 108대가 목록에 아예 없음) 유형을
                       --   자유 사건에서 가져오면 **직전 트립의 유형**이 된다 — 실측 오라벨 53.5%(2026-08-21 2차 리뷰).
                       SELECT DISTINCT ON (ytno) ytno, p, pjt FROM (
                         SELECT trk_id ytno, comp_ts p, 'DS'::text pjt FROM qc_move_log
                          WHERE jobtype='DS' AND comp_ts > now()-interval '3 hours' AND trk_id IS NOT NULL
                         UNION ALL
                         SELECT trk_id, comp_ts, 'LD' FROM rtg_move_log
                          WHERE jobtype='LD' AND comp_ts > now()-interval '3 hours' AND trk_id IS NOT NULL
                       ) u ORDER BY ytno, p DESC
                     ), ids AS (
                       SELECT ytno FROM freed UNION SELECT ytno FROM disp UNION SELECT ytno FROM asg UNION SELECT ytno FROM picked
                     )
                     -- 유형은 **갈래별로 다른 출처**다(2026-08-21 2차 리뷰): 빈 트럭은 방금 끝낸 일(f.jt),
                     -- 운행 중 트럭은 지금 하는 일(배차행 d.jobtype > 없으면 픽업 pk.pjt). 한 컬럼으로 합치면
                     -- 적하 운행 중이 직전 트립 유형으로 뒤집힌다.
                     SELECT i.ytno, f.f, d.d, coalesce(d.jobtype, pk.pjt), f.jt, d.topos, a.asof, pk.p
                       FROM ids i LEFT JOIN freed f USING (ytno) LEFT JOIN disp d USING (ytno) LEFT JOIN asg a USING (ytno) LEFT JOIN picked pk USING (ytno)",
                )
                .fetch_all(&pool).await;
            // ★실패를 '풀 축소'가 아니라 '풀 오염'으로 끝내지 않는다(2026-08-19 리뷰). 이 질의가 비면 아래 2)번의
            //   "TOS 에 기록이 없다 = 배차된 적 없다" 근거가 통째로 거짓이 되어, 배차돼 staging 중인 트럭까지 빈 차로
            //   계상된다. 실패한 틱은 GPS 갈래를 끄고 경보를 올린다(warn 로그 한 줄로는 사후에 구분이 안 된다).
            // ★실패하면 **그 틱은 통째로 건너뛴다.** 이 질의가 비면 아래 1)번 루프(`tos_sig.iter()`)가 한 번도 돌지
            //   않아 앵커도 조회되지 않는다 — 즉 "GPS 갈래만 끈다"가 아니라 후보가 0이 된다(2026-08-21 2차 리뷰에서
            //   경보 문구가 사실과 달랐던 것). 작업목록 신선도(위 stale_workpool)와 같은 성격이라 등급도 crit 로 맞춘다.
            if let Err(e) = &tos_sig_rows {
                tracing::warn!(error = %e, "TOS 자유/배차 신호 질의 실패 — 이번 틱 매칭을 건너뛴다 (mig 0154/0155)");
                crate::db::alert(&pool, "stage2_pool", "tos_sig_query", "crit",
                    "후보 풀의 TOS 자유/배차 신호 질의가 실패해 매칭을 건너뛴다 — 그 틱은 추천이 나가지 않는다",
                    Some(&e.to_string())).await;
                continue;
            }
            let tos_sig: HashMap<String, TosSig> = tos_sig_rows.unwrap_or_default()
                .into_iter().map(|(y, f, d, jt, fjt, tp, asof, pk)| (y, TosSig { free: f, dis: d, jobtype: jt, free_jt: fjt, topos: tp, listed_at: asof, picked: pk })).collect();
            #[allow(clippy::type_complexity)]
            let vehicles_built: (Vec<(String, f64, f64, i64, &'static str)>, Vec<HeldCandidateOut>, Vec<PoolRow>, Vec<(String, i64, &'static str, &'static str, Option<String>)>) = {
                let map = lm.devices.read().await;
                let plc = lm.plc.read().await;
                let centroids = lm.centroids.read().await;
                let assigned_pool = lm.assigned_pool.read().await;
                let rtgs: Vec<(f64, f64)> = map.values()
                    .filter(|p| p.cls == "RTG" && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|p| (p.lat, p.lon)).collect();
                let cranes: HashMap<String, (f64, f64)> = map.iter()
                    .filter(|(_, p)| matches!(p.cls.as_str(), "C" | "M" | "Z") && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|(id, p)| (id.clone(), (p.lat, p.lon))).collect();
                let cranes = {
                    let line = *lm.quay_line.read().await;
                    let g = lm.crane_wp.read().await;
                    resolve_crane_wp(&line, &g, &cranes)
                };
                let fi_bias = lm.free_in_bias.read().await; // ⑥ learned soon_idle seconds-to-idle
                let st_free = lm.stationary_free.read().await; // 정차 앵커 (mig 0091)
                let mut v: Vec<(String, f64, f64, i64, &'static str)> = Vec::new();
                let mut held: Vec<HeldCandidateOut> = Vec::new();
                let mut rows: Vec<PoolRow> = Vec::new();
                let mut n_ds_anchor: usize = 0;
                // 위치를 장치 목록에서 못 찾은 후보(침묵 >600s) — 뒤에서 truck_pos_hist 로 한 번에 채운다
                let mut need_pos: Vec<(String, i64, &'static str, &'static str, Option<String>)> = Vec::new(); // (ytno, base, state, reason, jobtype)

                // ── 1) TOS 신호가 있는 트럭(자유 or 배차 중) ──────────────────────────────
                for (id, sig) in tos_sig.iter() {
                    if !id.starts_with("TT") { continue; }
                    let dev = map.get(id);
                    let age = dev.map(|p| (now - p.last_seen_ms) / 1000);
                    let free_now = is_free_tos(sig);
                    let jt_tos: Option<&str> = sig.jobtype.as_deref().filter(|j| matches!(*j, "LD" | "DS"));
                    if free_now {
                        // 빈 채 대기 — GPS 상태 라벨과 무관하게 포함. 위치는 있는 대로.
                        let free_jt: Option<&str> = sig.free_jt.as_deref().filter(|j| matches!(*j, "LD" | "DS"));
                        match dev {
                            Some(p) => {
                                let src = if age.unwrap_or(i64::MAX) <= STALE_AFTER_S { "gps_live" } else { "gps_stale" };
                                v.push((id.clone(), p.lat, p.lon, 0, "free_tos"));
                                rows.push(PoolRow { ytno: id.clone(), reason: "free_tos", free_in_s: 0, pos_src: src, gps_age_s: age, jobtype: free_jt.map(str::to_string) });
                                if age.unwrap_or(0) > STALE_AFTER_S {
                                    held.push(HeldCandidateOut { id: id.clone(), jobtype: free_jt.unwrap_or_default().to_string(), free_in_s: 0, anchored: false });
                                }
                            }
                            None => need_pos.push((id.clone(), 0, "free_tos", "free_tos", free_jt.map(str::to_string))),
                        }
                        continue;
                    }
                    // 배차 중 — 예측 자유까지 시간
                    let anchored = inflight.get(id).copied();
                    // ★앵커를 언제 쓰나 — 위 헤드투헤드 실측(4711~ 주석)이 정한 규칙을 그대로 지킨다:
                    //   "GPS 가 보는 곳은 GPS, 못 보는 곳은 앵커". 적하는 **값의 정확도**가 GPS 우세(|err| 240s vs
                    //   앵커 659s)라 GPS 가 그 트럭을 곧-빔으로 분류하면 GPS 값을 쓴다. 양하는 앵커가 우세(311s vs 424s).
                    //   ⚠단 **정확도와 커버리지를 섞지 말 것**(2026-08-21 회귀에서 배움): 위 주석이 "GPS 에 없는 것은
                    //   정확도가 아니라 COVERAGE"라고 적어둔 그대로다. 적하에서 앵커를 통째로 버렸더니 GPS 가
                    //   `delivering` 으로 보는 트럭(=곧 빌 것을 GPS 가 못 알아본 트럭)이 풀에서 사라져 재현율이
                    //   98.7%→87.7% 로 떨어졌다. 그러니 적하는 **GPS 분류가 될 때만 GPS 값**을 쓰고, 안 되면 앵커로
                    //   풀에 남긴다 — 값은 GPS 우선, 소속은 앵커가 보장.
                    let gps_fresh = age.unwrap_or(i64::MAX) <= STALE_AFTER_S;
                    // 적하 + GPS 신선 + GPS 가 곧-빔으로 분류 → 그때만 앵커를 미룬다.
                    let gps_sees_soon = gps_fresh && jt_tos == Some("LD") && dev.is_some_and(|p| {
                        matches!(classify_tt(p, assigned_pool.get(id), &rtgs, &plc, &cranes, &centroids, now).state,
                                 "soon_idle" | "wait_rtg" | "idle")
                    });
                    let anchored = if gps_sees_soon { None } else { anchored };
                    let (base, state, reason): (i64, &'static str, &'static str) = match (anchored, dev) {
                        // 앵커: TOS 권위 · GPS 가 못 보는 트럭도 본다(커버리지)
                        (Some(rem), _) => (rem.clamp(0, 3600), "soon_idle_anchored", "inflight_anchor"),
                        // 앵커 없고 GPS 신선: 상태 학습값 (soon_idle/wait_rtg 만 예측 가능; delivering 은 상수라 H 밖)
                        (None, Some(p)) if gps_fresh => {
                            let c = classify_tt(p, assigned_pool.get(id), &rtgs, &plc, &cranes, &centroids, now);
                            match c.state {
                                s @ ("soon_idle" | "wait_rtg") => {
                                    let jt = p.jobtype.clone().or_else(|| p.latched_jobtype.clone()).or_else(|| jt_tos.map(|x| x.to_string())).unwrap_or_default();
                                    let stopped = p.speed < IDLE_SPEED_KMH;
                                    let b = if let Some(&(med, _p90)) = st_free.get(&jt).filter(|_| stopped) { med } else {
                                        let bin = dist_bin_of(c.nearest_rtg_m);
                                        fi_bias.get(&(s.to_string(), jt.clone(), bin))
                                            .or_else(|| fi_bias.get(&(s.to_string(), jt.clone(), -99)))
                                            .copied()
                                            .unwrap_or_else(|| free_in(s, (!jt.is_empty()).then_some(jt.as_str())).0.unwrap_or(0))
                                            .clamp(30, 3600)
                                    };
                                    (b, if s == "soon_idle" { "soon_idle" } else { "wait_rtg" }, "inflight_gps")
                                }
                                "idle" => (0, "idle", "inflight_gps"), // TOS 는 배차 중이라는데 GPS 는 빈 차 정지 — 작업목록 60초 지연 구간. 곧 빔으로 본다.
                                _ => (i64::MAX, "delivering", "inflight_gps"),
                            }
                        }
                        // 앵커 없고 GPS 낡음/없음: 옛 held 가지(짐 실음 + 드랍 120m 안)만 살린다
                        (None, Some(p)) => {
                            let jt = match p.jobtype.as_deref().or(p.latched_jobtype.as_deref()).or(jt_tos) { Some(j @ ("LD" | "DS")) => j, _ => "" };
                            let loaded = p.container1.as_deref().is_some_and(|s| !s.is_empty())
                                || p.latched_container.as_deref().is_some_and(|s| !s.is_empty());
                            let code = p.topos1.as_deref().filter(|s| !s.is_empty()).or(p.latched_topos.as_deref()).or(sig.topos.as_deref());
                            let mut r = (i64::MAX, "delivering", "inflight_held");
                            if !jt.is_empty() && loaded {
                                if let Some(code) = code {
                                    let is_cr = is_crane_code(code);
                                    let side_ok = if jt == "LD" { is_cr } else { !is_cr };
                                    let dpos = if is_cr { cranes.get(code).copied().or_else(|| centroids.get(code).map(|c| (c.lat, c.lon))) }
                                               else { centroids.get(code).or_else(|| centroids.get(block_prefix(code))).map(|c| (c.lat, c.lon)) };
                                    if side_ok {
                                        if let Some((dlat, dlon)) = dpos {
                                            if dist_m((p.lat, p.lon), (dlat, dlon)) <= HELD_NEAR_DROP_M {
                                                let b = st_free.get(jt).map(|&(m, _)| m).unwrap_or(300).clamp(30, 3600);
                                                r = (b, "soon_idle_held", "inflight_held");
                                            }
                                        }
                                    }
                                }
                            }
                            r
                        }
                        (None, None) => (i64::MAX, "delivering", "inflight_held"),
                    };
                    if base > pool_h_s { continue; } // 명백히 작업 중 — 이번 틱 풀 밖
                    if reason == "inflight_anchor" && jt_tos == Some("DS") { n_ds_anchor += 1; }
                    match dev {
                        Some(p) => {
                            let src = if age.unwrap_or(i64::MAX) <= STALE_AFTER_S { "gps_live" } else { "gps_stale" };
                            v.push((id.clone(), p.lat, p.lon, base, state));
                            rows.push(PoolRow { ytno: id.clone(), reason, free_in_s: base, pos_src: src, gps_age_s: age, jobtype: jt_tos.map(str::to_string) });
                            if age.unwrap_or(0) > STALE_AFTER_S {
                                held.push(HeldCandidateOut { id: id.clone(), jobtype: jt_tos.unwrap_or_default().to_string(), free_in_s: base, anchored: anchored.is_some() });
                            }
                        }
                        None => need_pos.push((id.clone(), base, state, reason, jt_tos.map(str::to_string))),
                    }
                }
                // ── 2) TOS 신호가 없는데 GPS 가 신선하고 빈 차인 트럭 (신규 투입 등) ────────────
                // 이 갈래의 근거는 "TOS 에 최근 3h 기록이 없다 = 배차된 적 없다" — 위 질의가 실패하면 그 틱은
                // 이미 건너뛰었으므로 여기까지 오면 근거가 성립한다.
                for (id, p) in map.iter() {
                    if p.cls != "TT" || tos_sig.contains_key(id) { continue; }
                    let age = (now - p.last_seen_ms) / 1000;
                    if age > STALE_AFTER_S { continue; }
                    let c = classify_tt(p, assigned_pool.get(id), &rtgs, &plc, &cranes, &centroids, now);
                    match c.state {
                        // TOS 가 최근 3h 아무 기록도 없다 = 배차된 적 없다. 빈 차로 보이면 빈 차다(staging 의 '배차됨'은 latched 잔류).
                        "idle" | "staging" | "empty_travel" => {
                            v.push((id.clone(), p.lat, p.lon, 0, "free_gps"));
                            rows.push(PoolRow { ytno: id.clone(), reason: "gps_free", free_in_s: 0, pos_src: "gps_live", gps_age_s: Some(age), jobtype: p.jobtype.clone().or_else(|| p.latched_jobtype.clone()).filter(|j| matches!(j.as_str(), "DS" | "LD")) });
                        }
                        // 싣고 있다는데 TOS 기록이 없다 — 라벨 잔류로 본다. 이번 틱은 제외.
                        _ => {}
                    }
                }
                if tick % 10 == 0 {
                    let n_free = rows.iter().filter(|r| r.reason == "free_tos").count();
                    let n_inf = rows.iter().filter(|r| r.reason.starts_with("inflight")).count();
                    let n_gps = rows.iter().filter(|r| r.reason == "gps_free").count();
                    tracing::info!(total = v.len(), free_tos = n_free, inflight = n_inf, gps_free = n_gps, ds_anchor = n_ds_anchor, horizon_s = pool_h_s, "후보 풀 (pull 재정의)");
                }
                (v, held, rows, need_pos)
            };
            // ── 3) 장치 목록에 없는 후보의 위치: truck_pos_hist 마지막 행 ──────────────────────
            // ★락을 놓은 뒤에 질의한다(2026-08-19 리뷰). 위 블록이 끝나며 devices/plc/centroids/assigned_pool
            //   읽기 가드가 전부 풀린다 — 종전에는 이 질의의 await 를 가드가 넘어가, 매 분 웹소켓 인제스트
            //   (plc/centroids write)가 질의 시간만큼 멈췄다(이 파일 3455행 "no two locks held" 규약).
            let (vehicles, held_out, pool_rows) = {
                let (mut v, mut held, mut rows, need_pos) = vehicles_built;
                if !need_pos.is_empty() {
                    let ids: Vec<String> = need_pos.iter().map(|x| x.0.clone()).collect();
                    // ⚠`DISTINCT ON (ytno) … ORDER BY ytno, ts DESC` 는 PK(ytno,ts)를 역방향으로 못 타 2일치를
                    //   전부 읽고 디스크 정렬했다(실측 452ms/틱·8MB). unnest+LATERAL 로 트럭당 인덱스 seek 1회.
                    let found: HashMap<String, (f64, f64, DateTime<Utc>)> = sqlx::query_as::<_, (String, f64, f64, DateTime<Utc>)>(
                        "SELECT u.ytno, p.lat, p.lon, p.ts
                           FROM unnest($1::text[]) AS u(ytno)
                           JOIN LATERAL (SELECT lat, lon, ts FROM truck_pos_hist h
                                          WHERE h.ytno = u.ytno AND h.lat IS NOT NULL
                                          ORDER BY h.ts DESC LIMIT 1) p ON true",
                    )
                    .bind(&ids)
                    .fetch_all(&pool).await
                    .inspect_err(|e| tracing::warn!(error = %e, "truck_pos_hist 위치 보충 실패 (mig 0154)"))
                    .unwrap_or_default()
                    .into_iter().map(|(y, la, lo, t)| (y, (la, lo, t))).collect();
                    let (mut n_nopos, mut n_tooold) = (0usize, 0usize);
                    for (id, base, state, reason, jt) in need_pos {
                        match found.get(&id) {
                            Some(&(la, lo, t)) => {
                                let age = (now - t.timestamp_millis()) / 1000;
                                // ★위치 나이 상한(2026-08-19 리뷰). "정지한 트럭은 그 자리에 있다"는 참이지만,
                                //   몇 시간 전 픽스는 정지가 아니라 단말 사망/명단 이탈이다. 그런 트럭이 최단 비용으로
                                //   뽑히면 존재하지 않는 위치로 추천이 나간다(실측: 1시간+ 낡은 후보가 틱당 25대).
                                if age > POS_MAX_AGE_S { n_tooold += 1; continue; }
                                v.push((id.clone(), la, lo, base, state));
                                rows.push(PoolRow { ytno: id.clone(), reason, free_in_s: base, pos_src: "pos_hist", gps_age_s: Some(age), jobtype: jt.clone() });
                                held.push(HeldCandidateOut { id, jobtype: jt.clone().unwrap_or_default(), free_in_s: base, anchored: reason == "inflight_anchor" });
                            }
                            None => { n_nopos += 1; }
                        }
                    }
                    if (n_nopos > 0 || n_tooold > 0) && tick % 10 == 0 {
                        tracing::info!(no_pos = n_nopos, too_old = n_tooold, cap_s = POS_MAX_AGE_S,
                            "후보 트럭인데 쓸 위치가 없어 뺐다 (위치 없음 / 나이 상한 초과)");
                    }
                }
                (v, held, rows)
            };
            // ★낡은 위치 계기 (2026-08-21 2차 리뷰) — 재현율은 위치 오차를 못 본다. GPS 피드가 죽으면 장치 목록이
            //   10분 뒤 비고 전 후보가 `pos_hist` 로 넘어가, 상한(3h)까지 **얼어붙은 좌표로 추천이 계속 나간다**
            //   (2026-07-16 케이블 단선 선례가 이 모양이다). 그래서 "쓴 위치가 얼마나 낡았나"를 매 틱 세고,
            //   30분 넘은 위치가 풀의 절반을 넘으면 경보한다 — 피드 사망의 첫 신호다.
            {
                let n = pool_rows.len().max(1);
                let stale30 = pool_rows.iter().filter(|r| r.gps_age_s.is_some_and(|a| a > 1800)).count();
                let stale60 = pool_rows.iter().filter(|r| r.gps_age_s.is_some_and(|a| a > 3600)).count();
                if tick % 10 == 0 {
                    tracing::info!(pool = pool_rows.len(), stale_30m = stale30, stale_60m = stale60,
                        pct_30m = (100 * stale30 / n), "후보 풀이 쓴 위치의 나이");
                }
                if stale30 * 2 > n && pool_rows.len() >= 20 {
                    crate::db::alert(&pool, "stage2_pool", "stale_positions", "warn",
                        "후보 풀의 절반 넘는 트럭이 30분 이상 낡은 위치를 쓰고 있다 — GPS 피드를 확인할 것",
                        Some(&format!("pool={} stale_30m={} stale_60m={} cap_s={}", pool_rows.len(), stale30, stale60, POS_MAX_AGE_S))).await;
                }
            }
            // 풀 기록 (mig 0154) — 배정 여부와 무관하게 이번 틱 풀 전부. 풀 재현율의 분모 쪽 근거.
            if !pool_rows.is_empty() {
                let ts_now = Utc::now();
                let ytnos: Vec<String> = pool_rows.iter().map(|r| r.ytno.clone()).collect();
                let reasons: Vec<String> = pool_rows.iter().map(|r| r.reason.to_string()).collect();
                let bases: Vec<i32> = pool_rows.iter().map(|r| r.free_in_s.clamp(0, 2_000_000_000) as i32).collect();
                let srcs: Vec<String> = pool_rows.iter().map(|r| r.pos_src.to_string()).collect();
                let ages: Vec<Option<i32>> = pool_rows.iter().map(|r| r.gps_age_s.map(|a| a.clamp(0, 2_000_000_000) as i32)).collect();
                let jts: Vec<Option<String>> = pool_rows.iter().map(|r| r.jobtype.clone()).collect();
                if let Err(e) = sqlx::query(
                    "INSERT INTO stage2_pool_truck_shadow (ts, ytno, reason, free_in_s, pos_src, gps_age_s, jobtype, pool_ver)
                     SELECT $1::timestamptz, u.ytno, u.reason, u.free_in_s, u.pos_src, u.gps_age_s, u.jobtype, $8::int2
                       FROM unnest($2::text[], $3::text[], $4::int4[], $5::text[], $6::int4[], $7::text[]) AS u(ytno, reason, free_in_s, pos_src, gps_age_s, jobtype)
                     ON CONFLICT (ts, ytno) DO NOTHING",
                )
                .bind(ts_now).bind(&ytnos).bind(&reasons).bind(&bases).bind(&srcs).bind(&ages).bind(&jts).bind(POOL_VER)
                .execute(&pool).await
                {
                    tracing::warn!(error = %e, "stage2_pool_truck_shadow 기록 실패");
                }
                if tick % 30 == 0 {
                    crate::db::prune(&pool, "stage2_pool_truck_shadow", "DELETE FROM stage2_pool_truck_shadow WHERE ts < now() - interval '3 days'").await;
                }
            }
            // 배차 목록 스냅샷 이력 (mig 0155) — "트럭이 물어본 순간" = 자유 뒤 처음 실린 틱. tt_move_log 는 최종
            // 배차만 남겨 재배정된 첫 배차가 사라진다. 매 틱 ~350행, ON CONFLICT 로 같은 스냅샷 중복 없음.
            if let Err(e) = sqlx::query(
                "INSERT INTO assigned_tt_hist (as_of_ts, ytno, jobstatus)
                 SELECT as_of_ts, ytno, jobstatus FROM live_assigned_tt WHERE ytno IS NOT NULL
                 ON CONFLICT (as_of_ts, ytno) DO NOTHING",
            ).execute(&pool).await {
                tracing::warn!(error = %e, "assigned_tt_hist 기록 실패");
            }
            if tick % 30 == 15 {
                crate::db::prune(&pool, "assigned_tt_hist", "DELETE FROM assigned_tt_hist WHERE as_of_ts < now() - interval '3 days'").await;
            }
            // publish the pool this tick ACTUALLY uses (empty included — that's the truth) so
            // positions/TT-page mirror the matcher's numbers instead of re-deriving them.
            {
                let mut sp = lm.stage2_pool.write().await;
                sp.as_of_ms = now;
                sp.bases = vehicles.iter().map(|(id, _, _, b, _)| (id.clone(), *b)).collect();
                sp.held = held_out;
            }
            if vehicles.is_empty() {
                continue;
            }
            // candidate work + pickup coord
            let Ok((_, work)) = crate::workpool::stage2_work_candidates(pool.clone()).await else { continue };
            let (cranes_now, centroids_now): (HashMap<String, (f64, f64)>, HashMap<String, (f64, f64)>) = {
                let map = lm.devices.read().await;
                let cr = map.iter()
                    .filter(|(_, p)| matches!(p.cls.as_str(), "C" | "M" | "Z") && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|(id, p)| (id.clone(), (p.lat, p.lon))).collect();
                let ce = lm.centroids.read().await;
                let cem = ce.iter().map(|(k, c)| (k.clone(), (c.lat, c.lon))).collect();
                (cr, cem)
            };
            // (work index, pickup lat, pickup lon, work-ETA ms) — only those with a coord + ETA
            // ⚠ 여기서 후보가 **조용히 빠진다**. 좌표가 없거나 작업시작 시각이 없으면 그대로 사라지고
            // 아무도 세지 않았다 — 오늘 발견한 결함들이 전부 이런 식으로 숨어 있었으므로 센다(mig 0121).
            // 작업시작 시각이 없으면 배차 마감도 만들 수 없으니 새 규칙에서도 똑같이 빠진다.
            let mut works_no_coord: i32 = 0;
            let mut works_no_eta: i32 = 0;
            let works: Vec<(usize, f64, f64, i64)> = work.iter().enumerate().filter_map(|(i, w)| {
                let coord = if w.jobtype == "LD" {
                    w.src_block.as_ref().and_then(|b| centroids_now.get(b).copied())
                } else {
                    cranes_now.get(&w.qc).copied().or_else(|| centroids_now.get(&w.qc).copied())
                };
                let Some(coord) = coord else { works_no_coord += 1; return None };
                let Some(eta) = w.work_eta_ts else { works_no_eta += 1; return None };
                Some((i, coord.0, coord.1, eta.timestamp_millis()))
            }).collect();
            if works_no_coord + works_no_eta > 0 && tick % 10 == 0 {
                tracing::info!(no_coord = works_no_coord, no_eta = works_no_eta, kept = works.len(),
                    "Stage-2 후보에서 제외된 작업");
            }
            if works.is_empty() {
                continue;
            }
            // OD cost = road-network ROUTE TIME (directed Dijkstra over the inferred graph: lane
            // speeds + work-point connectors) × the actual÷route calibration learned from realized
            // empty trips (road_route_eval) — see roadgraph::RouteCost. Replaces the 225m grid lookup
            // (mig 0082): the router answers every pair, and the graph has headroom Manhattan lacks
            // (lane speeds, one-ways, congestion) + generalizes to non-grid terminals. tier R = routed,
            // L3 = Manhattan fallback (unroutable pair).
            let rc = crate::roadgraph::RouteCost::load(&pool).await;
            let cost = |vlat: f64, vlon: f64, wlat: f64, wlon: f64, is_ld: bool| -> (i64, i64, &'static str) {
                match rc.p50_p90(vlat, vlon, wlat, wlon, is_ld) {
                    Some((p50, p90)) => (p50 as i64, p90 as i64, "R"),
                    None => {
                        // unroutable pair → Manhattan, mapped through the SAME learned realized
                        // scale as routed costs (raw manh÷speed ran systematically hot vs R).
                        let m = quay_manhattan_m(vlat, vlon, wlat, wlon);
                        match rc.manh_p50_p90(m, is_ld) {
                            Some((p50, p90)) => (p50 as i64, p90 as i64, "L3"),
                            None => { let p50 = m / SEG_SPEED_MS; (p50 as i64, (p50 * 1.5) as i64, "L3") }
                        }
                    }
                }
            };
            // 버킷 여유 = (이 베이의 완료기한 − now) − 이 베이의 처리시간
            //          = "지금 당장 트럭이 붙어도 자기 마감 전에 끝나는가"
            // 대수적으로 = (크레인 여유 + 앞선 작업량)이라 크레인 '내부'에서는 work-ETA와 같은 단조
            // 순서 → 크레인 내부 순서는 오늘과 동일하고, 효과는 100% 크레인 '사이' 재배열이다. 위험
            // 크레인의 앞쪽 베이만 자동으로 티어0에 들어온다(뒤 베이는 자기 마감이 뒤 작업을 이미
            // 반영하므로 덜 급함). 실측 2026-07-27: 여유<0이 26버킷·147컨·11 QC이고 그 11개 위험 QC
            // 중 지금 굶주림인 것은 1개뿐 — 즉 기존 최상위 키와 거의 직교하는 신호다.
            // ⚠ 여유 산식에 work_eta를 섞지 않는다(deadline_ts·proc_s 둘 다 출항 역산 단일 출처).
            let mut dep_slack: Vec<Option<i64>> = Vec::with_capacity(works.len());
            let mut dep_tier: Vec<u8> = Vec::with_capacity(works.len());
            let mut next_tier: HashMap<(String, String, String), u8> = HashMap::new();
            for &(wi, _, _, _) in &works {
                let w = &work[wi];
                // 하드 언랩 금지: 불변식(deadline ⟺ work_eta ⟺ proc)은 오늘의 사실이지 계약이 아니다.
                let s: Option<i64> = w.deadline_ts.zip(w.proc_s)
                    .map(|(d, p)| (d.timestamp_millis() - now) / 1000 - p);
                let raw: u8 = match s {
                    None => 2,                              // 마감 없음(가상선박 RHXX 등) = 중립
                    Some(v) if v < 0 => 0,                  // 이미 늦음
                    Some(v) if v < DEP_TIGHT_S => 1,        // 빠듯 (완료 버퍼를 먹는 중)
                    _ => 2,                                 // 여유
                };
                let key = (w.qc.clone(), w.vessel.clone(), w.queuename.clone());
                let p = prev_tier.get(&key).copied().unwrap_or(raw);
                // 하강(더 급해짐)은 즉시, 상승(덜 급해짐)은 밴드를 넘어야 허용 — 경계 왕복(flap)이 풀
                // 멤버십을 흔들어 강제 전환(thrash)을 늘리는 것을 막는다. thrash의 95%가 "직전 QC가
                // 풀에서 사라져서" 생기는 강제 전환이고 SWITCH_PENALTY/COMMIT_LOCK은 그 경우 무력하다.
                let t = if raw <= p { raw } else {
                    let need = if p == 0 { DEP_HYST_S } else { DEP_TIGHT_S + DEP_HYST_S };
                    if s.unwrap_or(i64::MAX) >= need { raw } else { p }
                };
                next_tier.insert(key, t);
                dep_slack.push(s);
                dep_tier.push(t);
            }
            prev_tier = next_tier;
            // ── 설계 ③ — 마감 기준 풀 (2026-08-06 부터 매칭을 구동한다. mig 0121 → 0132 → 0133) ─
            // 규칙: 모든 작업을 `마감 − 여유` 가 이른 순으로 줄 세우고, 그 시각이 지난 것만 담는다.
            // 담을 게 트럭보다 적으면 트럭을 남긴다(억지로 채우지 않는다).
            //
            // 묶음 하나에 컨테이너가 여럿이면 슬롯마다 마감이 다르다 —
            //     슬롯 j 마감 = 베이시작 + j×무브시간 − 준비시간 = (묶음 마감) + j×무브시간
            // 크레인은 베이 안을 차례로 처리하므로(실측 88% 계획순서·물량기준 위반 6%) 이 근사가 선다.
            //
            // ⚠ 여유(POOL_MARGIN_S)는 잠정값이다. "우리가 우리 예측을 못 믿는 만큼"이 정의인데,
            //   작업도달 예측의 퍼짐이 IQR ~1,400초라 그 절반도 안 되는 보수적 값에서 출발한다.
            //   1단계에서 마감이 지난 슬롯(pool_overdue_n)이 계속 나오면 늘려야 한다.
            //   (상수는 모듈 상단 pub(crate) — 보드 깔때기가 같은 잣대로 '마감 도래'를 센다.)
            let mut self_cover_n: i32 = 0; // 자기 추천 이력 적중 수 (mig 0142)
            let (pool_new, pool_overdue_n, trucks_held_n, due_buckets_n) = {
                // (works 인덱스, 이 틱에 마감이 도래한 슬롯 수, 가장 이른 슬롯의 마감 ms)
                let mut due: Vec<(usize, i64, i64)> = Vec::new();
                let mut overdue: i32 = 0;
                for (oi, &(wi, _, _, _)) in works.iter().enumerate() {
                    let w = &work[wi];
                    // ★배차 대상은 **아직 배차 안 된 상자**만 (2026-08-04 사용자 지시).
                    //
                    // 시간 계산(앞에 얼마나 밀렸나)은 TOS 배차와 무관하게 **전부** 센다 — 그건 위
                    // works/구역 카운터가 담당한다. 하지만 트럭을 실제로 **보낼 대상**은 다르다:
                    // TOS 가 이미 트럭을 보낸 상자에 우리가 또 보내면 낭비다.
                    //
                    // 실측(2026-08-04): TOS 배차분은 지시 나이 중앙 54~70분으로 이미 처리 중이고,
                    // 미배차분은 23~26분이다. 마감이 지난 것으로 세어지던 612개는 대부분 전자였다 —
                    // 크레인이 지금 다루는 상자라 배차 마감이 준비시간만큼 전에 지난 게 당연하고,
                    // 우리가 할 수 있는 일이 없다.
                    //
                    // ⚠ 이건 **시작 시점의 인수인계** 규칙이었다. "TOS 가 우리 결과로 배차하게
                    //   되면 이 조건을 우리 배차 이력으로 바꾼다"고 적어뒀었는데, 다시 따져보니
                    //   **바꾸는 게 아니라 둘이 공존하는 것**이 맞다(2026-08-11 재검토):
                    //   - `tos_assigned` 는 그때도 참이다 — TOS 가 배차했다면 출처가 우리 추천이든
                    //     아니든 **그 상자에는 이미 트럭이 가 있다.** 또 보내면 낭비다.
                    //   - 진짜 구멍은 그 사이의 지연이었다: 우리가 추천하고 TOS 반영까지 1~2분,
                    //     그동안 tos_assigned 가 아직 거짓이라 같은 상자를 다시 추천한다.
                    //     그 구멍은 아래 자기 추천 커버(mig 0142·TTL 180초)가 메운다.
                    //   ⇒ 즉 이 줄은 그대로 두고, active 전환 시 필요한 것은 아래 한 줄뿐이다.
                    //     라이브 확인(2026-08-11): self_cover_n 28~44 가 직전 틱 추천 33~46 을
                    //     따라붙고 추천행 contno 채움률 100% — 키가 맞아 배선이 살아 있다.
                    if w.tos_assigned { continue }
                    // 자기 추천 이력 (mig 0142): shadow = 계상만(self_cover_n 게이지 — 직전 틱
                    // 추천 수와 비슷해야 배선이 산 것, 0이면 키 불일치 버그). active = 제외.
                    if let Some(c) = w.contno.as_ref() {
                        if self_recent.contains(&(w.vessel.clone(), w.queuename.clone(), c.clone())) {
                            self_cover_n += 1;
                            if DISPATCH_ACTIVE.load(Ordering::Relaxed) { continue }
                        }
                    }
                    let Some(dd) = w.dispatch_deadline_ts else { continue };
                    let move_s = if w.jobtype == "LD" { LD_MOVE_S } else { DS_MOVE_S };
                    let base = dd.timestamp_millis();
                    let cutoff = now + POOL_MARGIN_S * 1000;
                    let mut slots = 0i64;
                    for j in 0..w.n.max(0) as i64 {
                        let slot = base + j * move_s * 1000;
                        if slot <= cutoff { slots += 1; if slot < now { overdue += 1; } } else { break }
                    }
                    if slots > 0 { due.push((oi, slots, base)); }
                }
                due.sort_by_key(|&(_, _, d)| d); // 마감이 이른 순
                let truck_n = vehicles.len() as i64;
                let mut acc = 0i64;
                let mut kept_new: Vec<(usize, i64)> = Vec::new();
                for &(oi, slots, _) in &due {
                    if acc >= truck_n { break }
                    let alloc = slots.min(truck_n - acc); // 마감 도래 슬롯만큼, 남은 트럭 수로 절단
                    acc += alloc;
                    kept_new.push((oi, alloc));
                }
                if tick % 3 == 0 {
                    let due_slots_total: i64 = due.iter().map(|&(_, s, _)| s).sum();
                    let kept_slots: i64 = kept_new.iter().map(|&(_, alloc)| alloc).sum();
                    tracing::info!(truck_n, acc, held = (truck_n - acc).max(0),
                        due_buckets = due.len(), due_slots_total, kept_buckets = kept_new.len(), kept_slots,
                        "설계③ 트럭 배분");
                }
                if tick % 5 == 0 {
                    let with_dd = works.iter().filter(|&&(wi, _, _, _)| work[wi].dispatch_deadline_ts.is_some()).count();
                    let min_slack = works.iter()
                        .filter_map(|&(wi, _, _, _)| work[wi].dispatch_deadline_ts)
                        .map(|d| (d.timestamp_millis() - now) / 1000)
                        .min();
                    let assigned_n = work.iter().filter(|w| w.tos_assigned).count();
                    tracing::info!(works = works.len(),
                        assigned_rows = assigned_n, with_deadline = with_dd,
                        min_slack_s = ?min_slack, due_buckets = due.len(),
                        "설계③ 새 규칙 진단");
                }
                (kept_new, overdue, (truck_n - acc).max(0) as i32, due.len() as i32)
            };
            let pool_new_set: std::collections::HashSet<usize> = pool_new.iter().map(|&(oi, _)| oi).collect();
            // (qc,vessel,queuename) → work-ETA ms, for the committed-window check on prior recommendations
            let eta_by_key: HashMap<(String, String, String), i64> = work.iter()
                .filter_map(|w| w.work_eta_ts.map(|e| ((w.qc.clone(), w.vessel.clone(), w.queuename.clone()), e.timestamp_millis())))
                .collect();
            // 구동 풀(mig 0121 → 0132 → 0133 → 0140 → 0141). pool_mode=3: 마감 = 출항 요구
            // 페이스 균등 배분 — (출항까지 남은 시간)÷(남은 무브 수)를 무브당 배정 시간으로,
            // j번째 무브 시작 = now + j×배정시간, 매 틱 fresh now 로 재계산(2026-08-10 확정).
            // 2(출항 역산×학습 걸음, 같은 날 ~30분 라이브)·1(전방 예측 마감, 08-06~10)과
            // 모집단이 다르므로 집계는 반드시 이 값으로 가른다.
            let pool_mode: i16 = 3;
            let driving: Vec<(usize, i64)> = pool_new;
            // STAGE 2 — PURE EFFICIENCY MATCHING. The work pool + per-crane demand caps are already
            // fixed by Stage 1; here each edge cost is just the truck's empty travel (+ anti-thrash
            // switch penalty). No urgency/starve/load-balance terms and no QC layer — those are Stage-1.
            let mut caps: Vec<i64> = Vec::with_capacity(driving.len()); // per-bucket demand (Stage-1 capped)
            let mut deadlines: Vec<i64> = Vec::with_capacity(driving.len()); // feasibility deadline ms per wpos
            // 출항 축 로깅용(정렬 후 wpos 인덱스로 재배열). 기존 deadlines[]와 '별개 축'이다 —
            // 절대 교체/융합하지 않는다(현행 지평 p50 1043s는 실제 도착 p90 538s와 같은 스케일이라
            // 진짜 물리는 지표인데, 출항 마감 p50 17041s로 바꾸면 전 구간 88% 평평한 상수가 된다).
            let mut dep_slack_w: Vec<Option<i64>> = Vec::with_capacity(driving.len());
            let mut dep_tier_w: Vec<u8> = Vec::with_capacity(driving.len());
            let mut edges: Vec<(usize, usize, i64)> = Vec::new(); // (truck, work-pos, cost)
            let mut matrix: Vec<Vec<(i64, i64, &'static str, bool)>> = Vec::with_capacity(driving.len()); // [wpos][vi]=(arr,p90,tier,switched)
            for &(oi, cap_j) in &driving {
                let (wi, wlat, wlon, eta_ms) = works[oi];
                let w = &work[wi];
                // 무브시간은 마감 계산에서 빠졌다(mig 0122: spread 항 폐기). 상자별 시각은
                // workpool 쪽에서 이미 매겨 넘어온다.
                // cap_j = 구동 풀이 배정한 트럭 몫.
                caps.push(cap_j);
                // 마감 = 크레인이 이 컨테이너를 다루는 시각. **더하는 항 없음**(mig 0122).
                //
                // 옛 식은 `max(eta, now) + (크레인당 트럭 상한 ÷ 2) × 무브시간` 이었다. 설계는
                // "크레인 시각에서 트럭 준비시간을 **뺀다**" 인데 반대로 더하고 있었고, 게다가 그
                // 상한(NEED_HORIZON_S)은 트럭을 여러 크레인에 흩뿌리려고 둔 값이라 마감과 무관한
                // 설정이 마감을 정하고 있었다. 실측 2026-08-04(16시간·51,856행, 정답=그 큐의 다음
                // 크레인 핸드오버): 절대오차 양하 1,344 → 642초(52.2% 개선·위약 953초보다 우세·
                // 13개 시간대 전원 승). 옛 식은 설계상 틀렸고 실측에서도 져서 폐기한다.
                //
                // 준비시간은 여기서 빼지 않는다 — 트럭마다 다르므로 **아래 pair 단위 판정**에서
                // 그 트럭의 p90 도착 + 안 세는 구간(learn_dispatch_lead)으로 뺀다. 작업유형 평균을
                // 쓰는 버킷 단위 축은 dispatch_deadline_ts 로 따로 있다(mig 0120·후보 풀 선정용).
                let _ = cap_j; // spread 항 폐기로 더는 마감에 쓰이지 않는다(캡은 매칭 용량으로 유지)
                deadlines.push(eta_ms);
                dep_slack_w.push(dep_slack[oi]);
                dep_tier_w.push(dep_tier[oi]);
                let this_key = (w.qc.clone(), w.vessel.clone(), w.queuename.clone());
                let wpos = matrix.len();
                // DISPATCH COST = the truck's EMPTY travel to engage the work (truck→pickup): discharge
                // pickup = the QC, load pickup = the yard block. The loaded delivery leg (block→QC for
                // load) is PRODUCTIVE work, not waste — penalising it would push idle yard trucks into
                // long empty drives to the quay (worse) and starve load work. So we minimise empty travel.
                let mut row = Vec::with_capacity(vehicles.len());
                for (vi, v) in vehicles.iter().enumerate() {
                    let (p50, p90, tier) = cost(v.1, v.2, wlat, wlon, w.jobtype == "LD");
                    let arr = v.3 + p50; // empty travel to the pickup
                    let prevk = prev.get(&v.0);
                    let switched = prevk.map(|pk| pk != &this_key).unwrap_or(false);
                    // committed window: if this truck's PRIOR work is on the verge of dispatch, switching
                    // it away is near-locked (COMMIT_LOCK_S); otherwise the normal switch penalty.
                    let switch_pen = if switched {
                        let prev_imminent = prevk.and_then(|pk| eta_by_key.get(pk)).map(|&e| e - now < COMMIT_WINDOW_MS).unwrap_or(false);
                        if prev_imminent { COMMIT_LOCK_S } else { SWITCH_PENALTY_S }
                    } else {
                        0
                    };
                    if arr < 1800 {
                        let eff = arr + switch_pen; // PURE efficiency (+ anti-thrash); urgency is Stage-1
                        edges.push((vi, wpos, eff)); // prune the far tail (never in the optimum)
                    }
                    row.push((arr, v.3 + p90, tier, switched));
                }
                matrix.push(row);
            }
            // greedy BASELINE (urgent-first, n cheapest) — computed only to measure what we'd lose;
            // NOT logged as the recommendation anymore.
            let mut used: std::collections::HashSet<usize> = std::collections::HashSet::new();
            let mut greedy_cost: i64 = 0;
            let mut greedy_n: i32 = 0;
            let mut greedy_miss: i32 = 0;
            for (wpos, _) in driving.iter().enumerate() {
                let limit = caps[wpos]; // bucket demand (already Stage-1 per-crane capped)
                if limit <= 0 {
                    continue;
                }
                let deadline = deadlines[wpos];
                let mut cand: Vec<(usize, i64)> = (0..vehicles.len())
                    .filter(|vi| !used.contains(vi))
                    .map(|vi| { let (arr, _p90, _t, sw) = matrix[wpos][vi]; (vi, arr + if sw { SWITCH_PENALTY_S } else { 0 }) })
                    .collect();
                cand.sort_by_key(|c| c.1);
                let mut taken = 0i64;
                for (vi, _pen) in cand {
                    if taken >= limit {
                        break;
                    }
                    let (arr, p90, _t, _s) = matrix[wpos][vi];
                    // mig 0122 — 마감이 work_eta 그 자체가 됐으므로, 판정에는 그 트럭의 준비시간
                    // (p90 도착 + 안 세는 구간)을 빼서 비교한다. 아래 최적 매칭 쪽과 같은 식이다.
                    let g_extra = lead_extra.get(&work[works[driving[wpos].0].0].jobtype).copied().unwrap_or(0);
                    if now + (p90 + g_extra) * 1000 > deadline {
                        greedy_miss += 1;
                    }
                    used.insert(vi);
                    taken += 1;
                    greedy_cost += arr;
                    greedy_n += 1;
                }
            }
            // STAGE 2: min-cost (pure empty-travel) optimal matching = the recommendation (logged).
            let assign = optimal_assign(vehicles.len(), &caps, &edges);
            let ts = Utc::now();
            let mut opt_cost: i64 = 0;
            let mut opt_miss: i32 = 0;
            // 이 INSERT가 조용히 실패하면 로깅만 멈추는 게 아니다 — 다음 틱의 anti-thrash가 바로 이
            // 테이블에서 직전 추천(prev)을 읽으므로 prev가 영구히 비고 switched=false가 되어
            // SWITCH_PENALTY_S/COMMIT_LOCK_S가 통째로 죽는다. sqlx::query는 런타임 바인딩이라 빌드가
            // 못 막으므로(예: 0104 미적용) 반드시 경보를 남긴다. 틱당 1줄만.
            let mut ins_err: Option<String> = None;
            let mut ins_err_n: i32 = 0;
            for &(vi, wpos) in &assign {
                let (wi, wlat, wlon, _eta) = works[driving[wpos].0];
                let w = &work[wi];
                let deadline = deadlines[wpos];
                let (arr, arr_p90, tier, switched) = matrix[wpos][vi];
                // mig 0122 — 설계 ②로 전환. 마감(= work_eta)에서 **이 트럭의 준비시간**을 뺀다:
                //   준비시간 = p90 도착(픽업 지점까지) + 안 세는 구간(learn_dispatch_lead)
                // 적하는 픽업 지점이 야드 블록이고 마감은 안벽 QC 시각이라 그 사이 적재 구간이
                // 통째로 빠져 있었다 — extra 가 그 몫이다(실측 양하 +75초 / 적하 +1,084초).
                let extra_s = lead_extra.get(&w.jobtype).copied().unwrap_or(0);
                let arrival_at = now + (arr_p90 + extra_s) * 1000;
                let slack = (deadline - arrival_at) / 1000;
                // ★ 마감을 못 맞추는 매칭도 **그대로 추천한다** — 라벨이지 필터가 아니다.
                //   실배차 전환을 앞두고 사용자가 명시적으로 결정했다(2026-08-11): "늦는 추천도
                //   실배차에 내보낸다". 종전에는 "아직 필터를 안 달았다"는 미결 상태였고, 이 줄이
                //   그 상태를 결정으로 바꾼다. 근거: 늦었다고 안 보내면 그 상자는 아무도 안 가져가
                //   더 늦어진다. 늦음은 **감추는 것이 아니라 보이게** 다룬다 — 보드가 마감 경과를
                //   🔴 칩으로, 지도 추천선이 주황으로 표시한다.
                //   ⇒ 여기에 `if !feasible { continue }` 를 넣으려는 충동이 들면 이 결정을 먼저 뒤집을 것.
                let feasible = arrival_at <= deadline;
                if !feasible {
                    opt_miss += 1;
                }
                // mig 0116 축. mig 0122 전환 후로는 위 slack 과 **같은 값**이다 — 지우지 않는 이유는
                // 전환 이전 구간을 소급 비교할 수 있는 유일한 계열이기 때문이다.
                let crane_slack = slack;
                opt_cost += arr;
                let v = &vehicles[vi];
                // deadline_slack_s / feasible = 크레인 필요 시각(work-ETA) 기준 — 정의 불변(19일치
                // 시계열 + 21일 보존이라 재정의하면 두 의미가 구분자 없이 섞인다). 출항 축은 신규
                // 컬럼(dep_slack_s / dep_tier)으로만 추가한다.
                let ins = sqlx::query(
                    "INSERT INTO stage2_match_shadow
                       (ts,tick,ytno,qc,vessel,queuename,jobtype,src_block,veh_state,arrival_s,od_p90_s,deadline_slack_s,feasible,cost_tier,switched,dest_lat,dest_lon,src_lat,src_lon,dep_slack_s,dep_tier,lead_extra_s,crane_slack_s,feasible_crane,dispatch_deadline_ts,dd_slack_s,dd_lead_s,deadline_ver,contno)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26,$27,2,$28) ON CONFLICT (ts,ytno) DO NOTHING",
                )
                .bind(ts).bind(tick as i64).bind(&v.0).bind(&w.qc).bind(&w.vessel).bind(&w.queuename)
                .bind(&w.jobtype).bind(&w.src_block).bind(v.4)
                .bind(arr as i32).bind(arr_p90 as i32).bind(slack as i32).bind(feasible).bind(tier).bind(switched)
                .bind(wlat).bind(wlon).bind(v.1).bind(v.2)
                .bind(dep_slack_w[wpos].map(|v| v.clamp(-2_000_000_000, 2_000_000_000) as i32))
                .bind(dep_tier_w[wpos] as i16)
                .bind(extra_s as i32)
                .bind(crane_slack.clamp(-2_000_000_000, 2_000_000_000) as i32)
                .bind(crane_slack >= 0)
                // mig 0120 — 설계 ② 축. 기록만 하고 판정에는 아직 안 쓴다(옛 축과 나란히 비교용).
                .bind(w.dispatch_deadline_ts)
                .bind(w.dispatch_deadline_ts.map(|d| {
                    ((d.timestamp_millis() - now) / 1000).clamp(-2_000_000_000, 2_000_000_000) as i32
                }))
                .bind(w.dd_lead_s.map(|v| v as i32))
                .bind(w.contno.clone()) // mig 0142 — 상자 단위 집행·자기 추천 이력의 키
                .execute(&pool).await;
                if let Err(e) = ins {
                    ins_err_n += 1;
                    if ins_err.is_none() {
                        ins_err = Some(e.to_string());
                    }
                }
            }
            if let Some(e) = ins_err {
                tracing::warn!(error = %e, failed = ins_err_n, of = assign.len(),
                    "stage2_match_shadow insert failed — anti-thrash(prev)도 함께 죽는다. 0104 마이그레이션 적용 여부 확인");
            }
            let gap_pct = if opt_cost > 0 { 100.0 * (greedy_cost - opt_cost) as f64 / opt_cost as f64 } else { 0.0 };
            let solver_ins = sqlx::query(
                "INSERT INTO stage2_solver_shadow (ts,tick,n_trucks,n_works,greedy_n,greedy_cost_s,optimal_n,optimal_cost_s,gap_pct,greedy_miss,optimal_miss,dep_tier_on,dep_tier0_n,dep_urgent_slots,dep_null_n,dep_demoted_n,ab_block,ab_warmup,works_raw,need_horizon_on,works_no_eta,works_no_coord,pool_new_n,pool_overlap_n,trucks_held_n,pool_overdue_n,pool_mode,due_buckets_n,self_cover_n,workpool_age_s,wake_src)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26,$27,$28,$29,$30,$31) ON CONFLICT (ts) DO NOTHING",
            )
            .bind(ts).bind(tick as i64).bind(vehicles.len() as i32).bind(driving.len() as i32)
            .bind(greedy_n).bind(greedy_cost).bind(assign.len() as i32).bind(opt_cost).bind(gap_pct as f32)
            .bind(greedy_miss).bind(opt_miss)
            // 2026-08-06 레거시 풀 제거(mig 0133) — 아래 5개는 레거시 레버 전용이라 원천이
            // 사라졌다. NULL 로 멈춘다(과거 구간만 값이 있다). dep_tier0_n/dep_null_n은 게이지라 유지.
            .bind(None::<bool>)                                                           // dep_tier_on
            .bind(driving.iter().filter(|&&(oi, _)| dep_tier[oi] == 0).count() as i32)    // dep_tier0_n (게이지 유지)
            .bind(None::<i32>)                                                            // dep_urgent_slots
            .bind(driving.iter().filter(|&&(oi, _)| dep_slack[oi].is_none()).count() as i32) // dep_null_n (게이지 유지)
            .bind(None::<i32>)                                                             // dep_demoted_n
            .bind(None::<i64>).bind(None::<bool>).bind(None::<i32>)                        // ab_block, ab_warmup, works_raw
            .bind(None::<bool>)                                                            // need_horizon_on
            .bind(works_no_eta).bind(works_no_coord)                                      // mig 0121: 조용히 빠진 작업
            .bind(driving.len() as i32).bind(None::<i32>)                                  // pool_new_n, pool_overlap_n
            .bind(trucks_held_n).bind(pool_overdue_n)
            .bind(pool_mode)
            .bind(due_buckets_n)                                                           // mig 0133
            .bind(self_cover_n)                                                            // mig 0142
            .bind(workpool_age_s)                                                          // mig 0150
            .bind(wake_src.as_str())                                                       // mig 0153
            .execute(&pool).await;
            // 생산 0 경보 (mig 0142): 트럭도 작업도 있는데 추천이 3틱 연속 0이면 매칭이 죽은
            // 것이다. 총정지는 stage2_match_shadow DEADMAN(30분)이 백스톱으로 잡지만, 이건
            // "틱은 도는데 비어 있다"를 3분 안에 잡는 빠른 경보다. 조용한 시간대(작업 0)에는
            // 조건이 성립하지 않아 오경보가 없다.
            if !vehicles.is_empty() && !driving.is_empty() && assign.is_empty() {
                zero_streak += 1;
                if zero_streak == 3 {
                    crate::db::alert(&pool, "stage2_reco", "zero_production", "crit",
                        "트럭·작업이 있는데 추천 생산이 3틱 연속 0 — 매칭이 죽었다", None).await;
                }
            } else {
                zero_streak = 0;
            }
            // mig 0121 → 0133 — 구동 풀(설계③)에 든 묶음의 상세. 레거시 풀이 사라져 in_current_pool/
            // rank_current는 NULL(비교 대상 없음).
            {
                let rank_new: HashMap<usize, i32> = driving.iter().enumerate().map(|(r, &(oi, _))| (oi, r as i32)).collect();
                for &oi in &pool_new_set {
                    let w = &work[works[oi].0];
                    let dd = w.dispatch_deadline_ts;
                    let due = dd.map(|d| {
                        let move_s = if w.jobtype == "LD" { LD_MOVE_S } else { DS_MOVE_S };
                        let cutoff = now + POOL_MARGIN_S * 1000;
                        (0..w.n.max(0) as i64)
                            .take_while(|j| d.timestamp_millis() + j * move_s * 1000 <= cutoff)
                            .count() as i32
                    });
                    let _ = sqlx::query(
                        "INSERT INTO stage2_pool_shadow
                           (ts,qc,vessel,queuename,jobtype,n,work_eta_ts,dispatch_deadline_ts,dd_slack_s,
                            due_slots,in_current_pool,in_new_pool,rank_current,rank_new,slot_idx)
                         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) ON CONFLICT DO NOTHING",
                    )
                    .bind(ts).bind(&w.qc).bind(&w.vessel).bind(&w.queuename).bind(&w.jobtype).bind(w.n)
                    .bind(w.work_eta_ts).bind(dd)
                    .bind(dd.map(|d| ((d.timestamp_millis() - now) / 1000).clamp(-2_000_000_000, 2_000_000_000) as i32))
                    .bind(due)
                    .bind(None::<bool>).bind(pool_new_set.contains(&oi))
                    .bind(None::<i32>).bind(rank_new.get(&oi).copied())
                    .bind(w.slot_idx)   // mig 0126 — 없으면 같은 구역 상자들이 한 줄로 뭉개진다
                    .execute(&pool).await;
                }
            }
            if let Err(e) = solver_ins {
                // ⚠ 새 컬럼을 더한 배포에서 psql 보다 restart 가 먼저 오면 이 INSERT 가 전건
                //   실패한다. 그런데 DEADMAN 은 stage2_match_shadow 만 보고 이 표는 안 본다
                //   (crates/api/src/db.rs) — 경보가 안 뜨니 사람이 읽는 건 이 줄뿐이다.
                //   그래서 **최근 마이그레이션 번호를 여기 같이 적는다**(0104 = 표 신설,
                //   0150 = workpool_age_s, 0153 = wake_src).
                tracing::warn!(error = %e, "stage2_solver_shadow insert failed — 마이그레이션 0104/0150/0153 적용 여부 확인");
            }
            if tick % 30 == 0 {
                crate::db::prune(&pool, "stage2_match_shadow", "DELETE FROM stage2_match_shadow WHERE ts < now() - interval '21 days'").await;
                crate::db::prune(&pool, "stage2_solver_shadow", "DELETE FROM stage2_solver_shadow WHERE ts < now() - interval '21 days'").await;
            }
        }
    });
}

/// TOS-vs-ours dispatch comparison (SHADOW). Timing-skew-free: for works TOS just assigned, we
/// reconstruct the truck pool AT the dispatch instant (T1) from `truck_pos_hist`, then — for
/// that one work — recompute OUR pick (closest available truck to the pickup) and TOS's truck arrival
/// from the SAME T1 positions. Same instant, same pool, same work → a clean 1:1 comparison.
///
/// **T1 = `live_workpool.yt_dis_ts`(= TOS `YT_DIS_DT`, 배차 시각 실물)** — 2026-08-11 절체,
/// 사용자 승인, mig 0149. 종전에는 `upd_ts`(행 마지막 갱신)를 T1 로 썼는데 그건 배차 이후의
/// 갱신에 밀린다: 실측 배차행 175건 중 90건(51.4%)이 둘이 다르고 격차 p90 2,170초(36분)였다.
/// 절반가량을 실제 배차가 아닌 순간의 트럭 위치로 비교하고 있었다는 뜻이다.
///
/// 같이 봐야 하는 자리(전부 같은 절체를 받았다): 아래 fair_compare 의 15분 창 ·
/// `workpool.rs` 의 D_tos 시드. `dispatch_pred_sample` 은 컬럼을 갈랐다(mig 0151).
///
/// 판별자: `t1_ver` (1=yt_dis_ts · 0=폴백 · NULL=경계 이전), 실제 쓴 값은 `t1_ts`(mig 0152).
/// 집계는 반드시 `t1_ver` 로 먼저 가른다.
pub fn spawn_dispatch_compare(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        loop {
            ticker.tick().await;
            if !lm.connected.load(Ordering::Relaxed) {
                continue;
            }
            // same OD cost as the Stage-2 matcher: calibrated road-network route time, Manhattan fallback.
            let rc = crate::roadgraph::RouteCost::load(&pool).await;
            let cost = |vlat: f64, vlon: f64, wlat: f64, wlon: f64, is_ld: bool| -> i64 {
                match rc.p50_p90(vlat, vlon, wlat, wlon, is_ld) {
                    Some((p50, _)) => p50 as i64,
                    None => {
                        let m = quay_manhattan_m(vlat, vlon, wlat, wlon);
                        match rc.manh_p50_p90(m, is_ld) {
                            Some((p50, _)) => p50 as i64,
                            None => (m / SEG_SPEED_MS) as i64,
                        }
                    }
                }
            };
            // truck pool snapshots, keyed by snapshot time → reconstruct T1 state.
            //
            // ★창 6분 → 60분 (2026-08-11, T1 절체와 한 쌍). T1 이 upd_ts 였을 때는 값이 늘 최근이라
            //   6분이면 닿았는데, 진짜 배차 시각으로 바꾸니 T1 이 훨씬 과거로 갔다. 실측(같은 날,
            //   비교 대상 배차행 152건의 T1 나이): 중앙 11.5분 · p90 44.9분 —
            //   6분 창은 48건(31.6%)밖에 못 덮고 나머지는 조용히 latest_pos 폴백을 탔다
            //   (스냅샷 적중 41.0% → 15.8% 실측). 자료가 없어서가 아니라 안 읽어서였다:
            //   truck_pos_hist 는 2일치를 보관한다(프룬 5377행).
            //   60분이면 147건(96.7%)을 덮고 적재는 1,380 → 17,313행/틱(실측). fair_compare 는
            //   이미 20분을 읽는다. 더 넓히면 덮는 이득이 급감한다(20분 62.5% → 60분 96.7%).
            let hist = sqlx::query_as::<_, (DateTime<Utc>, String, f64, f64, Option<String>)>(
                "SELECT ts, ytno, lat, lon, state FROM truck_pos_hist WHERE ts > now() - interval '60 minutes'",
            )
            .fetch_all(&pool).await.unwrap_or_default();
            let mut snaps: std::collections::BTreeMap<i64, Vec<(String, f64, f64, String)>> = std::collections::BTreeMap::new();
            for (t, yt, la, lo, st) in hist {
                snaps.entry(t.timestamp_millis()).or_default().push((yt, la, lo, st.unwrap_or_default()));
            }
            // each truck's MOST-RECENT position+state — fallback when the T1 snapshot doesn't contain
            // a given truck (captured a frame apart) or for assignments older than the history window.
            //
            // ★나이 상한은 **스냅샷 창과 분리**한다(2026-08-11 2차 리뷰). 위 창을 6분→60분으로
            //   넓혔더니 이 폴백 후보풀까지 같이 넓어져, 마지막 GPS 고정이 최대 60분 된 트럭이
            //   낡은 위치·낡은 상태로 후보에 들어왔다(실측 411대 중 196대 = 48%가 6분 초과).
            //   `best` 는 후보 위의 최소값이라 후보를 더하면 our_arrival 은 **단조 감소**하고
            //   delta_s 는 단조 증가한다 — 즉 **비교기가 우리 편을 드는 방향으로만** 움직인다.
            //   T1 을 되감는 데 필요한 창(60분)과 "지금 이 트럭이 어디 있나"의 유효기간(6분)은
            //   서로 다른 값이다.
            const LATEST_POS_MAX_AGE_S: i64 = 360;
            let latest_cut = Utc::now().timestamp_millis() - LATEST_POS_MAX_AGE_S * 1000;
            let mut latest_pos: HashMap<String, (f64, f64, String)> = HashMap::new();
            for (_, trucks) in snaps.range(latest_cut..) {
                for (yt, la, lo, st) in trucks {
                    latest_pos.insert(yt.clone(), (*la, *lo, st.clone()));
                }
            }
            // work-pickup coords, computed DIRECTLY from live crane GPS / learned block centroids —
            // NOT from our advisory. This is the key to completeness: every truck TOS assigned gets
            // compared, even works we never recommended for. (We run in parallel — we don't skip a
            // work just because TOS dispatched it.)
            let now = Utc::now().timestamp_millis();
            let (cranes, centroids): (HashMap<String, (f64, f64)>, HashMap<String, (f64, f64)>) = {
                let map = lm.devices.read().await;
                let cr = map.iter()
                    .filter(|(_, p)| matches!(p.cls.as_str(), "C" | "M" | "Z") && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|(id, p)| (id.clone(), (p.lat, p.lon))).collect();
                let ce = lm.centroids.read().await;
                let cem = ce.iter().map(|(k, c)| (k.clone(), (c.lat, c.lon))).collect();
                (cr, cem)
            };
            // EVERY truck TOS has assigned that we haven't compared yet (one row per bay×truck). No
            // time window → covers the whole current backlog, not just the last few minutes.
            let rows = sqlx::query_as::<_, (String, String, String, String, DateTime<Utc>, DateTime<Utc>, Option<DateTime<Utc>>, Option<String>)>(
                // ★T1 = yt_dis_ts (배차 시각 실물·mig 0148). 종전엔 upd_ts 를 T1 로 썼는데 그건 행
                //   마지막 갱신이라 배차 이후 갱신에 밀린다 — 그만큼 엉뚱한 순간의 트럭 위치로
                //   비교하게 된다. **키(tos_upd)는 그대로 둔다**: 중복 제거용 토큰으로는 여전히
                //   유효하고, 바꾸면 PK 의미가 바뀌면서 백로그 77만 행이 통째로 재비교된다.
                //   yt_dis_ts 가 비면(있으면 안 되지만) upd_ts 로 떨어져 행을 잃지 않는다.
                "SELECT DISTINCT ON (w.qc, w.queuename, w.ytno)
                        w.qc, w.queuename, w.jobtype, w.ytno, w.upd_ts,
                        coalesce(w.yt_dis_ts, w.upd_ts) AS t1, w.yt_dis_ts AS dis_raw, w.yt_topos
                   FROM live_workpool w
                  WHERE w.ytno IS NOT NULL AND w.ytno <> '' AND w.qc IS NOT NULL
                    AND w.jobtype IN ('DS','LD')
                    AND NOT EXISTS (SELECT 1 FROM dispatch_compare_shadow d
                                     WHERE d.qc=w.qc AND d.queuename=w.queuename
                                       AND d.tos_ytno=w.ytno AND d.tos_upd=w.upd_ts)
                  ORDER BY w.qc, w.queuename, w.ytno, w.upd_ts DESC",
            )
            .fetch_all(&pool).await.unwrap_or_default();
            for (qc, queue, jobtype, tos_ytno, tos_upd, tos_dis, dis_raw, yt_topos) in rows {
                // pickup coord: LD = the source block centroid, DS = the QC crane (live or learned)
                let coord = if jobtype == "LD" {
                    yt_topos.as_deref().and_then(|t| centroids.get(t).or_else(|| centroids.get(block_prefix(t))).copied())
                } else {
                    cranes.get(&qc).or_else(|| centroids.get(&qc)).copied()
                };
                let Some((dlat, dlon)) = coord else { continue };
                let t1 = tos_dis.timestamp_millis(); // 배차 시각(mig 0148) — upd_ts 아님
                // PRECISE = the snapshot ≤ T1 exists AND contains the TOS truck → tos + our pool are
                // read from that same instant (timing-skew-free). Otherwise fall back to each truck's
                // latest position (a "now-estimate", reason='now') so EVERY assignment still gets a pick.
                // ★스냅샷이 T1 보다 얼마나 일러도 되는지에 상한을 둔다(2026-08-11 2차 리뷰).
                //   창이 6분일 때는 창 자체가 상한이었는데 60분으로 넓히면서 사라졌다. GPS 피드에
                //   결손이 나면 T1 보다 수십 분 이른 스냅샷을 집고도 "timing-skew-free"로 라벨된다.
                //   최근 2일 실측은 결손 0(최대 간격 57초)이라 지금 일어나진 않지만, 이 저장소에는
                //   2026-07-16 케이블 단선 전례가 있고 그때 이 오류는 **무증상**이다.
                const T1_SNAP_MAX_SKEW_S: i64 = 180;
                let at_t1 = snaps.range(t1 - T1_SNAP_MAX_SKEW_S * 1000..=t1).next_back().map(|(_, v)| v);
                let tos_at_t1 = at_t1.and_then(|tk| tk.iter().find(|(yt, _, _, _)| *yt == tos_ytno).map(|(_, la, lo, _)| (*la, *lo)));
                let precise = tos_at_t1.is_some();
                // OUR pick = closest available (idle/soon-free) truck to the pickup. From the T1 snapshot
                // when precise, else the latest per-truck positions. Computed even if the TOS truck's own
                // GPS is currently stale (its position only affects the arrival/gap, not our pick).
                let avail: Vec<(&String, f64, f64, &str)> = if precise {
                    at_t1.unwrap().iter().map(|(yt, la, lo, st)| (yt, *la, *lo, st.as_str())).collect()
                } else {
                    latest_pos.iter().map(|(yt, (la, lo, st))| (yt, *la, *lo, st.as_str())).collect()
                };
                let mut best: Option<(String, i64)> = None;
                for (yt, la, lo, st) in avail {
                    if !matches!(st, "idle" | "soon_idle" | "wait_rtg") {
                        continue;
                    }
                    let a = cost(la, lo, dlat, dlon, jobtype == "LD");
                    if best.as_ref().map(|b| a < b.1).unwrap_or(true) {
                        best = Some((yt.clone(), a));
                    }
                }
                let Some((our_ytno, our_arrival)) = best else { continue };
                // TOS truck's arrival (for the gap) — None if its GPS is currently stale (no position)
                let tos_pos = tos_at_t1.or_else(|| latest_pos.get(&tos_ytno).map(|(la, lo, _)| (*la, *lo)));
                let tos_arrival: Option<i64> = tos_pos.map(|(tl, to)| cost(tl, to, dlat, dlon, jobtype == "LD"));
                let agree = our_ytno == tos_ytno;
                let delta: Option<i64> = tos_arrival.map(|t| t - our_arrival); // + = ours sooner
                let reason = if !precise { "now" }
                    else if agree { "same" }
                    else if tos_arrival.map(|t| our_arrival < t).unwrap_or(false) { "ours_closer" }
                    else { "tos_closer" };
                // t1_ver: 1 = T1 이 yt_dis_ts(배차 시각 실물) · 0 = upd_ts 로 폴백 ·
                // NULL = 2026-08-11 경계 이전(mig 0149).
                // ⚠판별은 **원시 컬럼의 NULL 여부**로 한다. `tos_dis == tos_upd` 로 재면
                //   틀린다 — mig 0148 실측이 두 값의 격차 **중앙 0초·61.7%가 5초 이내**라고
                //   적어뒀다. 즉 "같다"는 폴백 신호가 아니라 **정상적으로 흔한 경우**다.
                //   (이 방식으로 처음 배포했다가 11행이 폴백으로 오라벨됐다 — mig 0152 에서 정정.)
                let t1_ver: i16 = if dis_raw.is_some() { 1 } else { 0 };
                let ins = sqlx::query(
                    // t1_ts = **실제로 되감은 시각**(mig 0152). PK 는 tos_upd 라 TOS 가 UPD_DT 를
                    // 밀 때마다 같은 배차가 새 행으로 들어오는데(실측 47%가 2행 이상), 이 컬럼이
                    // 없으면 사후 중복 제거가 불가능하고 자주 갱신되는 배차가 그만큼 가중된다.
                    "INSERT INTO dispatch_compare_shadow
                       (qc,queuename,jobtype,tos_ytno,tos_arrival_s,our_ytno,our_arrival_s,agree,reason,delta_s,tos_upd,t1_ver,t1_ts)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) ON CONFLICT (qc,queuename,tos_ytno,tos_upd) DO NOTHING",
                )
                .bind(&qc).bind(&queue).bind(&jobtype).bind(&tos_ytno).bind(tos_arrival.map(|x| x as i32))
                .bind(&our_ytno).bind(our_arrival as i32).bind(agree).bind(reason).bind(delta.map(|x| x as i32)).bind(tos_upd)
                .bind(t1_ver).bind(tos_dis)
                .execute(&pool).await;
                // ⚠`let _ =` 로 버리면 마이그레이션 미적용 시 INSERT 가 **전건 조용히 실패**한다.
                // 같은 사이클의 게이지 커밋이 논증한 바로 그 유형이라 여기도 소리를 내게 한다.
                if let Err(e) = ins {
                    tracing::warn!(error = %e, "dispatch_compare_shadow INSERT 실패 (마이그레이션 미적용?)");
                }
            }
            crate::db::prune(&pool, "dispatch_compare_shadow", "DELETE FROM dispatch_compare_shadow WHERE ts < now() - interval '21 days'").await;
        }
    });
}

/// Truck position + dispatch-state history (every 30s, from live GPS — no TOS load). Powers the
/// timing-skew-free TOS-vs-ours comparison (reconstructs the truck pool at the dispatch moment).
pub fn spawn_pos_hist(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(30));
        let mut n = 0u64;
        loop {
            ticker.tick().await;
            if !lm.connected.load(Ordering::Relaxed) {
                continue;
            }
            let now = Utc::now().timestamp_millis();
            let ts = Utc::now();
            let rows: Vec<(String, f64, f64, &'static str)> = {
                let map = lm.devices.read().await;
                let plc = lm.plc.read().await;
                let centroids = lm.centroids.read().await;
                let assigned_pool = lm.assigned_pool.read().await;
                let rtgs: Vec<(f64, f64)> = map.values()
                    .filter(|p| p.cls == "RTG" && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|p| (p.lat, p.lon)).collect();
                let cranes: HashMap<String, (f64, f64)> = map.iter()
                    .filter(|(_, p)| matches!(p.cls.as_str(), "C" | "M" | "Z") && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|(id, p)| (id.clone(), (p.lat, p.lon))).collect();
                let cranes = {
                    let line = *lm.quay_line.read().await;
                    let g = lm.crane_wp.read().await;
                    resolve_crane_wp(&line, &g, &cranes)
                };
                map.iter()
                    .filter(|(_, p)| p.cls == "TT" && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|(id, p)| {
                        let c = classify_tt(p, assigned_pool.get(id), &rtgs, &plc, &cranes, &centroids, now);
                        (id.clone(), p.lat, p.lon, c.state)
                    })
                    .collect()
            };
            if rows.len() > POS_WRITE_MAX {
                // 자르지 않고 이번 틱을 통째로 거른다: 기기 모집단이 말이 안 되는 상태면 상류가
                // 고장난 것이고, 그때 일부만 골라 쓰면 어떤 기기가 빠졌는지 아무도 모른다.
                let msg = format!(
                    "truck_pos_hist 한 틱에 {}행 — 상한 {} 초과, 이번 틱 기록을 건너뛴다(기기 ID 폭주 의심)",
                    rows.len(), POS_WRITE_MAX
                );
                if crate::db::alert(&pool, "pos_write", "truck_pos_hist", "crit", &msg, None).await {
                    tracing::warn!(rows = rows.len(), "POSITION WRITE OVER CAP");
                }
                continue;
            }
            if rows.is_empty() {
                continue;
            }
            let ytnos: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
            let lats: Vec<f64> = rows.iter().map(|r| r.1).collect();
            let lons: Vec<f64> = rows.iter().map(|r| r.2).collect();
            let states: Vec<String> = rows.iter().map(|r| r.3.to_string()).collect();
            let _ = sqlx::query(
                "INSERT INTO truck_pos_hist (ts, ytno, lat, lon, state)
                 SELECT $1::timestamptz, u.ytno, u.lat, u.lon, u.state
                   FROM unnest($2::text[], $3::float8[], $4::float8[], $5::text[]) AS u(ytno, lat, lon, state)
                 ON CONFLICT (ytno, ts) DO NOTHING",
            )
            .bind(ts).bind(&ytnos).bind(&lats).bind(&lons).bind(&states)
            .execute(&pool).await;
            n += 1;
            if n % 120 == 0 {
                crate::db::prune(&pool, "truck_pos_hist", "DELETE FROM truck_pos_hist WHERE ts < now() - interval '2 days'").await;
            }
        }
    });
}

/// RTG/ES yard-crane GPS history → `rtg_pos_hist` (mig 0086). RTGs have NO PLC (unlike QC), so GPS
/// proximity is the only live "RTG engaged with this TT" signal — yet it fires for just ~16% of DS
/// drop handovers. RTG GPS was never persisted before. Stores each FRESH fix (dedup by last_seen),
/// INCLUDING stationary jitter — that scatter is exactly what the handover-detection study needs.
/// Matched offline against `rtg_move_log` (st_ts/comp_ts) ground truth. 3-day prune.
pub fn spawn_rtg_pos_hist(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(3));
        let mut last_seen: HashMap<String, i64> = HashMap::new();
        let mut n = 0u64;
        loop {
            ticker.tick().await;
            if !lm.connected.load(Ordering::Relaxed) {
                continue;
            }
            let now = Utc::now().timestamp_millis();
            let ts = Utc::now();
            // one row per NEW fix (last_seen advanced) — keeps stationary jitter, skips stale repeats
            let fixes: Vec<(String, i64, f64, f64)> = {
                let map = lm.devices.read().await;
                // hifreq 와 같은 문제이고 같은 이유로 나이 기준으로 턴다(값 자체가 시각이라
                // 추가 필드가 필요 없다). 기기 지도 존재 여부로 털면 600초 침묵-재출현이
                // 중복제거를 잃는다.
                last_seen.retain(|_, &mut ls| now - ls < 3_600_000);
                map.iter()
                    .filter(|(_, p)| matches!(p.cls.as_str(), "RTG" | "ES") && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .filter_map(|(id, p)| {
                        let fresh = last_seen.get(id).is_none_or(|&ls| p.last_seen_ms > ls);
                        fresh.then(|| (id.clone(), p.last_seen_ms, p.lat, p.lon))
                    })
                    .collect()
            };
            if fixes.len() > RTG_WRITE_MAX {
                // 자르지 않고 이번 틱을 통째로 거른다: 기기 모집단이 말이 안 되는 상태면 상류가
                // 고장난 것이고, 그때 일부만 골라 쓰면 어떤 기기가 빠졌는지 아무도 모른다.
                let msg = format!(
                    "rtg_pos_hist 한 틱에 {}행 — 상한 {} 초과, 이번 틱 기록을 건너뛴다(기기 ID 폭주 의심)",
                    fixes.len(), RTG_WRITE_MAX
                );
                if crate::db::alert(&pool, "pos_write", "rtg_pos_hist", "crit", &msg, None).await {
                    tracing::warn!(rows = fixes.len(), "POSITION WRITE OVER CAP");
                }
                continue;
            }
            for f in &fixes {
                last_seen.insert(f.0.clone(), f.1);
            }
            if fixes.is_empty() {
                continue;
            }
            let machnos: Vec<String> = fixes.iter().map(|r| r.0.clone()).collect();
            let lats: Vec<f64> = fixes.iter().map(|r| r.2).collect();
            let lons: Vec<f64> = fixes.iter().map(|r| r.3).collect();
            let _ = sqlx::query(
                "INSERT INTO rtg_pos_hist (ts, machno, lat, lon)
                 SELECT $1::timestamptz, u.machno, u.lat, u.lon
                   FROM unnest($2::text[], $3::float8[], $4::float8[]) AS u(machno, lat, lon)
                 ON CONFLICT (machno, ts) DO NOTHING",
            )
            .bind(ts).bind(&machnos).bind(&lats).bind(&lons)
            .execute(&pool).await;
            n += 1;
            if n % 200 == 0 {
                crate::db::prune(&pool, "rtg_pos_hist", "DELETE FROM rtg_pos_hist WHERE ts < now() - interval '3 days'").await;
            }
        }
    });
}

/// Phase-2 correction (mig 0088): pin the PICKUP completion (③ 픽업 떠남 = 상차 완료) from the TOS
/// crane ground truth. A crane's comp_ts is truck-relevant ONLY for LOAD-onto-truck ops = the two
/// PICKUPS: DS pickup = QC discharge (qc_move_log, jobtype DS); LD pickup = RTG load (rtg_move_log,
/// jobtype LD). (Drops are UNLOAD-from-truck → comp_ts lands the box on ship/block AFTER the truck
/// was freed, so NOT correctable here — truck-free stays GPS.) Keeps the GPS estimate (pickup_left_at)
/// and adds the truth alongside (pickup_done_at). Matched by (truck, container) via
/// tt_cycle_log.container ⨝ crane_log.contno. Every 5min over recently-dropped, still-uncorrected
/// cycles (crane data lands within ~7min). All format! args are code constants (no injection).
pub fn spawn_cycle_pickup_correct(pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(300));
        // (cycle jobtype, crane log table, crane-move jobtype, source tag)
        let jobs: [(&str, &str, &str, &str); 2] =
            [("DS", "qc_move_log", "DS", "qc"), ("LD", "rtg_move_log", "LD", "rtg")];
        loop {
            ticker.tick().await;
            for (cyc_jt, log_tbl, log_jt, src) in jobs {
                let sql = format!(
                    "WITH matched AS (
                       SELECT c.ytno, c.dropped_at,
                         -- pickup completion must sit in the pickup window: after pickup ARRIVAL
                         -- and before LADEN arrival. Guards the ~11% (DS) QC matches whose comp
                         -- lands past the laden drive (QC comp can lag / mismatch). Confirmed:
                         -- ST_DT = job-queue start (not physical); comp = physical completion.
                         (SELECT q.comp_ts FROM {log_tbl} q
                            WHERE q.trk_id = c.ytno AND q.contno = v1.container AND q.jobtype = '{log_jt}'
                              AND q.comp_ts BETWEEN COALESCE(c.empty_arrived_at, c.opened_at - interval '15 min')
                                                AND COALESCE(c.laden_arrived_at, c.dropped_at)
                            ORDER BY abs(extract(epoch FROM q.comp_ts - c.opened_at)) LIMIT 1) AS done_ts
                       FROM tt_cycle_v2 c
                       JOIN tt_cycle_log v1 ON v1.ytno = c.ytno AND v1.dropped_at = c.dropped_at
                      WHERE c.jobtype = '{cyc_jt}' AND c.pickup_done_at IS NULL AND c.opened_at IS NOT NULL
                        AND c.dropped_at > now() - interval '2 hours' AND v1.container IS NOT NULL
                     )
                     UPDATE tt_cycle_v2 c
                        SET pickup_done_at = m.done_ts, pickup_done_src = '{src}'
                       FROM matched m
                      WHERE c.ytno = m.ytno AND c.dropped_at = m.dropped_at AND m.done_ts IS NOT NULL"
                );
                match sqlx::query(&sql).execute(&pool).await {
                    Ok(r) if r.rows_affected() > 0 => {
                        tracing::info!(jobtype = cyc_jt, source = src, corrected = r.rows_affected(), "cycle pickup corrected")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(jobtype = cyc_jt, error = %e, "cycle pickup correct failed"),
                }
            }
        }
    });
}

/// High-frequency (~3s) truck GPS capture for road-network MAP INFERENCE → `truck_pos_hifreq` (mig 0067).
/// Separate from the 30s `truck_pos_hist` (whose cadence the pure-OD motion segmentation depends on).
/// Logs a truck only when it MOVED >5m since its last log (dense road trails, no parked dupes). 5-day prune.
pub fn spawn_pos_hist_hifreq(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(3));
        // (lat, lon, 마지막 기록 ms) — 시각을 함께 들고 있어야 나이 기준으로 정리할 수 있다.
        let mut last: HashMap<String, (f64, f64, i64)> = HashMap::new();
        let mut n = 0u64;
        loop {
            ticker.tick().await;
            if !lm.connected.load(Ordering::Relaxed) {
                continue;
            }
            let now = Utc::now().timestamp_millis();
            let ts = Utc::now();
            let rows: Vec<(String, f64, f64)> = {
                let map = lm.devices.read().await;
                // 이 이동-중복제거 캐시는 한 번도 정리된 적이 없어 프로세스가 본 모든 ID 가 영구
                // 누적됐다. 다만 "기기 지도에 있는가"로 털면 안 된다 — 지도는 600초 침묵이면
                // 항목을 버리는데, 단말은 정지 시 침묵하므로(reference: 멈추면 침묵) 주차·대기
                // 지점의 트럭이 그렇게 빠졌다가 돌아오면 기준점을 잃어 **안 움직였는데도 한 행을
                // 더 쓴다**. 실측 하루 14,298건(1.13%)이고 하필 정지 지점에 몰려서, 이 표를 먹는
                // 도로망 추론 래스터를 밀 수 있다. 그래서 나이로만 턴다.
                last.retain(|_, &mut (_, _, t)| now - t < 3_600_000);
                map.iter()
                    .filter(|(_, p)| p.cls == "TT" && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .filter_map(|(id, p)| {
                        let moved = last.get(id).map(|&(la, lo, _)| dist_m((la, lo), (p.lat, p.lon)) > 5.0).unwrap_or(true);
                        moved.then(|| (id.clone(), p.lat, p.lon))
                    })
                    .collect()
            };
            if rows.len() > POS_WRITE_MAX {
                // 자르지 않고 이번 틱을 통째로 거른다: 기기 모집단이 말이 안 되는 상태면 상류가
                // 고장난 것이고, 그때 일부만 골라 쓰면 어떤 기기가 빠졌는지 아무도 모른다.
                let msg = format!(
                    "truck_pos_hifreq 한 틱에 {}행 — 상한 {} 초과, 이번 틱 기록을 건너뛴다(기기 ID 폭주 의심)",
                    rows.len(), POS_WRITE_MAX
                );
                if crate::db::alert(&pool, "pos_write", "truck_pos_hifreq", "crit", &msg, None).await {
                    tracing::warn!(rows = rows.len(), "POSITION WRITE OVER CAP");
                }
                continue;
            }
            for r in &rows {
                last.insert(r.0.clone(), (r.1, r.2, now));
            }
            if rows.is_empty() {
                continue;
            }
            let ytnos: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
            let lats: Vec<f64> = rows.iter().map(|r| r.1).collect();
            let lons: Vec<f64> = rows.iter().map(|r| r.2).collect();
            let _ = sqlx::query(
                "INSERT INTO truck_pos_hifreq (ts, ytno, lat, lon)
                 SELECT $1::timestamptz, u.ytno, u.lat, u.lon
                   FROM unnest($2::text[], $3::float8[], $4::float8[]) AS u(ytno, lat, lon)
                 ON CONFLICT (ytno, ts) DO NOTHING",
            )
            .bind(ts).bind(&ytnos).bind(&lats).bind(&lons)
            .execute(&pool).await;
            n += 1;
            if n % 200 == 0 {
                crate::db::prune(&pool, "truck_pos_hifreq", "DELETE FROM truck_pos_hifreq WHERE ts < now() - interval '5 days'").await;
            }
        }
    });
}

/// FAIR head-to-head (SHADOW). Every 5 min, take a recent window of TOS dispatch DECISIONS (the
/// trucks TOS assigned + the works, each truck at its own dispatch-time position), build the
/// truck×work arrival matrix, and compare TOS's actual matching (its diagonal) to OUR solver's
/// optimal 1:1 matching (min-cost perfect matching — each truck used once, reservation respected).
/// This is the apples-to-apples efficiency comparison; unlike the per-work "closest truck" metric it
/// can NOT double-book the nearest truck, so it tells the true empty-travel saving over TOS.
/// xorshift64* — a shuffle and a few random permutations do not warrant a new dependency, and a
/// fixed algorithm keeps the result reproducible from a logged seed if a number ever needs re-checking.
fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Fisher-Yates, in place.
fn shuffle<T>(v: &mut [T], rng: &mut u64) {
    for i in (1..v.len()).rev() {
        let j = (xorshift(rng) % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
}

pub fn spawn_fair_compare(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        const WINDOW_MIN: i64 = 15;
        const MAX_N: usize = 120; // bound the assignment problem (keeps the solve sub-second)
        /// Random permutations averaged per batch to get the "coin-flip assignment" baseline.
        const RAND_TRIALS: usize = 8;
        let mut ticker = tokio::time::interval(Duration::from_secs(300));
        loop {
            ticker.tick().await;
            if !lm.connected.load(Ordering::Relaxed) {
                continue;
            }
            let now = Utc::now().timestamp_millis();
            // same OD cost as the Stage-2 matcher: calibrated road-network route time, Manhattan fallback.
            let rc = crate::roadgraph::RouteCost::load(&pool).await;
            let cost = |vlat: f64, vlon: f64, wlat: f64, wlon: f64, is_ld: bool| -> i64 {
                match rc.p50_p90(vlat, vlon, wlat, wlon, is_ld) {
                    Some((p50, _)) => p50 as i64,
                    None => {
                        let m = quay_manhattan_m(vlat, vlon, wlat, wlon);
                        match rc.manh_p50_p90(m, is_ld) {
                            Some((p50, _)) => p50 as i64,
                            None => (m / SEG_SPEED_MS) as i64,
                        }
                    }
                }
            };
            // truck positions over the window → per-truck position nearest its dispatch instant
            let hist = sqlx::query_as::<_, (DateTime<Utc>, String, f64, f64)>(
                "SELECT ts, ytno, lat, lon FROM truck_pos_hist WHERE ts > now() - interval '20 minutes'",
            )
            .fetch_all(&pool).await.unwrap_or_default();
            let mut snaps: std::collections::BTreeMap<i64, Vec<(String, f64, f64)>> = std::collections::BTreeMap::new();
            let mut latest_pos: HashMap<String, (f64, f64)> = HashMap::new();
            for (t, yt, la, lo) in hist {
                snaps.entry(t.timestamp_millis()).or_default().push((yt.clone(), la, lo));
                latest_pos.insert(yt, (la, lo));
            }
            let (cranes, centroids): (HashMap<String, (f64, f64)>, HashMap<String, (f64, f64)>) = {
                let map = lm.devices.read().await;
                let cr = map.iter()
                    .filter(|(_, p)| matches!(p.cls.as_str(), "C" | "M" | "Z") && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S)
                    .map(|(id, p)| (id.clone(), (p.lat, p.lon))).collect();
                let ce = lm.centroids.read().await;
                let cem = ce.iter().map(|(k, c)| (k.clone(), (c.lat, c.lon))).collect();
                (cr, cem)
            };
            // the window's TOS decisions (distinct bay×truck, most recent first, capped)
            // ★창·정렬·T1 전부 yt_dis_ts(배차 시각 실물·mig 0148) 기준. 종전엔 upd_ts 라, 배차는
            //   30분 전인데 무관한 갱신으로 UPD_DT 만 밀린 행이 "방금 배차"로 창에 들어왔다.
            let rows = sqlx::query_as::<_, (String, String, String, String, DateTime<Utc>, Option<String>)>(
                "SELECT DISTINCT ON (w.qc, w.queuename, w.ytno) w.qc, w.queuename, w.jobtype, w.ytno,
                        coalesce(w.yt_dis_ts, w.upd_ts) AS t1, w.yt_topos
                   FROM live_workpool w
                  WHERE w.ytno IS NOT NULL AND w.ytno <> '' AND w.qc IS NOT NULL AND w.jobtype IN ('DS','LD')
                    AND coalesce(w.yt_dis_ts, w.upd_ts) > now() - interval '15 minutes'
                  ORDER BY w.qc, w.queuename, w.ytno, coalesce(w.yt_dis_ts, w.upd_ts) DESC LIMIT 400",
            )
            .fetch_all(&pool).await.unwrap_or_default();
            // Group TOS assignments by the snapshot instant just before each. Only trucks that were
            // idle at the SAME instant form a valid pool to re-match — pooling trucks across different
            // times would hand the optimum an unreal pick (a truck that was only free at another moment).
            // Truck position = its position in THAT snapshot, so every batch is instant-consistent.
            #[allow(clippy::type_complexity)]
            let mut groups: HashMap<
                i64,
                Vec<((f64, f64), (f64, f64), String, String, String, String, DateTime<Utc>)>,
            > = HashMap::new();
            let mut considered = 0usize;
            // Shuffle BEFORE the cap. The rows arrive ordered by (qc, queuename, ytno) for the
            // DISTINCT ON, so taking the first MAX_N dropped whichever cranes sort late in the
            // alphabet — every single time. Measured 2026-07-31: 1,743 of 1,924 ticks (91%) hit the
            // cap, so that truncation was not a rare edge, it was the normal case and it always
            // removed the same cranes. A random sample of the same size is unbiased.
            let mut rng: u64 = (now as u64) | 1; // xorshift needs a non-zero seed
            let mut rows = rows;
            shuffle(&mut rows, &mut rng);
            for (qc, queue, jobtype, ytno, upd, yt_topos) in rows {
                if considered >= MAX_N {
                    break;
                }
                let wc = if jobtype == "LD" {
                    yt_topos.as_deref().and_then(|t| centroids.get(t).or_else(|| centroids.get(block_prefix(t))).copied())
                } else {
                    cranes.get(&qc).or_else(|| centroids.get(&qc)).copied()
                };
                let Some(wc) = wc else { continue };
                let t1 = upd.timestamp_millis();
                // truck position at ~T1 (snapshot ≤ T1 with the truck, else its latest fix)
                let tp = snaps.range(..=t1).next_back()
                    .and_then(|(_, v)| v.iter().find(|(yt, _, _)| *yt == ytno).map(|(_, la, lo)| (*la, *lo)))
                    .or_else(|| latest_pos.get(&ytno).copied());
                let Some(tp) = tp else { continue };
                // group into 60-second buckets ≈ "the same operational moment" — trucks dispatched
                // within the same minute are an ~simultaneous pool to re-match (no cross-time pooling).
                let bucket = t1 / 60_000;
                groups.entry(bucket).or_default().push((tp, wc, jobtype, qc, ytno, queue, upd));
                considered += 1;
            }
            // per-instant optimal permutation (min-cost perfect matching), summed across instants.
            // Also record per-pair detail: each truck's TOS cost (diagonal) vs our re-matched cost,
            // tagged by its TOS work's jobtype/crane → for breakdown + bias analysis.
            let mut n = 0i32;
            let mut tos_total = 0i64;
            let mut our_total = 0i64;
            // Coin-flip baseline over the SAME pool. Without it `savings_pct` is uninterpretable:
            // the identity permutation (= what TOS actually did) is always a feasible solution, so
            // the min-cost matching can never cost more and the "saving" can never be negative
            // (measured: 0 negatives in 1,924 ticks, min +0.027%). Random gives the third point that
            // makes the number mean something — random >= TOS >= optimal, so (random-TOS)/(random-optimal)
            // is the share of the achievable range TOS already captures.
            let mut rand_total = 0i64;
            let mut same = 0i32;
            #[allow(clippy::type_complexity)]
            // (jobtype, qc, tos_s, our_s, ytno, queuename, dispatch_ts)
            let mut det: Vec<(String, String, i32, i32, String, String, DateTime<Utc>)> = Vec::new();
            for batch in groups.values() {
                let m = batch.len();
                if m == 0 {
                    continue;
                }
                n += m as i32;
                let tos_each: Vec<i64> = batch
                    .iter()
                    .map(|(tp, wc, jt, ..)| cost(tp.0, tp.1, wc.0, wc.1, jt == "LD"))
                    .collect();
                for &c in &tos_each {
                    tos_total += c;
                }
                // σ(i) = the work our matching gives truck i (identity for singletons/infeasible)
                let mut sigma: Vec<usize> = (0..m).collect();
                if m > 1 {
                    let (s, t) = (0usize, 2 * m + 1);
                    let mut g = Mcmf::new(2 * m + 2);
                    for i in 0..m {
                        g.add(s, 1 + i, 1, 0);
                        g.add(1 + m + i, t, 1, 0);
                    }
                    for i in 0..m {
                        for j in 0..m {
                            let c = cost(batch[i].0 .0, batch[i].0 .1, batch[j].1 .0, batch[j].1 .1, batch[j].2 == "LD");
                            g.add(1 + i, 1 + m + j, 1, c);
                        }
                    }
                    let (_gc, flow) = g.run(s, t);
                    if (flow as usize) >= m {
                        for i in 0..m {
                            for &e in &g.head[1 + i] {
                                let v = g.to[e];
                                if v >= 1 + m && v < 1 + 2 * m && g.cap[e] == 0 {
                                    sigma[i] = v - (1 + m);
                                    break;
                                }
                            }
                        }
                    }
                }
                // random baseline: average over RAND_TRIALS shuffles of the same pool
                if m > 1 {
                    let mut acc = 0i64;
                    let mut perm: Vec<usize> = (0..m).collect();
                    for _ in 0..RAND_TRIALS {
                        shuffle(&mut perm, &mut rng);
                        for i in 0..m {
                            acc += cost(
                                batch[i].0 .0, batch[i].0 .1,
                                batch[perm[i]].1 .0, batch[perm[i]].1 .1,
                                batch[perm[i]].2 == "LD",
                            );
                        }
                    }
                    rand_total += acc / RAND_TRIALS as i64;
                } else {
                    rand_total += tos_each[0]; // a single pair has only one permutation
                }
                for i in 0..m {
                    let our_s = cost(batch[i].0 .0, batch[i].0 .1, batch[sigma[i]].1 .0, batch[sigma[i]].1 .1, batch[sigma[i]].2 == "LD");
                    our_total += our_s;
                    if sigma[i] == i {
                        same += 1;
                    }
                    det.push((
                        batch[i].2.clone(), batch[i].3.clone(), tos_each[i] as i32, our_s as i32,
                        batch[i].4.clone(), batch[i].5.clone(), batch[i].6,
                    ));
                }
            }
            if n < 4 {
                continue;
            }
            let savings = if tos_total > 0 { 100.0 * (tos_total - our_total) as f64 / tos_total as f64 } else { 0.0 };
            let _ = sqlx::query(
                "INSERT INTO fair_compare_shadow (window_min, n, tos_total_s, our_total_s, savings_pct, same_n, rand_total_s)
                 VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (ts) DO NOTHING",
            )
            .bind(WINDOW_MIN as i32).bind(n as i32).bind(tos_total).bind(our_total).bind(savings).bind(same).bind(rand_total)
            .execute(&pool).await;
            crate::db::prune(&pool, "fair_compare_shadow", "DELETE FROM fair_compare_shadow WHERE ts < now() - interval '21 days'").await;
            // per-pair detail for breakdown/bias analysis (bulk insert via UNNEST)
            if !det.is_empty() {
                let jts: Vec<String> = det.iter().map(|d| d.0.clone()).collect();
                let qcs: Vec<String> = det.iter().map(|d| d.1.clone()).collect();
                let toss: Vec<i32> = det.iter().map(|d| d.2).collect();
                let ours: Vec<i32> = det.iter().map(|d| d.3).collect();
                let yts: Vec<String> = det.iter().map(|d| d.4.clone()).collect();
                let qns: Vec<String> = det.iter().map(|d| d.5.clone()).collect();
                let dts: Vec<DateTime<Utc>> = det.iter().map(|d| d.6).collect();
                // ON CONFLICT: this runs every 5 minutes over a 15-minute window, so the same
                // dispatch is seen by ~3 consecutive ticks. Counting it 3x inflated the sample and
                // its weight in every breakdown (measured: 34,075 rows / 288 ticks in 24h against a
                // 120-row cap). The dedupe key keeps the first tick that scored it.
                let _ = sqlx::query(
                    "INSERT INTO fair_compare_detail (jobtype, qc, tos_s, our_s, ytno, queuename, dispatch_ts)
                     SELECT * FROM UNNEST($1::text[], $2::text[], $3::int[], $4::int[], $5::text[], $6::text[], $7::timestamptz[])
                     ON CONFLICT (ytno, queuename, dispatch_ts) WHERE ytno IS NOT NULL DO NOTHING",
                )
                .bind(&jts).bind(&qcs).bind(&toss).bind(&ours).bind(&yts).bind(&qns).bind(&dts)
                .execute(&pool).await;
                crate::db::prune(&pool, "fair_compare_detail", "DELETE FROM fair_compare_detail WHERE ts < now() - interval '7 days'").await;
            }
        }
    });
}

/// 배정 알고리즘(2단계 비용행렬 매칭)의 회귀 테스트.
///
/// 이 함수는 "어느 트럭을 어느 작업에 보낼지"를 실제로 정하는 곳인데 테스트가 0건이었다.
/// 그림자인 동안에는 틀려도 기록만 지저분해지지만, 실배차로 올리면 **틀린 배정이 그대로
/// 현장 지시**가 된다. 고칠 때 실수를 잡아줄 안전망으로 최소한을 고정한다.
///
/// 비용 단위는 초(트럭이 자유로워지는 시각 + 공차 주행). 값 자체가 아니라 **성질**을 고정한다 —
/// 상수를 바꾸면 깨지는 테스트는 상수를 바꿀 때마다 무의미하게 깨지기 때문이다.
#[cfg(test)]
mod optimal_assign_tests {
    use super::optimal_assign;

    /// 배정 결과의 총비용. 간선 목록에서 되짚어 잰다.
    fn cost_of(assign: &[(usize, usize)], edges: &[(usize, usize, i64)]) -> i64 {
        assign
            .iter()
            .map(|&(t, b)| {
                edges
                    .iter()
                    .find(|&&(u, v, _)| u == t && v == b)
                    .map(|&(_, _, c)| c)
                    .expect("배정된 쌍에 대응하는 간선이 없다")
            })
            .sum()
    }

    /// ★핵심 — 눈앞의 최선(탐욕)이 전체 최선이 아닌 고전 사례를 고정한다.
    ///
    /// 트럭0 은 작업0 이 가깝고(10), 트럭1 은 작업0 밖에 갈 수 없다(20).
    /// 탐욕이 트럭0 에게 작업0 을 주면 트럭1 은 갈 곳이 없어 작업1 이 빈다.
    /// 최적은 트럭0 을 작업1(30) 로 보내고 트럭1 에게 작업0(20) 을 줘 **둘 다** 채운다.
    /// 이 성질이 깨지면 매칭이 탐욕으로 퇴화한 것이다.
    #[test]
    fn 탐욕이_지는_사례에서_둘_다_채운다() {
        let edges = [(0, 0, 10), (0, 1, 30), (1, 0, 20)];
        let assign = optimal_assign(2, &[1, 1], &edges);
        assert_eq!(assign.len(), 2, "두 작업을 모두 채워야 한다: {assign:?}");
        assert!(assign.contains(&(1, 0)), "트럭1 은 작업0 밖에 못 간다: {assign:?}");
        assert!(assign.contains(&(0, 1)), "트럭0 이 양보해야 둘 다 찬다: {assign:?}");
        assert_eq!(cost_of(&assign, &edges), 50);
    }

    /// 선택지가 겹치지 않으면 각자 가장 싼 곳으로 간다(기본 동작).
    ///
    /// ⚠ 간선을 **비싼 것부터** 넣는다. 싼 것부터 넣으면 비용을 통째로 무시하는 구현도
    /// 간선 순서 운으로 통과한다 — 실제로 돌연변이 시험에서 그렇게 살아남았다(2026-08-11).
    /// 이 순서라면 비용을 안 보는 구현은 반드시 170 을 내놓는다.
    #[test]
    fn 같은_수를_배정하더라도_더_싼_쪽을_고른다() {
        let edges = [(0, 1, 90), (0, 0, 10), (1, 0, 80), (1, 1, 20)];
        let assign = optimal_assign(2, &[1, 1], &edges);
        assert_eq!(assign.len(), 2);
        assert_eq!(
            cost_of(&assign, &edges),
            30,
            "10+20=30 이어야 한다. 170 이면 비용을 안 보고 간선 순서대로 집은 것이다: {assign:?}"
        );
    }

    /// 트럭이 모자라면 **가장 싼 조합**을 고른다 — 비용을 무시하면 여기서 갈린다.
    /// 트럭 1대가 갈 수 있는 두 작업의 비용 차가 크고, 비싼 쪽 간선을 먼저 넣었다.
    #[test]
    fn 트럭이_모자라면_가장_싼_작업을_고른다() {
        let edges = [(0, 0, 900), (0, 1, 10)];
        let assign = optimal_assign(1, &[1, 1], &edges);
        assert_eq!(assign, vec![(0, 1)], "싼 작업1(10)을 두고 작업0(900)을 골랐다: {assign:?}");
    }

    /// 작업 묶음의 수요(cap)를 넘겨 보내지 않는다. 넘기면 크레인 한 대에 트럭이 몰린다.
    #[test]
    fn 묶음_수요를_초과해_보내지_않는다() {
        let edges = [(0, 0, 10), (1, 0, 11), (2, 0, 12)];
        let assign = optimal_assign(3, &[2], &edges);
        assert_eq!(assign.len(), 2, "수요가 2인데 {}대를 보냈다", assign.len());
        // 남길 트럭은 가장 비싼 트럭2 여야 한다
        assert!(!assign.iter().any(|&(t, _)| t == 2), "더 싼 트럭을 두고 비싼 트럭을 보냈다: {assign:?}");
    }

    /// 갈 수 있는 작업이 없는 트럭은 배정되지 않는다(억지로 내보내지 않는다).
    #[test]
    fn 간선이_없는_트럭은_남는다() {
        let assign = optimal_assign(2, &[1], &[(0, 0, 10)]);
        assert_eq!(assign, vec![(0, 0)]);
    }

    /// 빈 입력에서 죽지 않는다 — 한산한 시간대에 실제로 들어오는 값이다.
    #[test]
    fn 빈_입력은_빈_결과다() {
        assert!(optimal_assign(0, &[1, 2], &[]).is_empty());
        assert!(optimal_assign(3, &[], &[]).is_empty());
        assert!(optimal_assign(3, &[0, 0], &[(0, 0, 5)]).is_empty(), "수요 0 인 묶음에 보내면 안 된다");
    }

    /// 한 트럭이 두 작업에 동시에 배정되지 않는다(cap 1 층이 살아 있는지).
    #[test]
    fn 한_트럭은_한_작업만_받는다() {
        let assign = optimal_assign(1, &[5, 5], &[(0, 0, 10), (0, 1, 11)]);
        assert_eq!(assign.len(), 1, "트럭 하나가 두 곳에 갔다: {assign:?}");
    }
}

#[cfg(test)]
mod workpool_freshness_tests {
    use super::{workpool_stale_reason, WORKPOOL_MAX_AGE_S};

    /// 정상 대역을 통과시킨다. 2026-08-11 실측 톱니(10초 간격 9회)를 그대로 넣는다 —
    /// 임계를 잘못 낮추면 여기가 먼저 깨진다.
    #[test]
    fn 실측_정상_대역은_통과한다() {
        for age in [61, 12, 22, 32, 42, 52, 62, 12, 22] {
            assert!(
                workpool_stale_reason(Ok((Some(age), Some(age + 5)))).is_none(),
                "정상 대역 {age}초를 낡음으로 판정했다 — 매칭이 평시에 선다"
            );
        }
    }

    /// 경계: 임계 자체는 통과, 1초만 넘겨도 막는다.
    #[test]
    fn 임계_경계에서_뒤집힌다() {
        assert!(workpool_stale_reason(Ok((Some(WORKPOOL_MAX_AGE_S), None))).is_none());
        assert!(workpool_stale_reason(Ok((Some(WORKPOOL_MAX_AGE_S + 1), None))).is_some());
    }

    /// 이 게이트가 존재하는 이유 그 자체 — 추출이 죽어 목록이 낡으면 매칭을 막는다.
    /// 사유 문자열에 실제 나이가 들어가야 장애 때 로그만 보고 원인을 안다.
    #[test]
    fn 추출이_죽으면_막고_나이를_사유에_적는다() {
        let why = workpool_stale_reason(Ok((Some(1800), Some(1805)))).expect("막아야 한다");
        assert!(why.contains("1800"), "사유에 경과초가 없다: {why}");
    }

    /// 판정 불능은 전부 "낡음"으로 닫는다 — 모를 때 멈추는 쪽이 안전하다.
    /// 신선도 행이 없는 경우와 조회 실패를 각각 고정한다(둘 다 과거에 통과시키던 형태다).
    #[test]
    fn 판정_불능은_막는_쪽으로_닫힌다() {
        assert!(workpool_stale_reason(Ok((None, Some(10)))).is_some(), "신선도 행이 없으면 막아야 한다");
        assert!(workpool_stale_reason(Err("연결 끊김".into())).is_some(), "조회 실패면 막아야 한다");
    }

    /// 표가 비어도 신선도 기록이 최신이면 통과 — "진짜로 일이 없다"와 "추출이 죽었다"는
    /// 다른 상태다. 이 둘을 뭉개면 한산한 시간대마다 헛경보가 난다.
    #[test]
    fn 일이_없어_표가_비었을_뿐이면_통과한다() {
        assert!(workpool_stale_reason(Ok((Some(30), None))).is_none());
    }
}

#[cfg(test)]
mod wake_on_landing_tests {
    use super::{should_wake, WakeSrc, WakeStep, PREV_WINDOW_S, WAKE_MAX_WAIT_MS, WORKPOOL_MAX_AGE_S};
    use chrono::{DateTime, TimeZone, Utc};
    use std::time::Duration;

    fn 시각(sec: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 12, 12, 34, sec).unwrap()
    }
    const 잠깐: Duration = Duration::from_millis(0);
    const 최대대기: Duration = Duration::from_millis(WAKE_MAX_WAIT_MS);

    /// 이 변경의 전부다: **새 목록이 착지하면 그때 깨어난다.**
    ///
    /// 인자 뒤바꿈(직전↔이번)도 여기서 잡힌다 — 뒤바꾸면 `landed > seen` 이 거짓이 되어
    /// KeepWaiting 으로 무너진다.
    #[test]
    fn 새_착지가_오면_깨어난다() {
        assert_eq!(
            should_wake(Some(시각(10)), Some(시각(11)), 잠깐),
            WakeStep::Wake(WakeSrc::Landing)
        );
    }

    /// 같은 착지를 두 번 쓰지 않는다. 이게 무너지면 매칭이 폴링 간격(2초)마다 도는
    /// 폭주 루프가 된다 — 라이브에서 제일 비싼 실패 방향이다.
    #[test]
    fn 같은_착지로는_다시_깨어나지_않는다() {
        assert_eq!(should_wake(Some(시각(10)), Some(시각(10)), 잠깐), WakeStep::KeepWaiting);
    }

    /// 착지 시각이 **뒤로 가도** 깨우지 않는다(시계 되돌림·행 재기입).
    #[test]
    fn 착지가_뒤로_가면_깨우지_않는다() {
        assert_eq!(should_wake(Some(시각(20)), Some(시각(10)), 잠깐), WakeStep::KeepWaiting);
    }

    /// 새 목록이 영영 안 와도 최대 대기를 채우면 깨어난다. 이게 없으면 추출이 죽었을 때
    /// 매칭이 **조용히 멈춘다** — 경보도 게이지도 없이.
    #[test]
    fn 최대_대기를_채우면_폴백으로_깨어난다() {
        assert_eq!(
            should_wake(Some(시각(10)), Some(시각(10)), 최대대기),
            WakeStep::Wake(WakeSrc::Fallback)
        );
        // 경계 바로 앞에서는 아직 기다린다.
        assert_eq!(
            should_wake(Some(시각(10)), Some(시각(10)), 최대대기 - Duration::from_millis(1)),
            WakeStep::KeepWaiting
        );
    }

    /// 착지 시각을 **모를 때**(조회 실패·data_freshness 행 없음)는 깨우지 않는다.
    /// 그대로 폴백까지 기다리면 신선도 게이트가 "판정 불능 = 낡음"으로 닫는다.
    #[test]
    fn 착지_시각을_모르면_폴백까지_기다린다() {
        assert_eq!(should_wake(Some(시각(10)), None, 잠깐), WakeStep::KeepWaiting);
        assert_eq!(
            should_wake(Some(시각(10)), None, 최대대기),
            WakeStep::Wake(WakeSrc::Fallback)
        );
    }

    /// 기동 직후 **착지 시각을 읽을 수 있으면** 즉시 돈다. 이 틱은 `startup` 이지 `landing` 이
    /// 아니다 — 목록 나이가 임의라 landing 집계에 섞이면 p99 가 오염된다.
    #[test]
    fn 기동_직후_첫_회는_startup_으로_즉시_돈다() {
        assert_eq!(should_wake(None, Some(시각(10)), 잠깐), WakeStep::Wake(WakeSrc::Startup));
    }

    /// ★회귀 방지: 기준선도 착지도 없으면 **즉시 깨우면 안 된다.**
    ///
    /// 여기서 `Wake` 를 내면 호출부가 `seen` 을 전진시킬 값이 없어 영원히 `None` 으로 남고,
    /// 매 회차가 첫 폴에서 즉시 반환되는데 바깥 루프의 `continue` 경로에는 sleep 이 없다
    /// ⇒ **sleep 없는 폭주**(DB 질의 초당 수천 건). 2026-08-12 리뷰에서 잡힌 실제 결함이고,
    /// 그때 이 자리의 테스트가 오히려 폭주를 사양으로 못 박고 있었다.
    /// 도달 가능한 상태다: `data_freshness.last_success_at` 은 nullable 이고 추출기가 첫
    /// 실행 실패 시 NULL 로 넣는다.
    #[test]
    fn 착지를_모르면_기동_때도_즉시_깨우지_않는다() {
        assert_eq!(should_wake(None, None, 잠깐), WakeStep::KeepWaiting);
        // 폴백까지는 기다렸다가 깨어난다 — 그래야 신선도 게이트가 정상적으로 닫는다.
        assert_eq!(should_wake(None, None, 최대대기), WakeStep::Wake(WakeSrc::Fallback));
    }

    /// 폴백은 **정상 운전에서 울리면 안 되는** 하트비트다. 두 경계를 못 박는다.
    ///
    /// 60초였을 때 실제로 무너졌다(라이브 2시간): 착지 간격 중앙 66초라 폴백이 착지 6초 전에
    /// 먼저 터져 틱의 42%가 폴백이 됐고, 그 낡은 판단이 직후의 신선한 판단을 안티스래시로
    /// 밀어냈다. 이 테스트는 그 값으로 되돌리는 것을 막는다.
    #[test]
    fn 폴백_대기는_관측된_착지_간격보다_뒤에_있다() {
        // 착지 간격 실측(etl_run_log, kpi_key=WORKPOOL·status=OK).
        // **24시간 창**(n=1,413): p50 60.0 · p90 66.2 초.
        // **최악 1시간 창**(2026-08-12 00:00Z): p50 83.8 · p90 136.3 · 최대 238.3 초.
        // 여기서는 최악 창 쪽을 기준으로 잡는다 — 그 시간대에도 폴백이 착지를 앞지르면 안 된다.
        const 관측_착지간격_p90_초: u64 = 140; // 최악 1시간 창 기준(24시간 창이면 67)
        let 대기_초 = WAKE_MAX_WAIT_MS / 1000;
        assert!(
            대기_초 > 관측_착지간격_p90_초,
            "폴백 대기 {대기_초}초가 관측 착지 간격 p90 {관측_착지간격_p90_초}초보다 앞이다 — 정상 운전에서 폴백이 착지를 앞지른다"
        );
        // 게이트(300초) 전에 하트비트가 **최소 한 번은** 남아야 한다. `대기 < 300` 만으로는
        // 부족하다 — 299초도 통과하는데 그 값에서는 0번 남는다(2차 리뷰 지적).
        assert!(
            대기_초 * 2 <= WORKPOOL_MAX_AGE_S as u64,
            "폴백 대기 {대기_초}초 × 2 가 신선도 게이트 {WORKPOOL_MAX_AGE_S}초를 넘는다 — 게이트가 닫히기 전에 하트비트가 한 번도 안 남을 수 있다"
        );
    }

    /// ★안티스래시 창은 하트비트보다 **넓어야** 한다.
    ///
    /// 같으면(첫 판에서 둘 다 150초였다) 하트비트 틱에서 직전 틱의 행이 항상 창 밖이라
    /// `prev` 가 비고, `switched` 가 전부 false 가 되어 전환 벌점이 통째로 꺼진다.
    /// ⇒ **가장 낡은 목록으로 도는 틱이 유일하게 제동 없이 전 트럭을 재배정하는 틱**이 된다.
    #[test]
    fn 안티스래시_창은_하트비트보다_넓다() {
        assert!(
            PREV_WINDOW_S > WAKE_MAX_WAIT_MS / 1000,
            "prev 창 {PREV_WINDOW_S}초가 하트비트 {}초 이하다 — 하트비트 틱에서 안티스래시가 꺼진다",
            WAKE_MAX_WAIT_MS / 1000
        );
    }

    /// ★폭주 방지 불변식: **깨어나면 반드시 `landed` 가 있거나(= 호출부가 `seen` 을 전진시킨다)
    /// 최대 대기를 채웠다(= 그 자체가 속도 제한이다).**
    ///
    /// 실제로 폭주를 막는 것은 호출부의 `if landed.is_some() { *seen = landed }` 인데 그건
    /// 순수 함수 밖이라 테스트가 못 본다(2차 리뷰 지적). 그래서 성질 쪽을 못 박는다 —
    /// 이 성질을 깨는 Wake 팔을 새로 붙이면 여기서 걸린다.
    #[test]
    fn 깨어나면_seen_이_전진하거나_최대_대기를_채운다() {
        let 최대 = Duration::from_millis(WAKE_MAX_WAIT_MS);
        for seen in [None, Some(시각(10))] {
            for landed in [None, Some(시각(5)), Some(시각(10)), Some(시각(20))] {
                for waited in [잠깐, 최대 - Duration::from_millis(1), 최대] {
                    if let WakeStep::Wake(src) = should_wake(seen, landed, waited) {
                        assert!(
                            landed.is_some() || waited >= 최대,
                            "{src:?} 가 seen 전진도 최대 대기도 없이 깨웠다 (seen={seen:?} landed={landed:?} waited={waited:?}) — 대기 루프가 sleep 없이 폭주한다"
                        );
                    }
                }
            }
        }
    }

    /// DB 에 적히는 문자열을 못 박는다. 오타 하나면 판별자가 통째로 거짓말을 하는데,
    /// 값은 21일 남는다.
    #[test]
    fn 판별자_문자열이_마이그레이션과_같다() {
        assert_eq!(WakeSrc::Startup.as_str(), "startup");
        assert_eq!(WakeSrc::Landing.as_str(), "landing");
        assert_eq!(WakeSrc::Fallback.as_str(), "fallback");
    }
}

#[cfg(test)]
mod gate_tests {
    use super::{gate_fix, FixGate};

    fn kept(cls: &str, lat: f64, lon: f64) -> bool {
        !matches!(gate_fix(cls, lat, lon), FixGate::Drop(_))
    }

    /// Pins the gate to MEASURED data per population. The first version of this gate applied a
    /// TT-derived radius to every class and dropped live H-* hauliers at 25.66km; the cases below
    /// exist so that cannot happen again. Widening TT_MAX_R_M until the TT rejects pass, or
    /// making the non-TT cases drop, would each re-open a real outage.
    #[test]
    fn tt_is_bounded_by_its_own_measured_range() {
        // 99.9975% of 6.33M TT fixes are within 5km (max 4,959m); p99.99 = 3,032m
        assert!(kept("TT", 2.9052, 101.2789));
        assert!(kept("TT", 2.9510, 101.3064));
        assert!(kept("TT", 2.928, 101.2927));
        // the corrupt TT fixes from the 2026-07-28 window (89km..219km)
        assert!(!kept("TT", 4.1975, 99.7827), "the fix that OOM-killed the box must be dropped");
        assert!(!kept("TT", 1.9081, 101.0023));
        assert!(!kept("TT", 3.6898, 101.5460));
    }

    #[test]
    fn unmeasured_classes_are_never_dropped_for_distance() {
        // real live H-* external hauliers at 25.66km — the regression this test exists for
        for (la, lo) in [
            (2.989742, 101.51491),
            (2.989817, 101.515058),
            (2.98935, 101.514427),
            (2.9898, 101.515),
            (2.9902, 101.5155),
            (2.990667, 101.516),
        ] {
            assert!(kept("H", la, lo), "live haulier fix dropped: {la},{lo}");
        }
        // far out is flagged for measurement, still kept
        assert!(matches!(gate_fix("H", 4.1975, 99.7827), FixGate::KeepButFar(_)));
        // terminal equipment classes are not bounded either — no footprint measured for them
        assert!(kept("RT", 2.99, 101.52));
        assert!(kept("C", 2.99, 101.52));
        assert!(kept("ES", 2.99, 101.52));
    }

    #[test]
    fn non_finite_is_dropped_for_every_class() {
        for cls in ["TT", "H", "RT", "C", ""] {
            assert!(!kept(cls, f64::NAN, 101.2927), "NaN kept for {cls}");
            assert!(!kept(cls, 2.928, f64::INFINITY), "Inf kept for {cls}");
            assert!(!kept(cls, f64::NEG_INFINITY, f64::NAN));
        }
    }
}

#[cfg(test)]
mod pool_tests {
    use super::*;
    use chrono::TimeZone;

    fn t(sec: i64) -> DateTime<Utc> { Utc.timestamp_opt(1_780_000_000 + sec, 0).unwrap() }
    fn sig(free: Option<i64>, picked: Option<i64>, dis: Option<i64>, listed: Option<i64>) -> TosSig {
        TosSig { free: free.map(t), dis: dis.map(t), jobtype: None, free_jt: None, topos: None,
                 listed_at: listed.map(t), picked: picked.map(t) }
    }

    #[test]
    fn free_needs_a_free_event() {
        // 3시간 창 안에 자유 사건이 없으면 빈 트럭이 아니다 — 나머지 신호가 무엇이든.
        assert!(!is_free_tos(&sig(None, None, None, None)));
        assert!(!is_free_tos(&sig(None, Some(10), Some(20), Some(30))));
    }

    #[test]
    fn free_alone_is_enough() {
        assert!(is_free_tos(&sig(Some(100), None, None, None)));
    }

    #[test]
    fn pickup_after_free_means_loaded() {
        // 적하는 야드 픽업 직후 작업목록 A/Q 에서 사라진다 → 픽업 가드가 유일한 방어다.
        assert!(!is_free_tos(&sig(Some(100), Some(101), None, None)), "자유 뒤 픽업 = 싣고 가는 중");
        assert!(is_free_tos(&sig(Some(100), Some(99), None, None)), "픽업이 먼저면 그 트립은 끝났다");
        // 동시각은 '아직 안 실었다'로 본다(픽업 comp 와 자유 comp 가 같은 초에 찍히는 경우).
        assert!(is_free_tos(&sig(Some(100), Some(100), None, None)));
    }

    #[test]
    fn new_dispatch_after_free_means_working() {
        assert!(!is_free_tos(&sig(Some(100), None, Some(101), None)));
        assert!(is_free_tos(&sig(Some(100), None, Some(99), None)));
        assert!(is_free_tos(&sig(Some(100), None, Some(100), None)), "같은 초면 그 배차의 결과가 이 자유다");
    }

    #[test]
    fn listed_at_or_after_free_means_working() {
        // live_workpool 은 A + (Q ∧ 트럭 없음) 만 담아 Q 배차 트럭이 안 보인다 → 이 가드가 그 구멍을 막는다.
        assert!(!is_free_tos(&sig(Some(100), None, None, Some(100))), "자유와 같은 스냅샷이면 이미 실렸다");
        assert!(!is_free_tos(&sig(Some(100), None, None, Some(101))));
        assert!(is_free_tos(&sig(Some(100), None, None, Some(99))), "자유보다 낡은 스냅샷은 판정 근거가 못 된다");
    }

    #[test]
    fn any_single_guard_is_enough_to_reject() {
        // 호출부가 세 가드 중 하나를 빠뜨리면 이 테스트가 잡는다(돌연변이 확인용).
        for s in [sig(Some(100), Some(200), None, None),
                  sig(Some(100), None, Some(200), None),
                  sig(Some(100), None, None, Some(200))] {
            assert!(!is_free_tos(&s));
        }
    }
}
