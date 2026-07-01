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
use std::collections::BinaryHeap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// pure-drive speed (km/h) for edges missing a learned speed — the measured leg-level moving speed.
const FALLBACK_KMH: f64 = 22.8;
/// reject a snap if the nearest node is farther than this (truck/work not on the road network).
const SNAP_MAX_M: f64 = 60.0;

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
    /// so callers can fall back to the geometric estimate.
    pub async fn load(pool: &PgPool) -> Option<RoadGraph> {
        let nodes_raw: Vec<(i32, f64, f64)> =
            sqlx::query_as("SELECT id, lat, lon FROM road_node ORDER BY id")
                .fetch_all(pool)
                .await
                .ok()?;
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
        let edges: Vec<(i32, i32, f64, Option<f64>, bool)> =
            sqlx::query_as("SELECT from_id, to_id, len_m, speed_kmh, oneway FROM road_edge")
                .fetch_all(pool)
                .await
                .ok()?;
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
        let mlat = 111_320.0;
        let mlon = 111_320.0 * lat.to_radians().cos();
        let mut best: Option<u32> = None;
        let mut bestd = SNAP_MAX_M * SNAP_MAX_M;
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

/// SHADOW validation (mig 0078): every 10min, route each recent settled leg (origin→dest) on the road
/// graph and log route time/distance next to the drive_s label → `road_route_eval`. This is the GATE
/// before wiring routing into the cost: does the topology-aware route predict drive_s better than the
/// Manhattan baseline (computed from the same coords)? Snap-fail rate also lands here.
pub fn spawn_roadgraph_eval(pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(600));
        loop {
            ticker.tick().await;
            let Some(rg) = RoadGraph::load(&pool).await else { continue };
            let legs: Vec<(String, DateTime<Utc>, i32, f64, f64, f64, f64)> = sqlx::query_as(
                "SELECT ytno, leg_start, drive_s, origin_lat, origin_lon, dest_lat, dest_lon
                   FROM learn_leg_decomp d
                  WHERE origin_lat IS NOT NULL AND dest_lat IS NOT NULL AND drive_s > 10
                    AND captured_at > now() - interval '20 minutes'
                    AND NOT EXISTS (SELECT 1 FROM road_route_eval e WHERE e.ytno = d.ytno AND e.leg_start = d.leg_start)
                  LIMIT 4000",
            )
            .fetch_all(&pool)
            .await
            .unwrap_or_default();
            if legs.is_empty() {
                continue;
            }
            let mut yt = Vec::new();
            let mut ls = Vec::new();
            let mut ds = Vec::new();
            let mut rt: Vec<Option<i32>> = Vec::new();
            let mut rd: Vec<Option<i32>> = Vec::new();
            let mut sn = Vec::new();
            for (ytno, leg_start, drive_s, ola, olo, dla, dlo) in legs {
                let r = rg.route(ola, olo, dla, dlo);
                yt.push(ytno);
                ls.push(leg_start);
                ds.push(drive_s);
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
                "INSERT INTO road_route_eval (ytno, leg_start, drive_s, route_time_s, route_dist_m, snapped)
                 SELECT * FROM UNNEST($1::text[], $2::timestamptz[], $3::int[], $4::int[], $5::int[], $6::bool[])",
            )
            .bind(&yt)
            .bind(&ls)
            .bind(&ds)
            .bind(&rt)
            .bind(&rd)
            .bind(&sn)
            .execute(&pool)
            .await;
        }
    });
}
