#!/usr/bin/env python3
# GPS map inference (B): build a trusted road network from our OWN truck traces (not the imported graph).
# density(densified GPS) -> smooth -> road mask -> skeleton (1px road centerlines) -> [graph].
# Input: /tmp/gps_moving.tsv  (ytno \t epoch \t lat \t lon, ordered by truck,ts).  Run with .venv-geo.
import csv, math, sys, os
import numpy as np
SCRATCH = os.environ.get('ROADSCRATCH', '/tmp')  # redirect big temp files off the RAM tmpfs
from scipy.ndimage import gaussian_filter
from skimage.morphology import skeletonize, remove_small_objects, binary_closing, disk
import matplotlib; matplotlib.use('Agg'); import matplotlib.pyplot as plt

CELL = 6.0          # raster cell metres
STEP = 8.0          # densify interpolation step metres
GAP_MAX = 90        # s; only interpolate across fixes closer than this
SIG = 1.2           # gaussian smooth
MIN_OBJ = 40        # remove road blobs smaller than this (px) — lowered to keep minor connectors

trucks = {}
for r in csv.reader(open(f'{SCRATCH}/gps_moving.tsv'), delimiter='\t'):
    if len(r) < 4: continue
    trucks.setdefault(r[0], []).append((float(r[1]), float(r[2]), float(r[3])))

la = [p[1] for v in trucks.values() for p in v]; lo = [p[2] for v in trucks.values() for p in v]
la0, la1, lo0, lo1 = min(la), max(la), min(lo), max(lo)
mlat = 111320.0; mlon = 111320.0 * math.cos(math.radians((la0 + la1) / 2))
W = int((lo1 - lo0) * mlon / CELL) + 1; H = int((la1 - la0) * mlat / CELL) + 1
dens = np.zeros((H, W), dtype=np.float32)

def px(lat, lon): return int((lon - lo0) * mlon / CELL), int((lat - la0) * mlat / CELL)

npts = 0
for pts in trucks.values():
    pts.sort()
    for (t0, a0, o0), (t1, a1, o1) in zip(pts, pts[1:]):
        if not (1 < t1 - t0 < GAP_MAX): continue
        d = math.hypot((o1 - o0) * mlon, (a1 - a0) * mlat)
        steps = max(1, int(d / STEP))
        for k in range(steps + 1):
            x, y = px(a0 + (a1 - a0) * k / steps, o0 + (o1 - o0) * k / steps)
            if 0 <= x < W and 0 <= y < H:
                dens[y, x] += 1; npts += 1

sm = gaussian_filter(dens, sigma=SIG)
thr = np.mean(sm[sm > 0]) * 0.25   # lowered 0.4→0.25: keep lightly-travelled connectors/approach lanes
mask = sm > thr
mask = binary_closing(mask, disk(1))
mask = remove_small_objects(mask, MIN_OBJ)
skel = skeletonize(mask)

fig, ax = plt.subplots(figsize=(W / 80, H / 80), dpi=80)
ax.imshow(np.log1p(dens), origin='lower', cmap='gray')
ys, xs = np.where(skel)
ax.scatter(xs, ys, s=0.25, c='#ff3b3b', marker='.', linewidths=0)
ax.set_title(f'GPS-inferred road network  ({W}x{H} @ {CELL:.0f}m,  road {int(mask.sum())}px,  centerline {int(skel.sum())}px)', fontsize=8)
ax.axis('off')
plt.savefig(f'{SCRATCH}/gps_skeleton.png', bbox_inches='tight', pad_inches=0.05)
print(f'raster {W}x{H} @ {CELL:.0f}m | densified pts {npts} | road {int(mask.sum())}px | skeleton {int(skel.sum())}px | thr {thr:.2f}')
# persist mask+skel for the graph step
np.savez_compressed(f'{SCRATCH}/road_raster.npz', dens=dens, mask=mask, skel=skel,
                    la0=la0, lo0=lo0, mlat=mlat, mlon=mlon, cell=CELL)
print('saved /tmp/gps_skeleton.png and /tmp/road_raster.npz')
