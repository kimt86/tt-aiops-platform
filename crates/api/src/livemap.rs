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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
    // SHADOW: display-only time-to-free estimate (median + p90 seconds), by state+jobtype.
    // Not used in dispatch yet — see free_in().
    #[serde(skip_serializing_if = "Option::is_none")]
    free_in_s: Option<i64>,
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

/// Display-only estimate of time-to-free (median, p90) in seconds, by dispatch state + jobtype.
/// SHADOW: shown in the API/UI for situational awareness; NOT wired into any dispatch cost or
/// ranking yet (promotion happens only after the live-emitted ETAs are validated against actuals).
///
/// Grounded in `tt_cycle_v2` measurement (DS, last 36h, 2026-06-16, all from the same GPS clock so
/// the tiers are internally consistent):
///   - delivering (loaded, still driving): pickup_left→dropped  p50 17.2m / p90 40.3m  (n=7,264)
///   - arrived at block (approaching/wait_rtg): laden_arrived→dropped  p50 8.0m / p90 27m (n=6,249)
///   - soon_idle (RTG ≤30m engaged, or quay PLC): ~2m — least-grounded tier (no RTG-distance history),
///     rough "handover in progress" value.
/// Only DS is grounded; other jobtypes get None (their free-point differs and is unmeasured here).
fn free_in(state: &str, jobtype: Option<&str>) -> (Option<i64>, Option<i64>) {
    let ds = jobtype == Some("DS");
    match state {
        "delivering" if ds => (Some(1030), Some(2420)),
        "approaching" => (Some(480), Some(1620)), // approaching is DS-only by construction
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
            // shadow time-to-free, derived from the classified state (display-only)
            let (free_in_s, free_in_hi_s) = c
                .as_ref()
                .map(|c| free_in(c.state, p.jobtype.as_deref()))
                .unwrap_or((None, None));
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
    let cycling_trucks: usize = ["empty_travel", "delivering", "soon_idle", "approaching"]
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
// committed window (anti-thrash, TOS prefetch "early-decide, stable-execute"): once a truck's prior
// recommendation is on the verge of dispatch (its work-ETA within this window), switching it away
// costs COMMIT_LOCK_S — a near-lock so GPS jitter can't flip an about-to-go truck off its work.
const COMMIT_WINDOW_MS: i64 = 600_000; // 10 min
const COMMIT_LOCK_S: i64 = 1200;
// NOTE: urgency / starvation / load-balance are NOT cost-matrix terms — Stage 2 is pure empty-travel
// efficiency. Urgency is decided in Stage 1 (work-pool ordering: starving cranes first, then deadline)
// and the per-crane truck cap (NEED_HORIZON/move) bounds demand there. See spawn_stage2_shadow.
// per-container QC handling time (for the bucket's service window + the needed-trucks cap)
const DS_MOVE_S: i64 = 90;
// LD_MOVE_S was 110s. Measured 2026-08-03 on consecutive comp_ts of a continuously-working crane:
// DS p50 90s (the DS constant is exact), LD p50 132s — the LD constant ran 20% optimistic, and it
// scales the deadline spread term ((cap/2)*move_s), so LD deadlines were short by that much too.
const LD_MOVE_S: i64 = 132;
// a bucket can only usefully consume trucks as fast as the QC works through it — cap the trucks we
// commit to one bucket at what it can serve within this horizon (spreads trucks across more QCs).
//
// ⚠⚠ This must be at least as long as the journey a truck has to make, or the cap silences work the
// matcher could actually have served. Measured 2026-08-03: the required lead (p90 travel + the
// unmodelled legs, learn_dispatch_lead) is DS p90 877s but LD p90 1,693s — so at 900s the share of
// LD recommendations whose journey fits inside the horizon was **0.0%**. LD feasibility could not
// exceed ~10% no matter what the matcher did, while real crane starvation ran 8.9%.
//
// It is now derived per job type: lead + a pad for the free-time prediction error. The asymmetry is
// physical, not a special case — for DS the handover IS the pickup (truck drives to the QC and is
// loaded there), for LD the handover is the DROP at the quay and the whole laden leg comes after the
// pickup. mig 0116 measures exactly that gap (DS +75s / LD +1,084s).
// ⚠⚠ DEFAULT OFF — this is a diagnosis that is not yet validated as an improvement.
// Widening the horizon lets one crane absorb more trucks, and the original comment above was right
// that the cap is what "spreads trucks across more QCs". Measured on the live shadow, normalised by
// truck count (which varies with time of day, so the raw counts are confounded):
//     cranes served per truck   before → after
//        ~30 trucks/tick        14.1%  → 13.6%
//        ~50 trucks/tick        13.6%  → 12.7%
//        ~70 trucks/tick        13.9%  → 11.9%
// i.e. the cost (less crane spread) is real and reproducible, while the benefit (feasibility that
// tracks actual starvation) is still unmeasured. Shipping it on that balance would be exactly the
// "measurement-free improvement" the A/B harness above exists to prevent. Turn on with
// `STAGE2_NEED_HORIZON=1`, or `=ab` once the harness carries this arm.
//
// ⚠⚠ PRECONDITION FOR ANY A/B ON THIS ARM — do not measure it while the bias matview is empty.
// The one live window this ran in (2026-08-03) showed LD feasible_crane = 0.0% across every sample
// with crane slack ≈ −1,200s, which looks like "the horizon does nothing". That reading is
// CONFOUNDED: learn_work_eta_bias was still refilling after the mig 0115/0117 truth+version change,
// so the LD correction sat at its bootstrap 0 while the true residual is ~+1,474s. work_eta was
// therefore ~25 min early and `eta_ms.max(now)` pinned every LD deadline into the past — no horizon
// could have helped. Gate the run on `SELECT count(*) FROM learn_work_eta_bias WHERE jobtype='LD'`
// being non-zero first, or both arms just measure the transition.
static NEED_HORIZON_MODE: AtomicU8 = AtomicU8::new(0);
const NEED_HORIZON_BASE_S: i64 = 900;   // floor = the old constant; never shrink below it
const NEED_HORIZON_PAD_S: i64 = 300;    // free-time prediction slack (cycle_pred_shadow |err| p50 ~303s)
const NEED_HORIZON_MAX_S: i64 = 3_000;  // hard ceiling — this term sizes the solver, see works_raw
// The per-crane cap has always been computed with these move times. Kept separate from the physical
// LD_MOVE_S so that correcting the physics (110 → measured 132) does not silently tighten the cap
// while the lever is off — with the lever off the cap must reproduce the historical value exactly.
const CAP_MOVE_DS_S: i64 = 90;
const CAP_MOVE_LD_S: i64 = 110;

// ── 출항 역산 마감 → Stage-1 선택 티어 (SHADOW) ──────────────────────────────────────────────
// 마감을 '값'이 아니라 '계층'으로만 쓴다. work_eta에는 DS +600s(workpool.rs DS_WORK_ETA_BIAS_S)·
// 학습잔차(LD 평균 +780s)·교대정지가 들어 있고 deadline_ts에는 없어서(workpool.rs의 "UNAFFECTED"
// 주석 참조) 두 값의 산술 혼합은 부적절하다. 분기로 결합하면 보정 계통이 섞이지 않는다.
const DEP_TIGHT_S: i64 = 1800;        // = workpool FINISH_BUFFER_S. 마감식이 이미 '출항 30분 전
                                      //   완료'를 버퍼로 잡았으므로, 여유가 버퍼 하나보다 작다
                                      //   = 버퍼를 먹기 시작했다는 물리적 진술(임계값 발명 아님).
const DEP_HYST_S: i64 = 300;          // 60초 틱 5회. 여유는 벽시계만으로 틱당 −60s 표류하므로
                                      //   상승(덜 급해짐) 방향에만 밴드를 건다. SWITCH_PENALTY_S
                                      //   (180s)와 같은 자릿수.
const DEP_URGENT_SLOT_PCT: i64 = 50;  // 긴급 티어(0·1)가 먹을 수 있는 트럭 슬롯 상한 %.
                                      //   실측(2026-07-27): 티어0만으로 캡 통과 슬롯 90개 ≥ 평균
                                      //   트럭 64.7대 → 굶주림이 빈 틱에서 선단을 통째로 삼킨다
                                      //   (음수여유 슬롯 34%→94%, 버킷 Jaccard 0.21). 50%면 같은
                                      //   조건에서 54%·0.67로 잡힌다. 굶주림 버킷은 이 예산에서
                                      //   면제되므로(설계원칙 #3) '굶지는 않는데 마감만 급한'
                                      //   버킷에만 걸리는 가드지 드라이버가 아니다.
/// 출항 마감 티어 모드. `STAGE2_DEP_TIER` 로 정한다:
///   미설정/그 외 = 0 (항상 OFF, 기존 기본값) · `1` = 1 (항상 ON) · `ab` = 2 (A/B 측정)
///
/// A/B 모드가 필요한 이유: 07-27 부터 전 틱이 ON 이라 같은 조건의 반사실이 없다. 그래서 이
/// 레버가 도움이 됐는지 **판정 자체가 불가능**했다. 측정 없는 개선을 쌓지 않으려면 이게 먼저다.
static DEP_TIER_MODE: AtomicU8 = AtomicU8::new(0);
/// 팔을 바꾸는 단위(분). 틱 단위 교대가 아닌 이유: anti-thrash 의 '직전 추천'이 틱을 넘어
/// 이어지므로 매 틱 바꾸면 두 팔이 서로의 잔상에 오염돼 "항상 ON vs 항상 OFF" 를 근사하지 못한다.
const AB_BLOCK_MIN: i64 = 30;
/// 블록 앞부분 = 직전 팔의 잔상이 남은 구간. 버리지 않고 표시만 한다(조용히 버리면 표본이 왜
/// 줄었는지 나중에 아무도 모른다).
const AB_WARMUP_MIN: i64 = 3;

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
const SILENT_HOLD_S: i64 = 1200;     // hold a silent loaded truck up to 20min (covers 88% of waits) before assuming off-shift
const HELD_NEAR_DROP_M: f64 = 120.0; // ...only if its last position is within this of its drop (= waiting there, last stage)

pub fn spawn_stage2_shadow(lm: Arc<LiveMap>, pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        let mut tick = 0u64;
        NEED_HORIZON_MODE.store(
            match std::env::var("STAGE2_NEED_HORIZON").unwrap_or_default().as_str() {
                "1" => 1,
                "ab" => 2, // block-randomised A/B (mig 0119) — the only honest way to judge this arm
                _ => 0,    // default OFF — the cost is measured, the benefit is not (see the const above)
            },
            Ordering::Relaxed,
        );
        DEP_TIER_MODE.store(
            match std::env::var("STAGE2_DEP_TIER").unwrap_or_default().as_str() {
                "1" => 1,
                "ab" => 2,
                _ => 0,
            },
            Ordering::Relaxed,
        );
        // 티어 히스테리시스 상태(틱마다 현재 키 집합으로 통째 교체 = 누수 없음)
        let mut prev_tier: HashMap<(String, String, String), u8> = HashMap::new();
        loop {
            ticker.tick().await;
            tick += 1;
            if !lm.connected.load(Ordering::Relaxed) {
                continue;
            }
            let now = Utc::now().timestamp_millis();
            // previous-tick recommendation per vehicle (ytno → work bucket key) for anti-thrash.
            // ts-based (restart-safe), latest per vehicle within the last ~2.5 ticks.
            let prev: HashMap<String, (String, String, String)> = sqlx::query_as::<_, (String, String, String, String)>(
                "SELECT DISTINCT ON (ytno) ytno, coalesce(qc,''), coalesce(vessel,''), coalesce(queuename,'')
                   FROM stage2_match_shadow WHERE ts > now() - interval '150 seconds' ORDER BY ytno, ts DESC",
            )
            .fetch_all(&pool).await.unwrap_or_default()
            .into_iter().map(|(yt, q, v, qn)| (yt, (q, v, qn))).collect();
            // cranes that are ACTUALLY stuck waiting for a truck right now (live starvation signal) —
            // these get a decisive urgency pull so trucks go to them, not to merely-nearby load work.
            let starving: std::collections::HashSet<String> = sqlx::query_scalar::<_, String>(
                "SELECT DISTINCT qc FROM qc_wait_qc_sample WHERE ts > now() - interval '90 seconds' AND starving_real",
            )
            .fetch_all(&pool).await.unwrap_or_default().into_iter().collect();
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
            // Per-crane demand horizon, derived from the same measured lead. At the old flat 900s the
            // LD journey (p90 1,693s) never fit, so LD buckets were capped to work the crane would
            // reach within 15 minutes — work TOS had already dispatched ~21 minutes earlier. Every LD
            // recommendation was therefore late by construction. See NEED_HORIZON_BASE_S.
            // A/B arm for the horizon lever. Same 30-min blocks as the departure-tier harness, but a
            // DIFFERENT salt: sharing one would make the two levers' arms perfectly correlated and
            // neither effect could be separated from the other.
            let horizon_mode = NEED_HORIZON_MODE.load(Ordering::Relaxed);
            let horizon_block = now / 1000 / 60 / AB_BLOCK_MIN;
            let horizon_on = match horizon_mode {
                1 => true,
                2 => (splitmix64(horizon_block as u64 ^ 0x3C6E_F372_FE94_F82B) >> 63) & 1 == 1,
                _ => false,
            };
            let need_horizon: HashMap<String, i64> = ["DS", "LD"].iter().map(|jt| {
                if !horizon_on {
                    return (jt.to_string(), NEED_HORIZON_BASE_S); // no-op: exactly the old constant
                }
                let lead = lead_extra.get(*jt).copied().unwrap_or(0);
                let h = (lead + NEED_HORIZON_PAD_S + NEED_HORIZON_BASE_S / 2)
                    .clamp(NEED_HORIZON_BASE_S, NEED_HORIZON_MAX_S);
                (jt.to_string(), h)
            }).collect();
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
                   SELECT trk_id AS ytno, 'DS'::text jt, comp_ts pk FROM qc_move_log
                    WHERE jobtype='DS' AND status='F' AND comp_ts > now()-interval '3 hours' AND trk_id IS NOT NULL
                   UNION ALL
                   SELECT trk_id, 'LD', comp_ts FROM rtg_move_log
                    WHERE jobtype='LD' AND status='F' AND comp_ts > now()-interval '3 hours' AND trk_id IS NOT NULL
                 ), latest AS (
                   SELECT DISTINCT ON (ytno) ytno, jt, pk FROM pick ORDER BY ytno, pk DESC
                 ), freed AS (
                   SELECT ytno, max(free_ts) f FROM tt_move_log WHERE free_ts > now()-interval '3 hours' GROUP BY 1
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
            // candidate vehicles: idle (free now) + soon-free; committed/moving (delivering/empty) skipped
            let vehicles: Vec<(String, f64, f64, i64, &'static str)> = {
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
                let mut v = Vec::new();
                for (id, p) in map.iter() {
                    if p.cls != "TT" { continue; }
                    let age = (now - p.last_seen_ms) / 1000;
                    if age > STALE_AFTER_S {
                        // ★ hold a SILENT last-stage truck as a soon-free candidate (proactive coverage):
                        // loaded + last position near its drop + silent = it stopped waiting to be served
                        // (the unit only reports on movement). Key it on the DROP location (where it frees),
                        // robust to the stale GPS. Without this we miss ~25% of about-to-free trucks.
                        //
                        // SILENT_HOLD_S is a belief, not an observation: past 20 min of silence we ASSUME
                        // the truck went off-shift. The move-log anchor replaces that assumption with TOS
                        // evidence — a crane loaded it and no free_ts has landed, so it is still working.
                        // Hold those to twice the blind limit. Trucks with no anchor keep the old cutoff.
                        let anchored = inflight.get(id).copied();
                        if age > if anchored.is_some() { SILENT_HOLD_S * 2 } else { SILENT_HOLD_S } { continue; }
                        let jt = match p.jobtype.as_deref().or(p.latched_jobtype.as_deref()) { Some(j @ ("LD" | "DS")) => j, _ => continue };
                        let loaded = p.container1.as_deref().is_some_and(|s| !s.is_empty())
                            || p.latched_container.as_deref().is_some_and(|s| !s.is_empty());
                        if !loaded { continue; }
                        let code = match p.topos1.as_deref().filter(|s| !s.is_empty()).or(p.latched_topos.as_deref()) { Some(c) => c, None => continue };
                        let is_cr = is_crane_code(code);
                        if jt == "LD" { if !is_cr { continue; } } else if is_cr { continue; } // drop-side only (LD=crane / DS=bay)
                        let dpos = if is_cr { cranes.get(code).copied().or_else(|| centroids.get(code).map(|c| (c.lat, c.lon))) }
                                   else { centroids.get(code).or_else(|| centroids.get(block_prefix(code))).map(|c| (c.lat, c.lon)) };
                        let Some((dlat, dlon)) = dpos else { continue };
                        if dist_m((p.lat, p.lon), (dlat, dlon)) > HELD_NEAR_DROP_M { continue; }
                        // Time-to-free for a truck the GPS cannot see. The old value here was a learned
                        // per-jobtype CONSTANT (st_free) — the same number for every silent truck
                        // regardless of when it actually picked up. The anchor knows that: it counts
                        // down from this trip's own pickup. Only a constant is being replaced, so this
                        // cannot regress the GPS estimate — that path is untouched (see the head-to-head
                        // above: GPS wins on LD and must keep its own duration).
                        let (base, state) = match anchored {
                            Some(rem) => (rem.clamp(0, 3600), "soon_idle_anchored"),
                            None => (st_free.get(jt).map(|&(m, _)| m).unwrap_or(300).clamp(30, 3600), "soon_idle_held"),
                        };
                        v.push((id.clone(), dlat, dlon, base, state));
                        continue;
                    }
                    let c = classify_tt(p, assigned_pool.get(id), &rtgs, &plc, &cranes, &centroids, now);
                    let base = match c.state {
                        "idle" => 0,
                        // ⑤⑥ time-to-free: learned per-stage median seconds (state × jobtype × RTG
                        // bin, fallback state×jobtype, fallback the free_in constant). Replaces the
                        // miscalibrated constants for every candidate stage (soon_idle 120→~300, etc).
                        //
                        // ⚠ RETIREMENT DECIDED, NOT YET DONE. The plan is: keep the GPS states for
                        // SELECTING candidates, but drop this DURATION and take it from the move-log
                        // predictor (learn_cycle_remaining) instead — because this value is learned
                        // against tt_cycle_log.dropped_at, a GPS label that misses ~33% of real free
                        // events (see the ⚠⚠ block on spawn_free_in_logger for the measurements).
                        // Until that swap lands this is still what the matcher uses, so anyone
                        // benchmarking "current vs new" must not mistake it for a validated baseline.
                        s @ ("soon_idle" | "approaching" | "wait_rtg") => {
                            // jobtype must use the SAME latched fallback the MV/logger key on
                            // (free_in_sample writer + near-miss logger), else a momentary None
                            // jobtype misses the learned bucket and collapses to the 30s floor.
                            let jt = p.jobtype.clone().or_else(|| p.latched_jobtype.clone()).unwrap_or_default();
                            // 정차 앵커 (mig 0091): a truck genuinely STOPPED at its drop (arrived state
                            // + speed<idle) frees in the GPS-stationary median (LD ~141/DS ~258) —
                            // ~half the loose-arrival free_in estimate, correctly calibrated. Moving/
                            // approaching trucks keep free_in_bias (they still have to reach + stop).
                            let stopped = s != "approaching" && p.speed < IDLE_SPEED_KMH;
                            if let Some(&(med, _p90)) = st_free.get(&jt).filter(|_| stopped) {
                                med
                            } else {
                                let bin = dist_bin_of(c.nearest_rtg_m);
                                fi_bias
                                    .get(&(s.to_string(), jt.clone(), bin))
                                    .or_else(|| fi_bias.get(&(s.to_string(), jt.clone(), -99)))
                                    .copied()
                                    .unwrap_or_else(|| free_in(s, (!jt.is_empty()).then_some(jt.as_str())).0.unwrap_or(0))
                                    .clamp(30, 3600)
                            }
                        }
                        _ => continue,
                    };
                    v.push((id.clone(), p.lat, p.lon, base, c.state));
                }
                drop(fi_bias);
                drop(st_free);
                v
            };
            if vehicles.is_empty() {
                continue;
            }
            // candidate work + pickup coord
            let Ok(work) = crate::workpool::stage2_work_candidates(pool.clone()).await else { continue };
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
            let works: Vec<(usize, f64, f64, i64)> = work.iter().enumerate().filter_map(|(i, w)| {
                let coord = if w.jobtype == "LD" {
                    w.src_block.as_ref().and_then(|b| centroids_now.get(b).copied())
                } else {
                    cranes_now.get(&w.qc).copied().or_else(|| centroids_now.get(&w.qc).copied())
                }?;
                Some((i, coord.0, coord.1, w.work_eta_ts?.timestamp_millis()))
            }).collect();
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
            // 팔 배정. 블록 번호를 해시해서 정하므로 결정적이고(같은 블록은 늘 같은 팔) 단순 교대가
            // 아니다 — 30분 주기와 공명하는 외부 요인(교대·선박 리듬)에 팔이 정렬되는 것을 피한다.
            let mode = DEP_TIER_MODE.load(Ordering::Relaxed);
            let minute = now / 1000 / 60;
            let (ab_block, ab_warmup) = if mode == 2 {
                (Some(minute / AB_BLOCK_MIN), (minute % AB_BLOCK_MIN) < AB_WARMUP_MIN)
            } else {
                (None, false)
            };
            let dep_on = match mode {
                1 => true,
                // top bit of a splitmix64 hash of the block index — see the note on splitmix64
                2 => (splitmix64(ab_block.unwrap_or(0) as u64 ^ 0xA5A5_5A5A_1234_5678) >> 63) & 1 == 1,
                _ => false,
            };
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
            // STAGE 1 owns urgency + per-crane demand caps; STAGE 2 (the matching below) is then PURE
            // efficiency: edge cost = empty-travel arrival only (+ anti-thrash switch penalty). Order the
            // work pool by urgency HERE — starving cranes first, then the DEPARTURE tier (vessel-deadline
            // risk), then work-ETA (when the crane reaches the bay) — so urgency is decided in SELECTION,
            // not leaked into the matching cost.
            let mut order: Vec<usize> = (0..works.len()).collect();
            order.sort_by_key(|&i| {
                let qc = &work[works[i].0].qc;
                (if starving.contains(qc) { 0u8 } else { 1u8 },   // ① 굶주림(살아있는 신호) — 불변·최상위
                 if dep_on { dep_tier[i] } else { 0u8 },          // ② 출항 마감 티어 — 유일한 연결점
                 works[i].3)                                      // ③ work-ETA ms — 결정적 타이브레이크(필수)
            });
            // ③은 필수다: LD src_block fan-out 때문에 최대 13버킷이 같은 (qc,vessel,queuename) = 같은
            // 마감·같은 work_eta를 공유하므로 여기서 끊지 않으면 Vec 삽입순서로 무너진다.
            // OFF일 때 ②가 전 버킷 상수 0 → sort_by_key는 stable sort이므로 오늘과 바이트 단위로
            // 동일한 order가 언어 보장으로 성립한다(완전 무동작 킬스위치).
            // 티어 '없는' 기본 순서 = 레버가 꺼져 있을 때의 바로 그 순서(①③만). 아래 슬롯 예산에
            // 밀린 긴급 버킷을 버리지 않고 '원래 자리'로 되돌리는 데 쓴다 — 마감이 더 급한 버킷이
            // 레버 때문에 되레 풀 꼬리로 밀리는 역전을 막는 안전망.
            let mut base_order: Vec<usize> = (0..works.len()).collect();
            base_order.sort_by_key(|&i| {
                (if starving.contains(&work[works[i].0].qc) { 0u8 } else { 1u8 }, works[i].3)
            });
            // POINT 1 — cap the work pool to the available-truck count, most deadline-urgent first.
            // Walk works in deadline order, summing each bucket's QC-capped demand (= the slots a crane
            // can actually consume within the horizon); keep buckets until we've gathered as many slots
            // as there are trucks. Buckets whose QC is already full are dropped. Guarantees works ≤
            // trucks AND that trucks always go to the most deadline-urgent work first.
            // The CAPPED per-bucket demand (take) — the truck-loads each bucket may receive after the
            // per-crane horizon cap. This is the bucket cap carried into the matching, so STAGE 2 needs
            // NO separate QC layer (the per-crane limit is already baked into the demand here).
            // 캡 적용 '전' 후보 버킷 수. n_works(캡 후)만으로는 수요가 적은 건지 트럭이 모자라
            // 잘린 건지 구분할 수 없고, 두 팔 비교에는 그 구분이 필수다.
            let works_raw = order.len() as i32;
            let mut cap_by_oi: HashMap<usize, i64> = HashMap::new();
            let mut dep_demoted_n: i32 = 0; // 예산에 밀려 기본 순서로 강등된 긴급 버킷 수(레버 세기 진단)
            {
                let truck_n = vehicles.len() as i64;
                // 긴급 슬롯 예산 = 캡 '바깥'의 상한. NEED_HORIZON_S / qc_cap 산식은 일절 손대지 않는다
                // (마감 p50 222분 vs 캡 지평 15분 = 스케일 15~20배 불일치 → 섞으면 캡의 물리적 의미가
                // 깨지고 회귀 원인 특정이 불가능해진다).
                // ⚠ 굶주림 버킷은 예산에서 면제한다 — 설계원칙 #3(굶주림 최우선)은 불변이고, 예산은
                //   '굶지는 않는데 마감만 급한' 버킷이 선단을 삼키는 것을 막는 가드일 뿐이다. 면제가
                //   없으면 정렬 ①이 굶주림을 order 앞머리에 몰아놓은 탓에 예산이 굶주림 블록 '안에서'
                //   먼저 소진되고(굶주림 QC p50 5·p90 11 × 버킷캡 8~10 ≫ 트럭 68대 기준 예산 34),
                //   뒤쪽 굶주림 버킷이 굶지 않는 여유 버킷에 자리를 뺏긴다. 재현(라이브 workpool을
                //   "늦은 배의 크레인이 곧 굶는다"로 합성·굶주림 QC 5개): 면제 전에는 트럭 40대에서
                //   굶주림 슬롯 40→21·서빙 굶주림 QC 5→3개, 65대에서 40→34. 면제 후 전 구간 OFF와 동일.
                // 예산을 넘긴 긴급 버킷은 '삭제'가 아니라 티어 승격만 잃고 base_order(=OFF 순서)로
                // 강등된다. 아래 두 패스가 그 강등을 실제로 수행한다.
                let urgent_budget = if dep_on { truck_n * DEP_URGENT_SLOT_PCT / 100 } else { 0 };
                let mut qc_room: HashMap<String, i64> = HashMap::new();
                let mut acc: i64 = 0;
                let mut urgent_acc: i64 = 0; // 예산 '판정' 전용 카운터(로깅 집계는 아래에서 따로 낸다)
                let mut kept: Vec<usize> = Vec::with_capacity(order.len());
                // 기존 take 계산을 그대로 쓰는 내부 함수(캡처 없음 → 빌림 충돌 없음)
                fn take_bucket(w: &crate::workpool::Stage2Work, oi: usize,
                               qc_room: &mut HashMap<String, i64>, acc: &mut i64,
                               kept: &mut Vec<usize>, cap_by_oi: &mut HashMap<usize, i64>,
                               horizon: &HashMap<String, i64>) -> i64 {
                    // Cap arithmetic uses the historical move constants on purpose — see CAP_MOVE_*.
                    let move_s = if w.jobtype == "LD" { CAP_MOVE_LD_S } else { CAP_MOVE_DS_S };
                    let h = horizon.get(&w.jobtype).copied().unwrap_or(NEED_HORIZON_BASE_S);
                    let qc_cap = (h / move_s).max(1);
                    let room = qc_room.entry(w.qc.clone()).or_insert(qc_cap);
                    let take = (w.n.max(0) as i64).min(*room);
                    if take <= 0 {
                        return 0; // this QC can't consume more trucks this horizon → skip its bucket
                    }
                    *room -= take;
                    *acc += take;
                    cap_by_oi.insert(oi, take);
                    kept.push(oi);
                    take
                }
                // 승격 패스 — 굶주림 버킷(전부·예산 면제) + 비굶주림 긴급 버킷(예산까지)을 티어 순서로.
                for &oi in &order {
                    if acc >= truck_n {
                        break;
                    }
                    let starve = starving.contains(&work[works[oi].0].qc);
                    let urgent = dep_on && !starve && dep_tier[oi] < 2;
                    if !starve && !urgent {
                        continue;                            // 여유 티어는 아래 기본 패스에서 담는다
                    }
                    if urgent && urgent_acc >= urgent_budget {
                        dep_demoted_n += 1;                  // 강등 — 버리는 게 아니라 기본 패스에서 다시 만난다
                        continue;
                    }
                    let took = take_bucket(&work[works[oi].0], oi, &mut qc_room, &mut acc, &mut kept, &mut cap_by_oi, &need_horizon);
                    if urgent {
                        urgent_acc += took;
                    }
                }
                // 기본 패스 — 남은 버킷 전부를 티어 없는 기본 순서로. 강등된 긴급 버킷은 꼬리로
                // 밀리는 게 아니라 여기서 자기 원래(OFF) 자리를 되찾는다. 예산이 먼저 먹은 슬롯만큼
                // 남은 자리가 주는 것은 예산 레버의 의도된 효과이고, '더 급한데 더 뒤로'는 없다.
                // OFF일 때는 urgent가 전 버킷 false + order==base_order이라 두 패스를 이어붙인 방문
                // 순서가 종전 단일 워크와 완전히 동일하다(무동작 킬스위치 유지). 예산이 물리지 않는
                // 구간에서도 (승격=티어0→1, 기본=티어2) 순서가 종전 티어 워크와 동일하다.
                for &oi in &base_order {
                    if acc >= truck_n {
                        break;
                    }
                    if cap_by_oi.contains_key(&oi) {
                        continue;                            // 승격 패스에서 이미 담김
                    }
                    take_bucket(&work[works[oi].0], oi, &mut qc_room, &mut acc, &mut kept, &mut cap_by_oi, &need_horizon);
                }
                order = kept;
            }
            // 로깅용 집계는 예산 '판정'과 분리한다(굶주림 면제·강등 때문에 판정 카운터는 최종 풀의
            // 구성과 다르다). 이 값 = 최종 풀에서 티어0·1 버킷이 실제로 가져간 슬롯 = 레버의 실세기.
            // dep_on이 꺼져 있어도 티어는 계산되므로 그대로 반사실 베이스라인이 된다.
            let dep_urgent_slots: i64 = order.iter().filter(|&&oi| dep_tier[oi] < 2)
                .map(|&oi| cap_by_oi.get(&oi).copied().unwrap_or(0)).sum();
            // (qc,vessel,queuename) → work-ETA ms, for the committed-window check on prior recommendations
            let eta_by_key: HashMap<(String, String, String), i64> = work.iter()
                .filter_map(|w| w.work_eta_ts.map(|e| ((w.qc.clone(), w.vessel.clone(), w.queuename.clone()), e.timestamp_millis())))
                .collect();
            // STAGE 2 — PURE EFFICIENCY MATCHING. The work pool + per-crane demand caps are already
            // fixed by Stage 1; here each edge cost is just the truck's empty travel (+ anti-thrash
            // switch penalty). No urgency/starve/load-balance terms and no QC layer — those are Stage-1.
            let mut caps: Vec<i64> = Vec::with_capacity(order.len()); // per-bucket demand (Stage-1 capped)
            let mut deadlines: Vec<i64> = Vec::with_capacity(order.len()); // feasibility deadline ms per wpos
            // 출항 축 로깅용(정렬 후 wpos 인덱스로 재배열). 기존 deadlines[]와 '별개 축'이다 —
            // 절대 교체/융합하지 않는다(현행 지평 p50 1043s는 실제 도착 p90 538s와 같은 스케일이라
            // 진짜 물리는 지표인데, 출항 마감 p50 17041s로 바꾸면 전 구간 88% 평평한 상수가 된다).
            let mut dep_slack_w: Vec<Option<i64>> = Vec::with_capacity(order.len());
            let mut dep_tier_w: Vec<u8> = Vec::with_capacity(order.len());
            let mut edges: Vec<(usize, usize, i64)> = Vec::new(); // (truck, work-pos, cost)
            let mut matrix: Vec<Vec<(i64, i64, &'static str, bool)>> = Vec::with_capacity(order.len()); // [wpos][vi]=(arr,p90,tier,switched)
            for &oi in &order {
                let (wi, wlat, wlon, eta_ms) = works[oi];
                let w = &work[wi];
                let move_s = if w.jobtype == "LD" { LD_MOVE_S } else { DS_MOVE_S };
                let cap_j = *cap_by_oi.get(&oi).unwrap_or(&0); // Stage-1 capped demand (truck-loads)
                caps.push(cap_j);
                // feasibility deadline: bucket served from max(eta,now) over its near slots — midpoint.
                let spread_ms = (cap_j / 2) * move_s * 1000;
                deadlines.push(eta_ms.max(now) + spread_ms);
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
            for (wpos, _oi) in order.iter().enumerate() {
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
                    if now + p90 * 1000 > deadline {
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
                let (wi, wlat, wlon, _eta) = works[order[wpos]];
                let w = &work[wi];
                let deadline = deadlines[wpos];
                let (arr, arr_p90, tier, switched) = matrix[wpos][vi];
                let arrival_at = now + arr_p90 * 1000;
                let slack = (deadline - arrival_at) / 1000;
                let feasible = arrival_at <= deadline;
                if !feasible {
                    opt_miss += 1;
                }
                // mig 0116 — the crane-referenced axis, logged ALONGSIDE the two above. The old pair
                // keeps its exact meaning on purpose: 19 days of series sit behind it at 21-day
                // retention, so redefining in place would blend two meanings with no discriminator.
                let extra_s = lead_extra.get(&w.jobtype).copied().unwrap_or(0);
                let crane_slack = (deadline - (arrival_at + extra_s * 1000)) / 1000;
                opt_cost += arr;
                let v = &vehicles[vi];
                // deadline_slack_s / feasible = 크레인 필요 시각(work-ETA) 기준 — 정의 불변(19일치
                // 시계열 + 21일 보존이라 재정의하면 두 의미가 구분자 없이 섞인다). 출항 축은 신규
                // 컬럼(dep_slack_s / dep_tier)으로만 추가한다.
                let ins = sqlx::query(
                    "INSERT INTO stage2_match_shadow
                       (ts,tick,ytno,qc,vessel,queuename,jobtype,src_block,veh_state,arrival_s,od_p90_s,deadline_slack_s,feasible,cost_tier,switched,dest_lat,dest_lon,src_lat,src_lon,dep_slack_s,dep_tier,lead_extra_s,crane_slack_s,feasible_crane,dispatch_deadline_ts,dd_slack_s,dd_lead_s)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26,$27) ON CONFLICT (ts,ytno) DO NOTHING",
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
                "INSERT INTO stage2_solver_shadow (ts,tick,n_trucks,n_works,greedy_n,greedy_cost_s,optimal_n,optimal_cost_s,gap_pct,greedy_miss,optimal_miss,dep_tier_on,dep_tier0_n,dep_urgent_slots,dep_null_n,dep_demoted_n,ab_block,ab_warmup,works_raw,need_horizon_on)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20) ON CONFLICT (ts) DO NOTHING",
            )
            .bind(ts).bind(tick as i64).bind(vehicles.len() as i32).bind(order.len() as i32)
            .bind(greedy_n).bind(greedy_cost).bind(assign.len() as i32).bind(opt_cost).bind(gap_pct as f32)
            .bind(greedy_miss).bind(opt_miss)
            // 티어가 꺼진 구간에서도 항상 기록 → 켜기 전 반사실 베이스라인(신규 컬럼이라 과거
            // 19일치로는 복원 불가). dep_null_n은 커밋 A의 폴백이 선박을 잃으면 즉시 드러나는 경보.
            .bind(dep_on)                                                                 // dep_tier_on
            .bind(order.iter().filter(|&&oi| dep_tier[oi] == 0).count() as i32)           // dep_tier0_n
            .bind(dep_urgent_slots as i32)                                                // dep_urgent_slots
            .bind(order.iter().filter(|&&oi| dep_slack[oi].is_none()).count() as i32)     // dep_null_n
            .bind(dep_demoted_n)                                                          // dep_demoted_n
            .bind(ab_block).bind(ab_warmup).bind(works_raw)                                // A/B 하네스
            .bind(horizon_on)                                                             // mig 0119: 지평 팔
            .execute(&pool).await;
            if let Err(e) = solver_ins {
                tracing::warn!(error = %e, "stage2_solver_shadow insert failed — 0104 마이그레이션 적용 여부 확인");
            }
            if tick % 30 == 0 {
                crate::db::prune(&pool, "stage2_match_shadow", "DELETE FROM stage2_match_shadow WHERE ts < now() - interval '21 days'").await;
                crate::db::prune(&pool, "stage2_solver_shadow", "DELETE FROM stage2_solver_shadow WHERE ts < now() - interval '21 days'").await;
            }
        }
    });
}

/// TOS-vs-ours dispatch comparison (SHADOW). Timing-skew-free: for works TOS just assigned, we
/// reconstruct the truck pool AT the dispatch instant (T1=upd_ts) from `truck_pos_hist`, then — for
/// that one work — recompute OUR pick (closest available truck to the pickup) and TOS's truck arrival
/// from the SAME T1 positions. Same instant, same pool, same work → a clean 1:1 comparison.
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
            // truck pool snapshots (last 6 min), keyed by snapshot time → reconstruct T1 state
            let hist = sqlx::query_as::<_, (DateTime<Utc>, String, f64, f64, Option<String>)>(
                "SELECT ts, ytno, lat, lon, state FROM truck_pos_hist WHERE ts > now() - interval '6 minutes'",
            )
            .fetch_all(&pool).await.unwrap_or_default();
            let mut snaps: std::collections::BTreeMap<i64, Vec<(String, f64, f64, String)>> = std::collections::BTreeMap::new();
            for (t, yt, la, lo, st) in hist {
                snaps.entry(t.timestamp_millis()).or_default().push((yt, la, lo, st.unwrap_or_default()));
            }
            // each truck's MOST-RECENT position+state — fallback when the T1 snapshot doesn't contain
            // a given truck (captured a frame apart) or for assignments older than the 6-min history.
            let mut latest_pos: HashMap<String, (f64, f64, String)> = HashMap::new();
            for trucks in snaps.values() {
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
            let rows = sqlx::query_as::<_, (String, String, String, String, DateTime<Utc>, Option<String>)>(
                "SELECT DISTINCT ON (w.qc, w.queuename, w.ytno)
                        w.qc, w.queuename, w.jobtype, w.ytno, w.upd_ts, w.yt_topos
                   FROM live_workpool w
                  WHERE w.ytno IS NOT NULL AND w.ytno <> '' AND w.qc IS NOT NULL
                    AND w.jobtype IN ('DS','LD')
                    AND NOT EXISTS (SELECT 1 FROM dispatch_compare_shadow d
                                     WHERE d.qc=w.qc AND d.queuename=w.queuename
                                       AND d.tos_ytno=w.ytno AND d.tos_upd=w.upd_ts)
                  ORDER BY w.qc, w.queuename, w.ytno, w.upd_ts DESC",
            )
            .fetch_all(&pool).await.unwrap_or_default();
            for (qc, queue, jobtype, tos_ytno, tos_upd, yt_topos) in rows {
                // pickup coord: LD = the source block centroid, DS = the QC crane (live or learned)
                let coord = if jobtype == "LD" {
                    yt_topos.as_deref().and_then(|t| centroids.get(t).or_else(|| centroids.get(block_prefix(t))).copied())
                } else {
                    cranes.get(&qc).or_else(|| centroids.get(&qc)).copied()
                };
                let Some((dlat, dlon)) = coord else { continue };
                let t1 = tos_upd.timestamp_millis();
                // PRECISE = the snapshot ≤ T1 exists AND contains the TOS truck → tos + our pool are
                // read from that same instant (timing-skew-free). Otherwise fall back to each truck's
                // latest position (a "now-estimate", reason='now') so EVERY assignment still gets a pick.
                let at_t1 = snaps.range(..=t1).next_back().map(|(_, v)| v);
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
                    if !matches!(st, "idle" | "soon_idle" | "approaching" | "wait_rtg") {
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
                let _ = sqlx::query(
                    "INSERT INTO dispatch_compare_shadow
                       (qc,queuename,jobtype,tos_ytno,tos_arrival_s,our_ytno,our_arrival_s,agree,reason,delta_s,tos_upd)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT (qc,queuename,tos_ytno,tos_upd) DO NOTHING",
                )
                .bind(&qc).bind(&queue).bind(&jobtype).bind(&tos_ytno).bind(tos_arrival.map(|x| x as i32))
                .bind(&our_ytno).bind(our_arrival as i32).bind(agree).bind(reason).bind(delta.map(|x| x as i32)).bind(tos_upd)
                .execute(&pool).await;
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

/// splitmix64 finalizer — hashes a COUNTER (here: the A/B block index) into a well-mixed word.
///
/// ⚠ xorshift is the wrong tool for this and was the first attempt: one round of it over
/// sequential inputs barely avalanches, and the lowest bit is the least mixed of all. Measured
/// over 480 blocks it gave a **6-hour repeating pattern** (402 runs out of 480 = near-alternating),
/// which would have locked the two arms to the shift rhythm — the exact confound the block design
/// exists to avoid. splitmix64 + taking the TOP bit gives no detectable period, 222 runs (random
/// expectation ~240), and an even spread across time-of-day slots.
fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
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
            let rows = sqlx::query_as::<_, (String, String, String, String, DateTime<Utc>, Option<String>)>(
                "SELECT DISTINCT ON (w.qc, w.queuename, w.ytno) w.qc, w.queuename, w.jobtype, w.ytno, w.upd_ts, w.yt_topos
                   FROM live_workpool w
                  WHERE w.ytno IS NOT NULL AND w.ytno <> '' AND w.qc IS NOT NULL AND w.jobtype IN ('DS','LD')
                    AND w.upd_ts > now() - interval '15 minutes'
                  ORDER BY w.qc, w.queuename, w.ytno, w.upd_ts DESC LIMIT 400",
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
