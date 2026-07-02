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
psql $P -tAF$'\t' -c "SELECT lat,lon,topos FROM learn_topos_point WHERE n>=30" > "$SCRATCH/workpoints.tsv"
$PY scripts/infer_road_network.py >/dev/null
$PY scripts/build_road_graph.py   >/dev/null

# 2) inputs: lane field (direction), last full hour of 3s GPS (congestion)
psql $P -tAF$'\t' -c "SELECT lat,lon,heading_deg,directionality,mean_speed FROM learn_lane_cell WHERE passes>=3 AND mean_speed>0" > "$SCRATCH/lane_cells.tsv"
psql $P -tAF$'\t' -c "
  SELECT ytno, extract(epoch FROM ts), lat, lon FROM truck_pos_hifreq
   WHERE ts >= date_trunc('hour',now())-interval '1 hour' AND ts < date_trunc('hour',now()) ORDER BY 1,2" > "$SCRATCH/gps_lasthour.tsv"

# 3) congestion map-match (→ per-edge speed) THEN geojson (directed edges colored by speed) + congestion TSV
$PY - <<'PY'
import json, math, datetime, csv, os
import numpy as np
from scipy.spatial import cKDTree
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
    m=geom[len(geom)//2]; head=off(m[0],m[1],eb,7)
    bl=off(head[1],head[0],eb+148,6); br=off(head[1],head[0],eb-148,6)
    feats.append({"type":"Feature","properties":{"kind":"arrow","oneway":oneway},
                  "geometry":{"type":"LineString","coordinates":[bl, head, br]}})
for la,lo in g['nodes'].values():
    feats.append({"type":"Feature","properties":{"kind":"node"},"geometry":{"type":"Point","coordinates":[lo,la]}})
nwp=0
for r in csv.reader(open(f'{SCRATCH}/workpoints.tsv'),delimiter='\t'):
    if len(r)<3: continue
    feats.append({"type":"Feature","properties":{"kind":"workpoint","topos":r[2]},
                  "geometry":{"type":"Point","coordinates":[float(r[1]),float(r[0])]}}); nwp+=1
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
print(f"PUBLISHED nodes {fc['stats']['nodes']} edges {fc['stats']['edges']} {km}km wp {nwp} | speed-colored edges {len(edge_speed)} | routable edges {len(edge_rows)}")
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
