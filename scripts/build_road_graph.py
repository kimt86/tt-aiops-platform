#!/usr/bin/env python3
# Stage 2: skeleton -> routable graph, DENSIFIED + WORK-POINT CONNECTORS.
#   · densify: split each skeleton edge into ~SEG_M sub-edges (adds intermediate nodes) so an OD
#     endpoint can attach to a NEARBY node instead of a distant junction.
#   · connectors: every work point (learn_topos_point) becomes a node + a connector edge to the nearest
#     road node → OD endpoints are ON the graph (100% snap; route = connector + lanes + connector).
# Loads {SCRATCH}/road_raster.npz + {SCRATCH}/workpoints.tsv -> {SCRATCH}/road_graph.json.
import numpy as np, json, math, os, csv
import sknw
from scipy.spatial import cKDTree
SCRATCH = os.environ.get('ROADSCRATCH', '/var/tmp/roadscratch')
SEG_M = 35.0     # densify: max sub-edge length in metres

d = np.load(f'{SCRATCH}/road_raster.npz')
skel = d['skel']
la0, lo0, mlat, mlon, cell = float(d['la0']), float(d['lo0']), float(d['mlat']), float(d['mlon']), float(d['cell'])
G = sknw.build_sknw(skel.astype(np.uint16), multi=True)
def to_ll(r, c): return (la0 + r * cell / mlat, lo0 + c * cell / mlon)
def m_between(a, b): return math.hypot((b[0] - a[0]) * mlat, (b[1] - a[1]) * mlon)

base_nodes = {int(n): to_ll(*G.nodes[n]['o']) for n in G.nodes}
next_id = (max(base_nodes) + 1) if base_nodes else 0
nodes = dict(base_nodes)
edges = []   # {u, v, len_m, geom, connector}

# ── base skeleton edges → densified sub-edges (intermediate nodes every ~SEG_M) ──
# each sub-edge carries pid = its parent skeleton edge, so direction/one-way can be decided PER
# PARENT (majority over the whole road) — per-sub-edge orientation from single noisy lane cells
# fragments the directed graph (adjacent 35m pieces flip against each other → no path).
for pid, (u, v, k) in enumerate(G.edges(keys=True)):
    geom = [to_ll(r, c) for r, c in G[u][v][k]['pts']]
    if len(geom) < 2:
        continue
    # networkx MultiGraph reports (u,v) in node-id order, which can be the REVERSE of the sknw
    # trace order of 'pts' — then geom would run v→u while the sub-edge chain is built u→…→v,
    # mirroring intermediate node coordinates along the road (wrong snap positions) and inverting
    # the parent's one-way flow decision downstream. Align geom to start at u's coordinate.
    uo = to_ll(*G.nodes[u]['o'])
    if m_between(geom[0], uo) > m_between(geom[-1], uo):
        geom = geom[::-1]
    prev, seg, acc = int(u), [geom[0]], 0.0
    for i in range(1, len(geom)):
        acc += m_between(geom[i - 1], geom[i]); seg.append(geom[i])
        if acc >= SEG_M and i < len(geom) - 1:
            nid = next_id; next_id += 1
            nodes[nid] = geom[i]
            edges.append({'u': prev, 'v': nid, 'len_m': round(acc, 1), 'pid': pid,
                          'geom': [[round(a, 7), round(o, 7)] for a, o in seg], 'connector': False})
            prev, seg, acc = nid, [geom[i]], 0.0
    edges.append({'u': prev, 'v': int(v), 'len_m': round(acc, 1), 'pid': pid,
                  'geom': [[round(a, 7), round(o, 7)] for a, o in seg], 'connector': False})

# ── work-point connectors: attach each work point to the nearest (dense) road node ──
nid_list = list(nodes)
xy = np.array([[nodes[n][1] * mlon, nodes[n][0] * mlat] for n in nid_list])
tree = cKDTree(xy)
n_conn, conn_len = 0, []
try:
    wp = [r for r in csv.reader(open(f'{SCRATCH}/workpoints.tsv'), delimiter='\t') if len(r) >= 3]
except FileNotFoundError:
    wp = []
for r in wp:
    wlat, wlon = float(r[0]), float(r[1])
    dd, j = tree.query([wlon * mlon, wlat * mlat])
    near = nid_list[j]
    wid = next_id; next_id += 1
    nodes[wid] = (wlat, wlon)
    edges.append({'u': wid, 'v': near, 'len_m': round(float(dd), 1),
                  'geom': [[round(wlat, 7), round(wlon, 7)],
                           [round(nodes[near][0], 7), round(nodes[near][1], 7)]], 'connector': True})
    n_conn += 1; conn_len.append(float(dd))

json.dump({'nodes': {str(k): [round(v[0], 7), round(v[1], 7)] for k, v in nodes.items()}, 'edges': edges},
          open(f'{SCRATCH}/road_graph.json', 'w'))
cl = np.array(conn_len) if conn_len else np.array([0.0])
print(f'nodes {len(nodes)} (base {len(base_nodes)}) | edges {len(edges)} | connectors {n_conn} '
      f'| connector med {np.median(cl):.0f}m p90 {np.percentile(cl, 90):.0f}m '
      f'| total {sum(e["len_m"] for e in edges) / 1000:.1f}km')
