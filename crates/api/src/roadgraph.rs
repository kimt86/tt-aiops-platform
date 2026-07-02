//! Directed road-graph router (mig 0077). Loads the inferred road network (`road_node`/`road_edge`,
//! rebuilt hourly by `reinfer_roadgraph.sh` — skeleton links oriented to the learned lane flow, with
//! one-way + learned speed) and routes truck→work on it. This replaces the 225m grid, which cannot
//! represent narrow bridges / separated one-way lanes (a grid cell says two points across a gap are
//! adjacent, but the real path detours via a distant bridge — the graph gets that topology right).
//!
//! The graph is tiny (~264 nodes / ~353 edges), so: snap by linear nearest-node scan, and a
//! binary-heap Dijkstra on TIME (edge weight = len ÷ learned speed). A route returns both seconds
//! (for a cost estimate) and metres (the clean topology-aware distance = the model's route feature).

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::livemap::quay_manhattan_m;

/// pure-drive speed (km/h) for edges missing a learned speed — the measured leg-level moving speed.
const FALLBACK_KMH: f64 = 22.8;
/// reject a snap if the nearest node is farther than this (truck/work not on the road network).
const SNAP_MAX_M: f64 = 60.0;
/// wider snap for a route SOURCE (= a truck's raw GPS): idle trucks park in staging pockets off the
/// inferred lanes (the graph is built from MOVING gps only), but they re-enter a lane to drive — so
/// route from the nearest lane node; the short off-road stub is absorbed by the cost calibration.
const SNAP_SRC_MAX_M: f64 = 150.0;

pub struct RoadGraph {
    nodes: Vec<(f64, f64)>,             // node id (index) → (lat, lon); (0,0) marks an id gap
    adj: Vec<Vec<(u32, f64, f64)>>,     // node id → [(to, time_s, dist_m)]
}

pub struct Route {
    pub time_s: f64,
    pub dist_m: f64,
}

impl RoadGraph {
    /// Load from the DB (rebuilt hourly by the cron). Returns None if the graph is empty/unavailable
    /// so callers can fall back to the geometric estimate. Both tables are read from ONE repeatable-
    /// read snapshot: the cron swaps them atomically (TRUNCATE+COPY in one tx) and node ids are
    /// renumbered every build, so two autocommit reads straddling the swap would silently pair old
    /// node coordinates with new edge topology (chimera graph → wrong routes, poisoned eval rows).
    pub async fn load(pool: &PgPool) -> Option<RoadGraph> {
        let mut tx = pool.begin().await.ok()?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await
            .ok()?;
        let nodes_raw: Vec<(i32, f64, f64)> =
            sqlx::query_as("SELECT id, lat, lon FROM road_node ORDER BY id")
                .fetch_all(&mut *tx)
                .await
                .ok()?;
        let edges: Vec<(i32, i32, f64, Option<f64>, bool)> =
            sqlx::query_as("SELECT from_id, to_id, len_m, speed_kmh, oneway FROM road_edge")
                .fetch_all(&mut *tx)
                .await
                .ok()?;
        let _ = tx.commit().await;
        if nodes_raw.is_empty() {
            return None;
        }
        let maxid = nodes_raw.iter().map(|r| r.0).max().unwrap_or(0).max(0) as usize;
        let mut nodes = vec![(0.0f64, 0.0f64); maxid + 1];
        for (id, la, lo) in &nodes_raw {
            if *id >= 0 {
                nodes[*id as usize] = (*la, *lo);
            }
        }
        let mut adj: Vec<Vec<(u32, f64, f64)>> = vec![Vec::new(); maxid + 1];
        for (u, v, len_m, spd, oneway) in edges {
            if u < 0 || v < 0 || u as usize > maxid || v as usize > maxid {
                continue;
            }
            let (u, v) = (u as usize, v as usize);
            let kmh = spd.filter(|s| *s > 1.0).unwrap_or(FALLBACK_KMH);
            let t = len_m / (kmh / 3.6); // seconds
            adj[u].push((v as u32, t, len_m));
            if !oneway {
                adj[v].push((u as u32, t, len_m));
            }
        }
        Some(RoadGraph { nodes, adj })
    }

    pub fn n_nodes(&self) -> usize {
        self.nodes.iter().filter(|(la, _)| *la != 0.0).count()
    }
    pub fn n_edges(&self) -> usize {
        self.adj.iter().map(|a| a.len()).sum()
    }

    /// nearest node within SNAP_MAX_M, or None.
    fn snap(&self, lat: f64, lon: f64) -> Option<u32> {
        self.snap_r(lat, lon, SNAP_MAX_M)
    }

    /// nearest node within `max_m`, or None.
    fn snap_r(&self, lat: f64, lon: f64, max_m: f64) -> Option<u32> {
        let mlat = 111_320.0;
        let mlon = 111_320.0 * lat.to_radians().cos();
        let mut best: Option<u32> = None;
        let mut bestd = max_m * max_m;
        for (i, (nla, nlo)) in self.nodes.iter().enumerate() {
            if *nla == 0.0 {
                continue; // id gap
            }
            let dn = (nla - lat) * mlat;
            let de = (nlo - lon) * mlon;
            let d2 = dn * dn + de * de;
            if d2 < bestd {
                bestd = d2;
                best = Some(i as u32);
            }
        }
        best
    }

    /// Public snap: nearest node within SNAP_MAX_M (work points are ON the graph via connectors, so
    /// they snap at ~0 m; a truck's raw GPS snaps to the densified lane nodes).
    pub fn snap_node(&self, lat: f64, lon: f64) -> Option<u32> {
        self.snap(lat, lon)
    }

    /// Single-source Dijkstra by TIME from a coordinate, over the whole graph. One call serves every
    /// target the matcher will ask about for this truck (targets × O(1) lookups instead of targets ×
    /// Dijkstra). Source = raw truck GPS → wider SNAP_SRC_MAX_M. None if it doesn't snap.
    pub fn field_from(&self, lat: f64, lon: f64) -> Option<RouteField> {
        let s = self.snap_r(lat, lon, SNAP_SRC_MAX_M)?;
        let n = self.nodes.len();
        let mut best_t = vec![f64::INFINITY; n];
        best_t[s as usize] = 0.0;
        let mut heap = BinaryHeap::new();
        heap.push(HeapItem { t: 0.0, node: s });
        while let Some(HeapItem { t: ct, node }) = heap.pop() {
            if ct > best_t[node as usize] {
                continue;
            }
            for &(to, et, _em) in &self.adj[node as usize] {
                let nt = ct + et;
                if nt < best_t[to as usize] {
                    best_t[to as usize] = nt;
                    heap.push(HeapItem { t: nt, node: to });
                }
            }
        }
        Some(RouteField { time_s: best_t })
    }

    /// Route by minimum TIME. Returns time_s + the distance along the chosen path. None if either
    /// endpoint doesn't snap to the network or no directed path exists.
    pub fn route(&self, alat: f64, alon: f64, blat: f64, blon: f64) -> Option<Route> {
        let s = self.snap(alat, alon)?;
        let t = self.snap(blat, blon)?;
        if s == t {
            return Some(Route { time_s: 0.0, dist_m: 0.0 });
        }
        let n = self.nodes.len();
        let mut best_t = vec![f64::INFINITY; n];
        let mut path_m = vec![0.0f64; n];
        best_t[s as usize] = 0.0;
        let mut heap = BinaryHeap::new();
        heap.push(HeapItem { t: 0.0, node: s });
        while let Some(HeapItem { t: ct, node }) = heap.pop() {
            if node == t {
                return Some(Route { time_s: ct, dist_m: path_m[node as usize] });
            }
            if ct > best_t[node as usize] {
                continue;
            }
            for &(to, et, em) in &self.adj[node as usize] {
                let nt = ct + et;
                if nt < best_t[to as usize] {
                    best_t[to as usize] = nt;
                    path_m[to as usize] = path_m[node as usize] + em;
                    heap.push(HeapItem { t: nt, node: to });
                }
            }
        }
        None
    }
}

/// Per-source shortest-time field (seconds to every node) from `field_from`.
pub struct RouteField {
    time_s: Vec<f64>,
}
impl RouteField {
    /// seconds to the node, or None if unreachable on the directed graph.
    pub fn time_to(&self, node: u32) -> Option<f64> {
        let t = *self.time_s.get(node as usize)?;
        t.is_finite().then_some(t)
    }
}

/// Cold-start multipliers if road_route_eval is too thin to fit curves: raw route time is a PHYSICS
/// estimate (edge len ÷ lane moving speed) so it under-counts the realized 순수주행 구간시간.
const CAL50_DEFAULT: f64 = 1.6;
const CAL90_DEFAULT: f64 = 3.7;

/// A monotone estimator→ACTUAL cost curve fitted from `road_route_eval`: knots at each bin's mean
/// estimator value, knot values = the bin's realized p50/p90 seconds (DIRECT levels, not ratios).
/// Review found two defects in the earlier ratio×t form: (a) t×ratio(t) went NON-MONOTONE where the
/// ratio falls steeply with scale (a nearer truck could cost MORE than a farther one, inverting both
/// the objective and the p90 deadline filter), and (b) short routes lost their realized ~75-90s
/// floor (a 15s raw route was costed ~21s, over-favouring trivially-near picks and inflating the
/// fair-compare savings). Direct levels + a cumulative-max clamp fix both by construction: held
/// flat below the first knot (the floor), linear between knots, last-slope extrapolation above.
struct CostCurve {
    knots: Vec<(f64, f64, f64)>, // (estimator value, actual p50_s, actual p90_s), all monotone ↑
}

impl CostCurve {
    fn fit(mut rows: Vec<(f64, f64, f64, i64)>, min_n: i64) -> Option<CostCurve> {
        rows.retain(|r| r.3 >= min_n && r.0.is_finite() && r.1 > 0.0);
        rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
        let mut knots: Vec<(f64, f64, f64)> = Vec::with_capacity(rows.len());
        let (mut m50, mut m90) = (0.0_f64, 0.0_f64);
        for (x, p50, p90, _n) in rows {
            m50 = m50.max(p50);
            m90 = m90.max(p90).max(m50);
            knots.push((x, m50, m90));
        }
        (knots.len() >= 2).then_some(CostCurve { knots })
    }

    fn eval(&self, x: f64) -> (f64, f64) {
        let k = &self.knots;
        if x <= k[0].0 {
            return (k[0].1, k[0].2); // short-range floor
        }
        for w in k.windows(2) {
            let (a, b) = (w[0], w[1]);
            if x <= b.0 {
                let f = (x - a.0) / (b.0 - a.0);
                return (a.1 + f * (b.1 - a.1), a.2 + f * (b.2 - a.2));
            }
        }
        let (a, b) = (k[k.len() - 2], k[k.len() - 1]);
        let span = (b.0 - a.0).max(1.0);
        let extra = x - b.0;
        (b.1 + (b.1 - a.1) / span * extra, b.2 + (b.2 - a.2) / span * extra)
    }
}

/// The dispatch OD cost: road-network ROUTE TIME mapped through a learned route→actual curve.
/// Replaces the 225m grid lookup (dropped, mig 0082) — the router answers every pair (work points
/// are graph nodes via connectors), and the graph has headroom the Manhattan formula lacks (lane
/// speeds, one-ways, congestion) plus it generalizes to non-grid terminals. A SECOND curve maps
/// Manhattan→actual for the L3 fallback so unroutable pairs sit on the SAME realized scale as
/// routed ones (review: raw manh÷speed ran +57s hot vs routed costs and its p90 was incomparable).
/// Per-tick object: caches one Dijkstra field per truck and one snap per coordinate (Mutex so
/// matcher futures holding &RouteCost across awaits stay Send).
pub struct RouteCost {
    rg: Option<RoadGraph>,
    route: JobCurves, // route-time → realized, per job type (DS crane-pickup vs LD block-pickup differ)
    manh: JobCurves,  // Manhattan → realized (L3 fallback), per job type
    fields: Mutex<HashMap<(i64, i64), Option<Arc<RouteField>>>>,
    snaps: Mutex<HashMap<(i64, i64), Option<u32>>>,
}

/// One cost curve per job type + a combined fallback. The cycle audit measured DS empty trips
/// (pickup = crane) running p50 ~1.5× / p90 ~2× longer than LD (pickup = block) at the SAME route
/// time, and DS is ~73% of samples — so a single pooled curve over-costs LD and under-costs DS
/// (worst at p90, which gates the deadline filter). Split by job type; `all` covers cold start,
/// other job types, or a per-type bin too thin to fit.
#[derive(Default)]
struct JobCurves {
    ds: Option<CostCurve>,
    ld: Option<CostCurve>,
    all: Option<CostCurve>,
}
impl JobCurves {
    fn pick(&self, is_ld: bool) -> Option<&CostCurve> {
        (if is_ld { self.ld.as_ref() } else { self.ds.as_ref() }).or(self.all.as_ref())
    }
}

/// Fit one estimator→actual curve from road_route_eval, optionally restricted to a job type.
/// `xcol`/`base_where`/`bins` are code constants (never user input) so the format! is injection-safe.
async fn fit_cost_curve(
    pool: &PgPool, xcol: &str, base_where: &str, bins: &str, jt: Option<&str>,
) -> Option<CostCurve> {
    let jt_clause = jt.map(|j| format!(" AND jobtype = '{j}'")).unwrap_or_default();
    let sql = format!(
        "SELECT avg({xcol})::float8,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY actual_s)::float8,
                percentile_cont(0.9) WITHIN GROUP (ORDER BY actual_s)::float8,
                count(*)
           FROM road_route_eval
          WHERE {base_where} AND actual_s BETWEEN 10 AND 3600 AND ts > now() - interval '14 days'{jt_clause}
          GROUP BY width_bucket({xcol}::float8, ARRAY[{bins}])"
    );
    let rows: Vec<(Option<f64>, Option<f64>, Option<f64>, i64)> =
        sqlx::query_as(&sql).fetch_all(pool).await.unwrap_or_default();
    let clean: Vec<(f64, f64, f64, i64)> =
        rows.into_iter().filter_map(|(x, p50, p90, n)| Some((x?, p50?, p90?, n))).collect();
    CostCurve::fit(clean, if jt.is_some() { 100 } else { 150 })
}

impl RouteCost {
    /// Load the graph + fit both cost curves from the eval table. Never fails — with no graph every
    /// route lookup returns None and callers use `manh_p50_p90` / the geometric fallback.
    pub async fn load(pool: &PgPool) -> RouteCost {
        let rg = RoadGraph::load(pool).await;
        // route→actual (routed rows, binned by raw route time — dense at the short end) and
        // Manhattan→actual (all rows, the L3 population), each fitted SEPARATELY for DS / LD, with a
        // combined `all` fallback. bin edges are code constants.
        const RT_W: &str = "snapped AND route_time_s >= 5";
        const RT_B: &str = "30.0,60.0,120.0,240.0,480.0";
        const MH_W: &str = "manh_m > 0";
        const MH_B: &str = "200.0,400.0,800.0,1600.0,3200.0";
        let route = JobCurves {
            ds: fit_cost_curve(pool, "route_time_s", RT_W, RT_B, Some("DS")).await,
            ld: fit_cost_curve(pool, "route_time_s", RT_W, RT_B, Some("LD")).await,
            all: fit_cost_curve(pool, "route_time_s", RT_W, RT_B, None).await,
        };
        let manh = JobCurves {
            ds: fit_cost_curve(pool, "manh_m", MH_W, MH_B, Some("DS")).await,
            ld: fit_cost_curve(pool, "manh_m", MH_W, MH_B, Some("LD")).await,
            all: fit_cost_curve(pool, "manh_m", MH_W, MH_B, None).await,
        };
        RouteCost { rg, route, manh, fields: Mutex::default(), snaps: Mutex::default() }
    }

    /// Realized (p50_s, p90_s) for a Manhattan distance, on the SAME learned scale as the routed
    /// cost — the L3 fallback for unroutable pairs. None until the eval table has enough rows.
    pub fn manh_p50_p90(&self, manh_m: f64, is_ld: bool) -> Option<(f64, f64)> {
        Some(self.manh.pick(is_ld)?.eval(manh_m))
    }

    fn key(lat: f64, lon: f64) -> (i64, i64) {
        ((lat * 1e7) as i64, (lon * 1e7) as i64)
    }

    /// Calibrated (p50_s, p90_s) for truck→work of the given job type, or None (no graph /
    /// unsnappable / unreachable) — caller falls back to Manhattan ÷ segment speed. `is_ld` picks
    /// the LD vs DS realized curve (crane vs block pickup differ ~1.5–2×).
    pub fn p50_p90(&self, vlat: f64, vlon: f64, wlat: f64, wlon: f64, is_ld: bool) -> Option<(f64, f64)> {
        let rg = self.rg.as_ref()?;
        let field = self
            .fields
            .lock()
            .unwrap()
            .entry(Self::key(vlat, vlon))
            .or_insert_with(|| rg.field_from(vlat, vlon).map(Arc::new))
            .clone()?;
        let node = (*self
            .snaps
            .lock()
            .unwrap()
            .entry(Self::key(wlat, wlon))
            .or_insert_with(|| rg.snap_node(wlat, wlon)))?;
        let t = field.time_to(node)?;
        match self.route.pick(is_ld) {
            Some(c) => Some(c.eval(t)),
            None => Some((t * CAL50_DEFAULT, t * CAL90_DEFAULT)), // cold start
        }
    }
}

// min-heap by time (BinaryHeap is a max-heap → reverse the compare)
struct HeapItem {
    t: f64,
    node: u32,
}
impl PartialEq for HeapItem {
    fn eq(&self, o: &Self) -> bool {
        self.t == o.t
    }
}
impl Eq for HeapItem {}
impl PartialOrd for HeapItem {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for HeapItem {
    fn cmp(&self, o: &Self) -> Ordering {
        o.t.partial_cmp(&self.t).unwrap_or(Ordering::Equal)
    }
}

/// Router eval + calibration source (mig 0078/0082): every 10min, route recent EMPTY TRIPS
/// (learn_travel_sample leg_ord=0, label = 순수주행 구간시간) on the graph and log route time/distance
/// + rotated-grid Manhattan next to the label → `road_route_eval`. Two consumers: (1) the standing
/// GATE metric — does route predict the label better than Manhattan as the graph improves; (2) the
/// COST CALIBRATION — RouteCost::load derives its actual÷route p50/p90 ratios from these rows.
/// Self-backfills (batch-drains the 21-day sample window keyed by (ytno, dropped_at)).
pub fn spawn_roadgraph_eval(pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(600));
        loop {
            ticker.tick().await;
            let Some(rg) = RoadGraph::load(&pool).await else { continue };
            for _ in 0..12 {
                let legs: Vec<(String, DateTime<Utc>, i32, f64, f64, f64, f64, Option<String>)> = sqlx::query_as(
                    "SELECT s.ytno, s.dropped_at, s.travel_s,
                            s.origin_lat, s.origin_lon, s.dest_lat, s.dest_lon, c.jobtype
                       FROM learn_travel_sample s
                       JOIN tt_cycle_v2 c ON c.ytno = s.ytno AND c.dropped_at = s.dropped_at
                      WHERE s.leg_ord = 0 AND s.travel_s BETWEEN 10 AND 3600
                        AND s.origin_lat IS NOT NULL AND s.dest_lat IS NOT NULL
                        AND s.trip_ts > now() - interval '21 days'
                        AND NOT EXISTS (SELECT 1 FROM road_route_eval e
                                         WHERE e.ytno = s.ytno AND e.leg_start = s.dropped_at)
                      LIMIT 2000",
                )
                .fetch_all(&pool)
                .await
                .unwrap_or_default();
                if legs.is_empty() {
                    break;
                }
                let n = legs.len();
                let mut yt = Vec::new();
                let mut ls = Vec::new();
                let mut act = Vec::new();
                let mut rt: Vec<Option<i32>> = Vec::new();
                let mut rd: Vec<Option<i32>> = Vec::new();
                let mut sn = Vec::new();
                let mut mh = Vec::new();
                let mut jt: Vec<Option<String>> = Vec::new();
                for (ytno, dropped_at, travel_s, ola, olo, dla, dlo, jobtype) in legs {
                    let r = rg.route(ola, olo, dla, dlo);
                    yt.push(ytno);
                    ls.push(dropped_at);
                    act.push(travel_s);
                    mh.push(quay_manhattan_m(ola, olo, dla, dlo) as i32);
                    jt.push(jobtype);
                    match r {
                        Some(rr) => {
                            rt.push(Some(rr.time_s as i32));
                            rd.push(Some(rr.dist_m as i32));
                            sn.push(true);
                        }
                        None => {
                            rt.push(None);
                            rd.push(None);
                            sn.push(false);
                        }
                    }
                }
                let _ = sqlx::query(
                    "INSERT INTO road_route_eval (ytno, leg_start, actual_s, route_time_s, route_dist_m, snapped, manh_m, jobtype)
                     SELECT * FROM UNNEST($1::text[], $2::timestamptz[], $3::int[], $4::int[], $5::int[], $6::bool[], $7::int[], $8::text[])",
                )
                .bind(&yt)
                .bind(&ls)
                .bind(&act)
                .bind(&rt)
                .bind(&rd)
                .bind(&sn)
                .bind(&mh)
                .bind(&jt)
                .execute(&pool)
                .await;
                if n < 2000 {
                    break;
                }
                tokio::task::yield_now().await;
            }
            let _ = sqlx::query("DELETE FROM road_route_eval WHERE ts < now() - interval '60 days'")
                .execute(&pool)
                .await;
        }
    });
}
