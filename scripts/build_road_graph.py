#!/usr/bin/env python3
# Stage 2: skeleton -> routable graph. Loads /tmp/road_raster.npz, builds networkx graph (nodes=junctions,
# edges=road segments with geometry+length), converts px->lat/lon, visualizes, saves /tmp/road_graph.json.
import numpy as np, json, math, matplotlib
matplotlib.use('Agg'); import matplotlib.pyplot as plt
import sknw, networkx as nx
d = np.load('/tmp/road_raster.npz')
skel, dens = d['skel'], d['dens']
la0, lo0, mlat, mlon, cell = float(d['la0']), float(d['lo0']), float(d['mlat']), float(d['mlon']), float(d['cell'])
G = sknw.build_sknw(skel.astype(np.uint16), multi=True)   # nodes have 'o'=(r,c); edges have 'pts'=Nx2 (r,c)
def to_ll(r, c): return (la0 + r * cell / mlat, lo0 + c * cell / mlon)
def seg_len_m(pts):
    tot = 0.0
    for (r0, c0), (r1, c1) in zip(pts, pts[1:]):
        tot += math.hypot((r1 - r0) * cell, (c1 - c0) * cell)
    return tot
nodes = {int(n): to_ll(*G.nodes[n]['o']) for n in G.nodes}
edges = []
for u, v, k in G.edges(keys=True):
    pts = G[u][v][k]['pts']
    edges.append({'u': int(u), 'v': int(v), 'len_m': round(seg_len_m(pts), 1),
                  'geom': [[round(la0 + r * cell / mlat, 7), round(lo0 + c * cell / mlon, 7)] for r, c in pts[::3]]})
total_m = sum(e['len_m'] for e in edges)
json.dump({'nodes': nodes, 'edges': edges}, open('/tmp/road_graph.json', 'w'))
# visualize
H, W = skel.shape
fig, ax = plt.subplots(figsize=(W / 80, H / 80), dpi=80)
ax.imshow(np.log1p(dens), origin='lower', cmap='gray')
for u, v, k in G.edges(keys=True):
    p = G[u][v][k]['pts']; ax.plot(p[:, 1], p[:, 0], '#39d0ff', lw=0.6)
for n in G.nodes: r, c = G.nodes[n]['o']; ax.plot(c, r, 'o', ms=1.6, c='#ff3b3b')
ax.set_title(f'GPS-inferred road GRAPH: {G.number_of_nodes()} nodes, {G.number_of_edges()} edges, {total_m/1000:.1f}km', fontsize=8)
ax.axis('off'); plt.savefig('/tmp/road_graph.png', bbox_inches='tight', pad_inches=0.05)
print(f'nodes {G.number_of_nodes()} | edges {G.number_of_edges()} | total {total_m/1000:.1f}km | saved /tmp/road_graph.{{json,png}}')
