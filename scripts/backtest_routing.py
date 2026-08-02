#!/usr/bin/env python3
# Stage 3-4: does GPS-inferred-graph shortest-path predict pure drive time better than Manhattan?
# Pixel-level routing graph from skeleton + Dijkstra. Compares graph-path / Manhattan / straight vs actual drive_s.
import numpy as np, math, csv, os, sys
from scipy.spatial import cKDTree
from scipy.sparse import csr_matrix
from scipy.sparse.csgraph import dijkstra

# Same scratch convention as the producers (infer_road_network.py, reinfer_roadgraph.sh). These used
# to read /tmp, which is a 124GB tmpfs — RAM — on this host; commit 5a24c7e moved the WRITERS to
# /var/tmp/roadscratch and left these readers pointing at the old place, so they have been looking at
# a directory nothing writes to since.
SCRATCH = os.environ.get('ROADSCRATCH', '/var/tmp/roadscratch')

def need(path, how):
    """Fail with the path and how to produce it. A bare FileNotFoundError here sends you hunting for
    a file whose location just moved."""
    if not os.path.exists(path):
        sys.exit(f'missing {path}\n  produce it with: {how}\n  (override the directory with ROADSCRATCH)')
    return path

RASTER = need(f'{SCRATCH}/road_raster.npz', 'scripts/infer_road_network.py')
# legs_od.tsv has no producer in this repo — it is a hand-made dump. Columns, tab separated, no header:
#   origin_lat  origin_lon  dest_lat  dest_lon  drive_seconds
LEGS = need(f'{SCRATCH}/legs_od.tsv',
            "psql -tAF'\\t' -c 'SELECT o_lat,o_lon,d_lat,d_lon,drive_s FROM <your leg query>' "
            f"> {SCRATCH}/legs_od.tsv")

d = np.load(RASTER)
skel = d['skel']; la0, lo0, mlat, mlon, cell = map(float, (d['la0'], d['lo0'], d['mlat'], d['mlon'], d['cell']))
COS, SIN = 0.86777, 0.49697            # quay-axis rotation (livemap GRID_COS/SIN)
ys, xs = np.where(skel)                 # skeleton pixels (row=y/lat, col=x/lon)
idx = {(int(r), int(c)): i for i, (r, c) in enumerate(zip(ys, xs))}
nodem = np.column_stack([xs * cell, ys * cell]).astype(np.float64)   # metres
tree = cKDTree(nodem)
# 8-neighbour edges
I, J, Wt = [], [], []
for (r, c), i in idx.items():
    for dr in (-1, 0, 1):
        for dc in (-1, 0, 1):
            if dr == 0 and dc == 0: continue
            j = idx.get((r + dr, c + dc))
            if j is not None:
                I.append(i); J.append(j); Wt.append(math.hypot(dr, dc) * cell)
A = csr_matrix((Wt, (I, J)), shape=(len(idx), len(idx)))
def merc(la, lo): return ((lo - lo0) * mlon, (la - la0) * mlat)
# load legs, snap
legs = []
for row in csv.reader(open(LEGS), delimiter='\t'):
    if len(row) < 5: continue
    ola, olo, dla, dlo, ds = map(float, row)
    ox, oy = merc(ola, olo); dx, dy = merc(dla, dlo)
    do, io = tree.query([ox, oy]); dd, jd = tree.query([dx, dy])
    if do > 40 or dd > 40: continue                       # off the inferred network
    dn = dy - oy; de = dx - ox
    manh = abs(dn * COS + de * SIN) + abs(-dn * SIN + de * COS)
    strt = math.hypot(dn, de)
    legs.append([int(io), int(jd), ds, manh, strt])
legs = np.array(legs, dtype=np.float64)
# group by source node → one dijkstra per unique source
gpath = np.full(len(legs), np.nan)
srcs = legs[:, 0].astype(int)
for s in np.unique(srcs):
    dist = dijkstra(A, indices=s)
    sel = np.where(srcs == s)[0]
    for k in sel:
        gpath[k] = dist[int(legs[k, 1])]
ok = np.isfinite(gpath) & (gpath < 1e9)
ds = legs[ok, 2]; manh = legs[ok, 3]; strt = legs[ok, 4]; gp = gpath[ok]
def stats(name, dist):
    r = np.corrcoef(dist, ds)[0, 1]
    sp = dist.sum() / ds.sum()                  # m per s (single fitted speed)
    pred = dist / sp
    mae = np.mean(np.abs(pred - ds))
    mape = np.mean(np.abs(pred - ds) / ds) * 100
    print(f'  {name:10s}  corr {r:.3f}  MAE {mae:5.1f}s  MAPE {mape:4.1f}%  (fit {sp*3.6:.1f}km/h)')
print(f'legs total {len(legs)}, routable(both snap<40m) {ok.sum()} ({100*ok.sum()/len(legs):.0f}%)')
stats('graph', gp); stats('manhattan', manh); stats('straight', strt)
