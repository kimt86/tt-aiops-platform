#!/usr/bin/env python3
# GPS map inference (B): build a trusted road network from our OWN truck traces (not the imported graph).
# density(densified GPS) -> smooth -> road mask -> skeleton (1px road centerlines) -> [graph].
# Input: /tmp/gps_moving.tsv  (ytno \t epoch \t lat \t lon, ordered by truck,ts).  Run with .venv-geo.
import csv, math, sys, os
import numpy as np
SCRATCH = os.environ.get('ROADSCRATCH', '/var/tmp/roadscratch')  # redirect big temp files off the RAM tmpfs
from scipy.ndimage import gaussian_filter
from skimage.morphology import skeletonize, remove_small_objects, binary_closing, disk
import matplotlib; matplotlib.use('Agg'); import matplotlib.pyplot as plt

CELL = 6.0          # raster cell metres
STEP = 8.0          # densify interpolation step metres
GAP_MAX = 90        # s; only interpolate across fixes closer than this
SIG = 1.2           # gaussian smooth
MIN_OBJ = 40        # remove road blobs smaller than this (px) — lowered to keep minor connectors
MAX_SEG_M = 5000.0  # a single fix-to-fix hop longer than this is corrupt GPS, not a drive

trucks = {}
for r in csv.reader(open(f'{SCRATCH}/gps_moving.tsv'), delimiter='\t'):
    if len(r) < 4: continue
    trucks.setdefault(r[0], []).append((float(r[1]), float(r[2]), float(r[3])))

# ── raster bounds: ROBUST, never raw min/max ─────────────────────────────────────────────────
# The terminal is ~5x3km, but a handful of corrupt GPS fixes land 100~250km away (2026-07-28:
# 20 rows out of 7.3M). Sizing the raster off raw min/max let those few rows inflate it 3,374x
# (0.43M → 1,461M cells); every downstream copy — smooth, label, imshow, savez — then multiplied
# that, peaked ~121GB and OOM-killed the machine. So: centre on the MEDIAN fix, drop anything past
# a physical radius, and hard-cap the raster so a bad day aborts loudly instead of eating the box.
MAX_R_M   = 20_000.0      # farther than this from the median centre = corrupt fix, not traffic
# Backstop. ⚠ This was 60M and could therefore NEVER fire: MAX_R_M=20km bounds the bbox to
# 40km per side, so at CELL=6m the raster tops out at (40000/6)^2 = 44.4M cells — always under
# 60M. A cap above the maximum its own radius allows is decoration, not a backstop. 20M sits
# above real operation (measured 8.05M on the 2026-07-28 input) and below that 44.4M worst case,
# so it can actually stop something.
MAX_CELLS = 20_000_000    # ~80MB float32; measured normal 8.05M, radius-implied worst case 44.4M
_la = np.fromiter((p[1] for v in trucks.values() for p in v), dtype=np.float64)
_lo = np.fromiter((p[2] for v in trucks.values() for p in v), dtype=np.float64)
cla = float(np.median(_la)); clo = float(np.median(_lo))
mlat = 111320.0; mlon = 111320.0 * math.cos(math.radians(cla))
keep = (np.abs(_la - cla) * mlat <= MAX_R_M) & (np.abs(_lo - clo) * mlon <= MAX_R_M)
nfar = int((~keep).sum())
if not keep.any(): sys.exit('ABORT: no GPS fix within bounds of the median centre')
la0, la1 = float(_la[keep].min()), float(_la[keep].max())
lo0, lo1 = float(_lo[keep].min()), float(_lo[keep].max())
del _la, _lo, keep
W = int((lo1 - lo0) * mlon / CELL) + 1; H = int((la1 - la0) * mlat / CELL) + 1
if W * H > MAX_CELLS:
    sys.exit(f'ABORT: raster {W}x{H} = {W*H/1e6:.0f}M cells > cap {MAX_CELLS/1e6:.0f}M '
             f'(bbox {la0:.4f}..{la1:.4f} / {lo0:.4f}..{lo1:.4f}) — refusing to allocate')
dens = np.zeros((H, W), dtype=np.float32)

def px(lat, lon): return int((lon - lo0) * mlon / CELL), int((lat - la0) * mlat / CELL)

npts = 0; nseg = 0
for pts in trucks.values():
    pts.sort()
    for (t0, a0, o0), (t1, a1, o1) in zip(pts, pts[1:]):
        if not (1 < t1 - t0 < GAP_MAX): continue
        d = math.hypot((o1 - o0) * mlon, (a1 - a0) * mlat)
        if d > MAX_SEG_M: nseg += 1; continue   # a 100km "hop" in <90s is a corrupt fix pair, not a drive
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

_sc = min(1.0, 3000.0 / max(W, H))   # clamp the debug canvas: figsize=W/80 at dpi=80 is a W x H px buffer
fig, ax = plt.subplots(figsize=(W * _sc / 80, H * _sc / 80), dpi=80)
ax.imshow(np.log1p(dens), origin='lower', cmap='gray')
ys, xs = np.where(skel)
ax.scatter(xs, ys, s=0.25, c='#ff3b3b', marker='.', linewidths=0)
ax.set_title(f'GPS-inferred road network  ({W}x{H} @ {CELL:.0f}m,  road {int(mask.sum())}px,  centerline {int(skel.sum())}px)', fontsize=8)
ax.axis('off')
plt.savefig(f'{SCRATCH}/gps_skeleton.png', bbox_inches='tight', pad_inches=0.05)
print(f'raster {W}x{H} @ {CELL:.0f}m ({W*H/1e6:.2f}M cells) | densified pts {npts} | road {int(mask.sum())}px '
      f'| skeleton {int(skel.sum())}px | thr {thr:.2f} | dropped: {nfar} far fixes, {nseg} long segs')
# persist mask+skel for the graph step
np.savez_compressed(f'{SCRATCH}/road_raster.npz', dens=dens, mask=mask, skel=skel,
                    la0=la0, lo0=lo0, mlat=mlat, mlon=mlon, cell=CELL)
print('saved /tmp/gps_skeleton.png and /tmp/road_raster.npz')
