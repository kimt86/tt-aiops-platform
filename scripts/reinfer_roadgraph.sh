#!/usr/bin/env bash
# Re-infer the road graph from ALL GPS (30s truck_pos_hist + growing 3s truck_pos_hifreq), give each edge
# a DIRECTION (from the learned lane field) + arrowheads + this-hour SPEED (3s GPS map-matched to edges),
# publish to the live map, and store ROAD-NETWORK congestion (congestion_edge). Denser as 3s grows.
# Run hourly. Writes web/{public,dist}/livemap-roadgraph.geojson + congestion_edge + stats line.
set -euo pipefail
cd /home/tkadmin/projects/wp-tt-dashboard
export PGPASSWORD=wp
PY=.venv-geo/bin/python
P="-h 127.0.0.1 -p 5433 -U wp -d wp_tt"
SCRATCH="${ROADSCRATCH:-/var/tmp/roadscratch}"; mkdir -p "$SCRATCH"; export ROADSCRATCH="$SCRATCH"  # off the full RAM tmpfs

# 1) dump moving GPS (30s + 3s) → infer skeleton + build graph (densified + work-point connectors)
psql $P -tAF$'\t' -c "
  SELECT ytno, extract(epoch FROM ts), lat, lon FROM truck_pos_hist WHERE state IN ('empty_travel','delivering')
  UNION ALL SELECT ytno, extract(epoch FROM ts), lat, lon FROM truck_pos_hifreq ORDER BY 1,2" > "$SCRATCH/gps_moving.tsv"
# work-points must be dumped BEFORE build (build attaches connectors to them)
psql $P -tAF$'\t' -c "SELECT lat,lon,topos,obs,round(spread_m)::int FROM learn_topos_point WHERE n>=30" > "$SCRATCH/workpoints.tsv"
$PY scripts/infer_road_network.py >/dev/null
$PY scripts/build_road_graph.py   >/dev/null

# 2) inputs: lane field (direction), last full hour of 3s GPS (congestion)
psql $P -tAF$'\t' -c "SELECT lat,lon,heading_deg,directionality,mean_speed FROM learn_lane_cell WHERE passes>=3 AND mean_speed>0" > "$SCRATCH/lane_cells.tsv"
psql $P -tAF$'\t' -c "
  SELECT ytno, extract(epoch FROM ts), lat, lon FROM truck_pos_hifreq
   WHERE ts >= date_trunc('hour',now())-interval '1 hour' AND ts < date_trunc('hour',now()) ORDER BY 1,2" > "$SCRATCH/gps_lasthour.tsv"

# 3) congestion map-match (→ per-edge speed) THEN geojson (directed edges colored by speed) + congestion TSV
$PY - <<'PY'
import json, math, datetime, csv, os, re, colorsys
import numpy as np
from scipy.spatial import cKDTree

# graph-color the blocks like a map: spatially-adjacent blocks (bays within ADJ_M) must differ,
# using as FEW colors as possible (Welsh-Powell greedy: order by degree desc, take smallest free color).
# few colors → each reused far apart → neighbours pop; exact block still on hover.
BLOCK_PALETTE = ['#ef4444','#3b82f6','#22c55e','#eab308','#a855f7','#f97316','#14b8a6','#ec4899','#84cc16','#06b6d4','#f43f5e','#0ea5e9']
def block_coloring(block_pts, ADJ_M=38.0):
    prefs = list(block_pts); idx = {p: i for i, p in enumerate(prefs)}
    ml = mlon(2.926)                                   # terminal-local metres (small area → fixed ref lat)
    pts = []; owner = []
    for i, p in enumerate(prefs):
        for (la, lo) in block_pts[p]:
            pts.append((lo * ml, la * MLAT)); owner.append(i)
    adj = {i: set() for i in range(len(prefs))}
    if pts:
        tree = cKDTree(np.array(pts)); owner = np.array(owner)
        for a, b in tree.query_pairs(r=ADJ_M):
            oa, ob = int(owner[a]), int(owner[b])
            if oa != ob: adj[oa].add(ob); adj[ob].add(oa)
    col = {}
    for i in sorted(range(len(prefs)), key=lambda i: -len(adj[i])):   # Welsh-Powell order
        used = {col[j] for j in adj[i] if j in col}
        c = 0
        while c in used: c += 1
        col[i] = c
    ncol = (max(col.values()) + 1) if col else 0
    def hexof(c):
        if c < len(BLOCK_PALETTE): return BLOCK_PALETTE[c]
        r, g, b = colorsys.hls_to_rgb(((c * 137) % 360) / 360.0, 0.6, 0.66)
        return '#%02x%02x%02x' % (int(r * 255), int(g * 255), int(b * 255))
    avgdeg = (sum(len(v) for v in adj.values()) / len(prefs)) if prefs else 0
    return {p: hexof(col[idx[p]]) for p in prefs}, ncol, avgdeg
SCRATCH = os.environ.get('ROADSCRATCH', '/tmp')
CONNECTOR_KMH = 10.0   # work-point approach-stub speed (bidirectional connectors)
g = json.load(open(f'{SCRATCH}/road_graph.json'))
MLAT = 111320.0
def mlon(lat): return 111320.0*math.cos(math.radians(lat))
def bearing(a, b):
    dn=(b[0]-a[0])*MLAT; de=(b[1]-a[1])*mlon((a[0]+b[0])/2)
    return math.degrees(math.atan2(de, dn)) % 360.0
def adiff(x,y):
    d=abs(x-y)%360.0; return min(d,360.0-d)
def off(lat,lon,brg,m):
    b=math.radians(brg); return [lon+m*math.sin(b)/mlon(lat), lat+m*math.cos(b)/MLAT]

# ── per-edge speed: map-match last-hour 3s GPS segments to the nearest ROAD edge ──
# (connector stubs excluded: they'd swallow GPS near work points, starving the adjacent road edge
#  of speed samples AND writing stub midpoints into the 180-day congestion_edge store.)
ex=[]; ey=[]; eidx=[]
for k,e in enumerate(g['edges']):
    if e.get('connector'): continue
    for la,lo in e['geom']:
        ex.append(lo*mlon(la)); ey.append(la*MLAT); eidx.append(k)
etree=cKDTree(np.column_stack([ex,ey])); eidx=np.array(eidx)
spd={}
by={}
for r in csv.reader(open(f'{SCRATCH}/gps_lasthour.tsv'),delimiter='\t'):
    if len(r)>=4: by.setdefault(r[0],[]).append((float(r[1]),float(r[2]),float(r[3])))
for pts in by.values():
    pts.sort()
    for (t0,a0,o0),(t1,a1,o1) in zip(pts,pts[1:]):
        dt=t1-t0
        if not (2<=dt<=15): continue
        d=math.hypot((a1-a0)*MLAT,(o1-o0)*mlon((a0+a1)/2))
        if not (5<=d<=400): continue
        mla=(a0+a1)/2; mlo=(o0+o1)/2
        dd,j=etree.query([mlo*mlon(mla), mla*MLAT])
        if dd>40: continue
        spd.setdefault(int(eidx[j]),[]).append(d/dt*3.6)
edge_speed={k:round(float(np.median(v)),1) for k,v in spd.items() if len(v)>=3}

# ── lane field for per-edge direction ──
lc=[r for r in csv.reader(open(f'{SCRATCH}/lane_cells.tsv'),delimiter='\t') if len(r)>=5]
lla=np.array([float(r[0]) for r in lc]); llo=np.array([float(r[1]) for r in lc])
lhd=np.array([float(r[2]) for r in lc]); ldir=np.array([float(r[3]) for r in lc])
lmspd=np.array([float(r[4]) for r in lc])   # learned lane mean speed (stable; edge weight for routing)
ltree=cKDTree(np.column_stack([llo*mlon(float(lla.mean())), lla*MLAT]))

# ── direction/one-way per PARENT skeleton edge (majority over its densified sub-edges) ──
# Per-sub-edge orientation from one noisy lane cell fragments the DIRECTED graph: adjacent 35m
# pieces of the same road flip against each other → "no directed path" for ~half of real OD pairs
# (measured). So: pass 1 gathers length-weighted flip/one-way/speed votes per pid; pass 2 applies
# ONE coherent decision to every sub-edge of that parent.
votes={}   # pid → [flip_len, keep_len, oneway_len, tot_len, spd_wsum, spd_len]
sub=[]     # (k, e, geom, eb) for pass 2
for k,e in enumerate(g['edges']):
    geom=[(la,lo) for la,lo in e['geom']]
    if len(geom)<2: continue
    if e.get('connector'):
        sub.append((k,e,geom,None)); continue
    mid=geom[len(geom)//2]
    _,i=ltree.query([mid[1]*mlon(mid[0]), mid[0]*MLAT])
    eb=bearing(geom[0], geom[-1])
    L=max(e['len_m'],1.0)
    v=votes.setdefault(e['pid'],[0.0]*6)
    if adiff(eb,lhd[i])>90: v[0]+=L
    else: v[1]+=L
    if ldir[i]>0.6: v[2]+=L
    v[3]+=L
    if lmspd[i]>0: v[4]+=lmspd[i]*L; v[5]+=L
    sub.append((k,e,geom,eb))

feats=[]; edge_rows=[]   # directed routable edges → road_edge (Rust Dijkstra router)
for k,e,geom,eb in sub:
    if e.get('connector'):   # work-point approach stub → one bidirectional row (oneway='f' → both arcs)
        edge_rows.append((e['u'],e['v'],round(e['len_m'],1),CONNECTOR_KMH,'f'))
        feats.append({"type":"Feature","properties":{"kind":"road","oneway":False,"len_m":e['len_m'],"connector":True},
                      "geometry":{"type":"LineString","coordinates":[[lo,la] for la,lo in geom]}})
        continue
    v=votes[e['pid']]
    flipped = v[0] > v[1]                       # parent-majority flow direction
    oneway  = (v[2] / max(v[3],1e-9)) > 0.5     # parent-majority one-way
    espd    = round(v[4]/v[5],1) if v[5] > 0 else ''
    if flipped: geom=geom[::-1]; eb=(eb+180)%360
    fr,to=(e['v'],e['u']) if flipped else (e['u'],e['v'])
    edge_rows.append((fr,to,round(e['len_m'],1),espd,'t' if oneway else 'f'))
    props={"kind":"road","oneway":oneway,"len_m":e['len_m']}
    if k in edge_speed: props["speed"]=edge_speed[k]      # this-hour median km/h → colored by congestion
    feats.append({"type":"Feature","properties":props,
                  "geometry":{"type":"LineString","coordinates":[[lo,la] for la,lo in geom]}})
# junction nodes = skeleton-graph vertices; carry id + road-degree (1=dead-end · 2=mid-road · 3+=intersection) for hover
deg={}
for e in g['edges']:
    if e.get('connector'): continue
    for nd in (e['u'],e['v']): deg[int(nd)]=deg.get(int(nd),0)+1
for nid,(la,lo) in g['nodes'].items():
    feats.append({"type":"Feature","properties":{"kind":"node","id":int(nid),"deg":deg.get(int(nid),0)},
                  "geometry":{"type":"Point","coordinates":[lo,la]}})
# work-points, tagged by node type (block/crane/wharf/other) for per-type filtering on the live map
def ntype_of(t):
    if t.startswith('WHARF'): return 'wharf'
    if re.match(r'^[CMZ][0-9]', t): return 'crane'
    if '-' in t: return 'block'
    return 'other'
nwp=0
rows=[]; block_pts={}
for r in csv.reader(open(f'{SCRATCH}/workpoints.tsv'),delimiter='\t'):
    if len(r)<3: continue
    la,lo,tp = float(r[0]),float(r[1]),r[2]
    obs = int(r[3]) if len(r)>3 and r[3] else 0   # accumulated samples (uncapped)
    sp  = int(r[4]) if len(r)>4 and r[4] else 0   # spread_m = positional precision (big = unreliable)
    nt = ntype_of(tp)
    rows.append((la,lo,tp,nt,obs,sp))
    if nt=='block': block_pts.setdefault(tp.split('-')[0],[]).append((la,lo))
# drop block bays >250m from their block's median center = stale-topos1 mislabels that plot 1-3km
# away and spawn garbage connectors (the live outlier-gate prevents new ones; this cleans existing).
BAY_OUTLIER_M = 250.0
block_ctr = {p:(float(np.median([a for a,b in v])), float(np.median([b for a,b in v]))) for p,v in block_pts.items() if len(v)>=3}
def far_from_block(pref, la, lo):
    c = block_ctr.get(pref)
    return bool(c) and math.hypot((la-c[0])*MLAT, (lo-c[1])*mlon(c[0])) > BAY_OUTLIER_M
block_pts = {p:[(la,lo) for la,lo in v if not far_from_block(p,la,lo)] for p,v in block_pts.items()}
block_pts = {p:v for p,v in block_pts.items() if v}
bcolor, nbcol, bavgdeg = block_coloring(block_pts)   # map-coloring: adjacent blocks differ, few colors
ndrop=0
for la,lo,tp,nt,obs,sp in rows:
    if nt=='block' and far_from_block(tp.split('-')[0], la, lo): ndrop+=1; continue
    props={"kind":"workpoint","topos":tp,"ntype":nt,"obs":obs,"spread":sp}
    if nt=='block': props["bcolor"]=bcolor.get(tp.split('-')[0], '#5eead4')
    feats.append({"type":"Feature","properties":props,
                  "geometry":{"type":"Point","coordinates":[lo,la]}}); nwp+=1
km=round(sum(e['len_m'] for e in g['edges'])/1000.0,1)
fc={"type":"FeatureCollection",
    "stats":{"nodes":len(g['nodes']),"edges":len(g['edges']),"km":km,"workpoints":nwp,
             "congested_edges":len(edge_speed),
             "generated_at":datetime.datetime.now().strftime("%Y-%m-%d %H:%M")},
    "features":feats}
for p in ("web/public/livemap-roadgraph.geojson","web/dist/livemap-roadgraph.geojson"):
    json.dump(fc,open(p,'w'))
with open("data/roadgraph_stats.tsv","a") as f:
    s=fc["stats"]; f.write(f"{s['generated_at']}\t{s['nodes']}\t{s['edges']}\t{s['km']}\t{s['workpoints']}\t{s['congested_edges']}\n")
with open(f'{SCRATCH}/congestion_edge.tsv','w') as f:
    for k,v in spd.items():
        if len(v)<3: continue
        geom=g['edges'][k]['geom']; m=geom[len(geom)//2]
        f.write(f"{m[0]:.6f}\t{m[1]:.6f}\t{edge_speed[k]:.1f}\t{len(v)}\t{g['edges'][k]['len_m']:.1f}\n")
# routable directed graph for the Rust dispatch router (mig 0077)
with open(f'{SCRATCH}/road_node.tsv','w') as f:
    for nid,(la,lo) in g['nodes'].items():
        f.write(f"{int(nid)}\t{la:.7f}\t{lo:.7f}\n")
with open(f'{SCRATCH}/road_edge.tsv','w') as f:
    for fr,to,lm,sp,ow in edge_rows:
        f.write(f"{fr}\t{to}\t{lm}\t{sp}\t{ow}\n")
print(f"PUBLISHED nodes {fc['stats']['nodes']} edges {fc['stats']['edges']} {km}km wp {nwp} (bay-outliers dropped {ndrop}) | blocks {len(block_pts)} → {nbcol} colors (adj~{bavgdeg:.1f}) | speed-colored edges {len(edge_speed)} | routable edges {len(edge_rows)}")
PY

# 4) load congestion (hour stamped by SQL so it's tz-correct)
psql $P <<SQL
CREATE TEMP TABLE ce_stage(mlat float8, mlon float8, med real, n int, len real);
\copy ce_stage FROM '$SCRATCH/congestion_edge.tsv'
INSERT INTO congestion_edge(hour,mlat,mlon,med_speed_kmh,n,len_m)
  SELECT date_trunc('hour',now())-interval '1 hour', mlat,mlon,med,n,len FROM ce_stage
  ON CONFLICT (hour,mlat,mlon) DO NOTHING;
DELETE FROM congestion_edge WHERE hour < now() - interval '180 days';
SQL

# 5) publish the routable directed road graph (mig 0077) for the Rust dispatch router — atomic swap
psql $P -v ON_ERROR_STOP=1 <<SQL
BEGIN;
TRUNCATE road_node;
\copy road_node FROM '$SCRATCH/road_node.tsv'
TRUNCATE road_edge;
\copy road_edge(from_id,to_id,len_m,speed_kmh,oneway) FROM '$SCRATCH/road_edge.tsv' WITH (NULL '')
COMMIT;
SQL
