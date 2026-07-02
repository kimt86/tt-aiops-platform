#!/usr/bin/env python3
# Gate: does the DENSIFIED + CONNECTOR road-network route distance predict empty-trip 순수주행(segment)
# time better than rotated-grid Manhattan? corr on the segment-time label + snap distance.
# Inputs: {SCRATCH}/road_graph.json (built) + {SCRATCH}/od_sample.tsv (olat,olon,dlat,dlon,travel_s).
import json, math, os, csv
import numpy as np, networkx as nx
from scipy.spatial import cKDTree
SCRATCH = os.environ.get('ROADSCRATCH', '/tmp')
g = json.load(open(f'{SCRATCH}/road_graph.json'))
nodes = {int(k): v for k, v in g['nodes'].items()}
Gr = nx.Graph()
for e in g['edges']:
    Gr.add_edge(e['u'], e['v'], w=e['len_m'])
ids = list(nodes)
clat = float(np.mean([nodes[n][0] for n in ids])); mlon = 111320.0 * math.cos(math.radians(clat))
tree = cKDTree(np.array([[nodes[n][1] * mlon, nodes[n][0] * 111320.0] for n in ids]))
def snap(lat, lon):
    dd, j = tree.query([lon * mlon, lat * 111320.0]); return ids[j], float(dd)
GC, GS = 0.86777, 0.49697
def gman(olat, olon, dlat, dlon):
    dn = (dlat - olat) * 111320.0; de = (dlon - olon) * 111320.0 * math.cos(math.radians((olat + dlat) / 2))
    return abs(dn * GC + de * GS) + abs(-dn * GS + de * GC)

rows = [r for r in csv.reader(open(f'{SCRATCH}/od_sample.tsv'), delimiter='\t') if len(r) >= 5]
route, manh, ts, snapd = [], [], [], []
for r in rows:
    olat, olon, dlat, dlon, tsec = (float(x) for x in r[:5])
    so, ddo = snap(olat, olon); sd, ddd = snap(dlat, dlon)
    try:
        rd = nx.shortest_path_length(Gr, so, sd, weight='w')
    except (nx.NetworkXNoPath, nx.NodeNotFound):
        continue
    if rd <= 0: continue
    route.append(rd); manh.append(gman(olat, olon, dlat, dlon)); ts.append(tsec); snapd.append(max(ddo, ddd))
route, manh, ts, snapd = (np.array(x) for x in (route, manh, ts, snapd))
c = lambda a, b: float(np.corrcoef(a, b)[0, 1])
print(f'n {len(ts)} | ROUTE↔time {c(route, ts):.3f} | MANHATTAN↔time {c(manh, ts):.3f} '
      f'| route↔manh {c(route, manh):.3f} | snap med {np.median(snapd):.0f}m p90 {np.percentile(snapd, 90):.0f}m '
      f'| route/manh ratio med {np.median(route / np.maximum(manh, 1)):.2f}')
