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
    // SMOOTHED live QC starvation (GPS-distance, pending-work gated) — rolling mean over the last
    // few 30s ticks, written by spawn_qc_wait_logger and read by the positions endpoint. Replaces
    // the jumpy per-request topos1 count. (count, avg_wait_s).
    qc_wait_live: RwLock<Option<(usize, Option<i64>)>>,
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
            qc_wait_live: RwLock::new(None),
            connected: AtomicBool::new(false),
            messages: AtomicU64::new(0),
            reconnects: AtomicU64::new(0),
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

const RTG_BAY_M: f64 = 30.0; // RTG within this of a TT ≈ same bay (engaged)
const IDLE_SPEED_KMH: f64 = 3.0;
// A TT within this of its ASSIGNED quay crane's GPS ≈ arrived at the crane. Used ONLY to
// populate the SHADOW crane-arrival columns (observational); the live phase logic is untouched.
const CRANE_ARRIVE_M: f64 = 40.0;
// A fresh, unengaged TT within this of a crane ≈ a truck that was available nearby. Used by the
// per-QC starvation log (near_idle_tt) to separate "no truck dispatched in time" (Stage-1) from
// "no truck was anywhere near" (Stage-2/location) when validating dispatch timing.
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
    aj: Option<&AssignedJob>,
    rtgs: &[(f64, f64)],
    plc: &HashMap<String, Plc>,
    cranes: &HashMap<String, (f64, f64)>,
    centroids: &HashMap<String, Centroid>,
    now: i64,
) -> Classed {
    let st = |state, reason: Option<String>| Classed { state, reason, ..Default::default() };
    let assigned = aj.is_some(); // in live_assigned_tt (any active job)
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
    if p.arrival.as_deref() != Some("ARRIVED") {
        return st("delivering", Some("적재 이동 중".into()));
    }
    let topos = p.topos1.as_deref().unwrap_or("");
    let is_crane = is_crane_code(topos);
    // Which side UNLOADS this job (= frees the TT)? LD unloads at the quay crane; DS/MO/MI at a
    // block. A loaded TT ARRIVED at the *other* side just picked up → still delivering.
    let drop_at_crane = match p.jobtype.as_deref().unwrap_or("") {
        "LD" => true,
        "DS" | "MO" | "MI" => false,
        _ => is_crane,
    };
    if drop_at_crane {
        if !is_crane {
            return st("delivering", Some("적재 이동 (안벽行)".into()));
        }
        let plc_ok = plc.get(topos).is_some_and(|c| (now - c.last_seen_ms) / 1000 <= STALE_AFTER_S);
        let reason = if plc_ok { format!("안벽 {topos} 핸드오버 · PLC 확인") } else { format!("안벽 {topos} 핸드오버") };
        return st("soon_idle", Some(reason));
    }
    if is_crane {
        return st("delivering", Some("적재 이동 (블록行)".into()));
    }
    // block handover — RTG engaged via GPS (≈ same bay) OR TOS says the order's RTG is active.
    // GPS misses DS RTGs (no PLC, nearest fix is often >30m), so the authoritative TOS ACTV
    // signal (JOB_ODR_ACTV_DT) fills that gap. Kept as a distinct reason so the upgrade is
    // auditable. See research/soon-idle-tos (연구 2차).
    let tos_active = aj.is_some_and(|a| a.rtg_active);
    let nearest = rtgs.iter().map(|r| dist_m((p.lat, p.lon), *r)).fold(f64::INFINITY, f64::min);
    let d = nearest.is_finite().then(|| (nearest * 10.0).round() / 10.0);
    match (d, tos_active) {
        (Some(d), _) if d <= RTG_BAY_M => {
            Classed { state: "soon_idle", reason: Some(format!("블록 RTG 근접 {d:.0}m")), nearest_rtg_m: Some(d), ..Default::default() }
        }
        (_, true) => {
            // ACTV set = the QC already discharged this box onto the truck (verified: ACTV_DT ==
            // QC move complete, 0s). The truck is at/near the block waiting for the RTG (not yet
            // GPS-engaged) — "approaching", ~12 min median from QC-handover to free.
            let reason = match d {
                Some(d) => format!("QC 양하 완료 · RTG 대기 (GPS {d:.0}m)"),
                None => "QC 양하 완료 · RTG 대기 (GPS 미관측)".into(),
            };
            Classed { state: "approaching", reason: Some(reason), nearest_rtg_m: d, ..Default::default() }
        }
        (Some(d), false) => {
            Classed { state: "wait_rtg", reason: Some(format!("도착 · RTG 대기 (최근접 {d:.0}m)")), nearest_rtg_m: Some(d), ..Default::default() }
        }
        (None, false) => st("wait_rtg", Some("도착 · RTG 미관측".into())),
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
    let (bd_cur, sh_cur) = wp_core::shift::current(wp_core::shift::terminal_now().naive_local());
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
            let (bd, sh) = wp_core::shift::current(wp_core::shift::terminal_now().naive_local());
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
            let (cur_container, soon): (HashMap<String, String>, Vec<PredRow>) = {
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
                let mut cur: HashMap<String, String> = HashMap::new();
                let mut soon: Vec<PredRow> = Vec::new();
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
                    if cl.state != "soon_idle" {
                        continue;
                    }
                    let Some(container) = container else { continue }; // soon_idle ⇒ loaded ⇒ present
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
                (cur, soon)
            };

            // release ended trips (truck no longer carrying that container) + pick new first-entries
            let to_insert: Vec<PredRow> = {
                let mut open = lm.soon_idle_open.lock().await;
                open.retain(|(yt, c0)| cur_container.get(yt).map(|c| c == c0).unwrap_or(false));
                soon.into_iter()
                    .filter(|r| open.insert((r.ytno.clone(), r.container.clone())))
                    .collect()
            };
            if to_insert.is_empty() {
                continue;
            }
            let (bd, sh) = wp_core::shift::current(wp_core::shift::terminal_now().naive_local());
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
            for (topos, c) in &snap {
                if c.obs == 0 {
                    continue;
                }
                if let Err(e) = sqlx::query(
                    "INSERT INTO learn_topos_point (topos, is_crane, lat, lon, n, obs, spread_m, updated_at)
                       VALUES ($1,$2,$3,$4,$5,$6,$7, now())
                     ON CONFLICT (topos) DO UPDATE SET
                       is_crane=$2, lat=$3, lon=$4, n=$5, obs=$6, spread_m=$7, updated_at=now()",
                )
                .bind(topos)
                .bind(is_crane_code(topos))
                .bind(c.lat)
                .bind(c.lon)
                .bind(c.n as i32)
                .bind(c.obs as i64)
                .bind(c.spread_m())
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
            let _ = sqlx::query("DELETE FROM learn_topos_metric WHERE captured_at < now() - interval '30 days'")
                .execute(&pool)
                .await;

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
            let _ = sqlx::query("DELETE FROM tt_soon_idle_metric WHERE captured_at < now() - interval '30 days'")
                .execute(&pool)
                .await;

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
            let _ = sqlx::query("DELETE FROM learn_lane_metric WHERE captured_at < now() - interval '30 days'")
                .execute(&pool)
                .await;
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
                let _ = sqlx::query("DELETE FROM zone_density WHERE ts < now() - interval '4 days'")
                    .execute(&pool)
                    .await;
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
            let mut qc_rows: Vec<(String, i64, bool, bool, bool, bool, i32, Option<String>, Option<String>)> =
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
                // fresh TTs NOT currently arrived at a crane = available-ish trucks (location control)
                let tt_free: Vec<(f64, f64)> = map
                    .values()
                    .filter(|p| p.cls == "TT" && (now - p.last_seen_ms) / 1000 <= STALE_AFTER_S
                        && p.arrival.as_deref() != Some("ARRIVED"))
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
                    let near_idle_tt = tt_free.iter().filter(|&&t| dist_m(cp, t) <= NEAR_TT_M).count() as i32;
                    let (nv, nq) = next_q.get(id.as_str()).cloned().unwrap_or((None, None));
                    qc_rows.push((
                        id.clone(), idle_s, gps_starv, topos_starv, pend,
                        idle_s > QCQ_IDLE_S && gps_starv && pend, near_idle_tt, nv, nq,
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
                let _ = sqlx::query(
                    "INSERT INTO qc_wait_qc_sample
                       (ts, qc, idle_s, no_truck_gps, no_truck_topos, pending, starving_real, near_idle_tt, next_vessel, next_queuename)
                     SELECT $1, * FROM unnest($2::text[], $3::int[], $4::bool[], $5::bool[], $6::bool[], $7::bool[], $8::int[], $9::text[], $10::text[])
                     ON CONFLICT (ts, qc) DO NOTHING",
                )
                .bind(ts)
                .bind(&qa).bind(&ia).bind(&nga).bind(&nta).bind(&pea).bind(&sra).bind(&nia).bind(&nva).bind(&nqa)
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
                let _ = sqlx::query("DELETE FROM qc_wait_sample WHERE ts < now() - interval '14 days'")
                    .execute(&pool)
                    .await;
                // 21d to match dispatch_pred_sample so the join window isn't truncated
                let _ = sqlx::query("DELETE FROM qc_wait_qc_sample WHERE ts < now() - interval '21 days'")
                    .execute(&pool)
                    .await;
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
        let off = wp_core::shift::terminal_offset();
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
            let today = wp_core::shift::current(wp_core::shift::terminal_now().naive_local()).0;
            let mut daily: HashMap<chrono::NaiveDate, (i64, i64)> = HashMap::new(); // bd → (sum, n)
            let mut byshift: HashMap<(chrono::NaiveDate, &'static str), (i64, i64)> = HashMap::new();
            for (ts, sr) in &rows {
                let (bd, sh) = wp_core::shift::current(ts.with_timezone(&off).naive_local());
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
                let Some(sh) = wp_core::shift::Shift::from_label(sh_label) else { continue };
                let window_start = wp_core::shift::terminal_to_utc(wp_core::shift::window(*bd, sh).0);
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
            let _ = sqlx::query("DELETE FROM learn_travel_sample WHERE captured_at < now() - interval '30 days'")
                .execute(&pool)
                .await;
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
            let _ = sqlx::query("DELETE FROM learn_travel_metric WHERE captured_at < now() - interval '30 days'")
                .execute(&pool)
                .await;
        }
    });
}

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
            let (bd, sh) = wp_core::shift::current(wp_core::shift::terminal_now().naive_local());
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
    let cls: String = id.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
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
                let (full, pre) = (t.to_string(), block_prefix(t).to_string());
                let mut c = lm.centroids.write().await;
                c.entry(full).or_default().push(lat, lon);
                if pre != t {
                    c.entry(pre).or_default().push(lat, lon);
                }
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
