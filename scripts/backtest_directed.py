#!/usr/bin/env python3
# Directed routing from EXISTING data: build a directed, speed-weighted graph straight from the learned
# lane field (learn_lane_cell: per ~22m cell → heading, one-way/two-way (directionality), mean speed).
# Edge u->v allowed if v is a road cell AND (u two-way, OR bearing(u->v) aligns with u's heading=one-way).
# Weight = travel TIME (dist / cell speed). Backtest: route O->D, predict time, vs Manhattan vs actual.
import csv, math
import numpy as np
from scipy.spatial import cKDTree
from scipy.sparse import csr_matrix
from scipy.sparse.csgraph import dijkstra

CELL_DEG = 0.0002                 # lane grid ~22m
ONEWAY_DIR = 0.6                  # directionality above this = enforce one-way (forward only)
ALIGN_DEG = 75.0                  # bearing within this of heading = "forward"
COS, SIN = 0.86777, 0.49697       # quay-axis (Manhattan baseline)

cells = {}                        # (i,j) -> (lat,lon,heading,dir,speed_mps)
for r in csv.reader(open('/tmp/lane_cells.tsv'), delimiter='\t'):
    if len(r) < 6: continue
    lat, lon, _passes, hd, di, spd = float(r[0]), float(r[1]), float(r[2]), float(r[3]), float(r[4]), float(r[5])
    i, j = round(lat / CELL_DEG), round(lon / CELL_DEG)
    cells[(i, j)] = (lat, lon, hd, di, max(5.0, spd) / 3.6)   # speed floor 5km/h → m/s

keys = list(cells)
idx = {k: n for n, k in enumerate(keys)}
la = np.array([cells[k][0] for k in keys]); lo = np.array([cells[k][1] for k in keys])
mlat = 111320.0; mlon = 111320.0 * math.cos(math.radians(float(la.mean())))
xy = np.column_stack([(lo - lo.min()) * mlon, (la - la.min()) * mlat])
tree = cKDTree(xy)

def bearing(i0, j0, i1, j1):       # compass bearing of cell step (lat=i, lon=j)
    dn = (i1 - i0); de = (j1 - j0)
    return (math.degrees(math.atan2(de, dn))) % 360.0

def adiff(a, b):
    d = abs(a - b) % 360.0
    return min(d, 360.0 - d)

I, J, W = [], [], []
for (i, j), (lat, lon, hd, di, spd) in cells.items():
    u = idx[(i, j)]
    for di_ in (-1, 0, 1):
        for dj_ in (-1, 0, 1):
            if di_ == 0 and dj_ == 0: continue
            v = idx.get((i + di_, j + dj_))
            if v is None: continue
            if di > ONEWAY_DIR and adiff(bearing(i, j, i + di_, j + dj_), hd) > ALIGN_DEG:
                continue           # one-way cell: skip backward steps
            dist = math.hypot(di_ * CELL_DEG * mlat, dj_ * CELL_DEG * mlon)
            W.append(dist / spd); I.append(u); J.append(v)   # weight = TIME (s)
A = csr_matrix((W, (I, J)), shape=(len(keys), len(keys)))

legs = []
for r in csv.reader(open('/tmp/legs_od.tsv'), delimiter='\t'):
    if len(r) < 5: continue
    ola, olo, dla, dlo, ds = map(float, r)
    ox, oy = (olo - lo.min()) * mlon, (ola - la.min()) * mlat
    dx, dy = (dlo - lo.min()) * mlon, (dla - la.min()) * mlat
    do, io = tree.query([ox, oy]); dd, jd = tree.query([dx, dy])
    if do > 45 or dd > 45: continue
    dn = (dla - ola) * mlat; de = (dlo - olo) * mlon
    manh = abs(dn * COS + de * SIN) + abs(-dn * SIN + de * COS)
    legs.append([int(io), int(jd), ds, manh])
legs = np.array(legs)
pred = np.full(len(legs), np.nan)
src = legs[:, 0].astype(int)
for s in np.unique(src):
    d = dijkstra(A, indices=s)
    for k in np.where(src == s)[0]:
        pred[k] = d[int(legs[k, 1])]
ok = np.isfinite(pred) & (pred < 1e8)
ds = legs[ok, 2]; manh = legs[ok, 3]; gp = pred[ok]

def stat(name, p, is_time):
    r = np.corrcoef(p, ds)[0, 1]
    sp = 1.0 if is_time else p.sum() / ds.sum()
    pr = p if is_time else p / sp
    mae = np.mean(np.abs(pr - ds)); mape = np.mean(np.abs(pr - ds) / ds) * 100
    extra = "" if is_time else f"  (fit {sp*3.6:.1f}km/h)"
    print(f'  {name:16s} corr {r:.3f}  MAE {mae:5.1f}s  MAPE {mape:4.1f}%{extra}')

print(f'lane cells {len(keys)} (one-way {sum(1 for c in cells.values() if c[3]>ONEWAY_DIR)}) | directed edges {len(W)}')
print(f'legs routable {ok.sum()}/{len(legs)} ({100*ok.sum()/len(legs):.0f}%)')
stat('directed+speed', gp, True)     # predicts TIME directly
stat('manhattan', manh, False)
