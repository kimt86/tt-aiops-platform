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

# 1) dump moving GPS (30s + 3s) → infer skeleton + build graph
psql $P -tAF$'\t' -c "
  SELECT ytno, extract(epoch FROM ts), lat, lon FROM truck_pos_hist WHERE state IN ('empty_travel','delivering')
  UNION ALL SELECT ytno, extract(epoch FROM ts), lat, lon FROM truck_pos_hifreq ORDER BY 1,2" > /tmp/gps_moving.tsv
$PY scripts/infer_road_network.py >/dev/null
$PY scripts/build_road_graph.py   >/dev/null

# 2) inputs: lane field (direction), work-points (anchors), last full hour of 3s GPS (congestion)
psql $P -tAF$'\t' -c "SELECT lat,lon,heading_deg,directionality,mean_speed FROM learn_lane_cell WHERE passes>=3 AND mean_speed>0" > /tmp/lane_cells.tsv
psql $P -tAF$'\t' -c "SELECT lat,lon,topos FROM learn_topos_point WHERE n>=30" > /tmp/workpoints.tsv
psql $P -tAF$'\t' -c "
  SELECT ytno, extract(epoch FROM ts), lat, lon FROM truck_pos_hifreq
   WHERE ts >= date_trunc('hour',now())-interval '1 hour' AND ts < date_trunc('hour',now()) ORDER BY 1,2" > /tmp/gps_lasthour.tsv

# 3) congestion map-match (→ per-edge speed) THEN geojson (directed edges colored by speed) + congestion TSV
$PY - <<'PY'
import json, math, datetime, csv
import numpy as np
from scipy.spatial import cKDTree
g = json.load(open('/tmp/road_graph.json'))
MLAT = 111320.0
def mlon(lat): return 111320.0*math.cos(math.radians(lat))
def bearing(a, b):
    dn=(b[0]-a[0])*MLAT; de=(b[1]-a[1])*mlon((a[0]+b[0])/2)
    return math.degrees(math.atan2(de, dn)) % 360.0
def adiff(x,y):
    d=abs(x-y)%360.0; return min(d,360.0-d)
def off(lat,lon,brg,m):
    b=math.radians(brg); return [lon+m*math.sin(b)/mlon(lat), lat+m*math.cos(b)/MLAT]

# ── per-edge speed: map-match last-hour 3s GPS segments to the nearest edge ──
ex=[]; ey=[]; eidx=[]
for k,e in enumerate(g['edges']):
    for la,lo in e['geom']:
        ex.append(lo*mlon(la)); ey.append(la*MLAT); eidx.append(k)
etree=cKDTree(np.column_stack([ex,ey])); eidx=np.array(eidx)
spd={}
by={}
for r in csv.reader(open('/tmp/gps_lasthour.tsv'),delimiter='\t'):
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
lc=[r for r in csv.reader(open('/tmp/lane_cells.tsv'),delimiter='\t') if len(r)>=5]
lla=np.array([float(r[0]) for r in lc]); llo=np.array([float(r[1]) for r in lc])
lhd=np.array([float(r[2]) for r in lc]); ldir=np.array([float(r[3]) for r in lc])
ltree=cKDTree(np.column_stack([llo*mlon(float(lla.mean())), lla*MLAT]))

feats=[]
for k,e in enumerate(g['edges']):
    geom=[(la,lo) for la,lo in e['geom']]
    if len(geom)<2: continue
    mid=geom[len(geom)//2]
    _,i=ltree.query([mid[1]*mlon(mid[0]), mid[0]*MLAT])
    hd=lhd[i]; oneway=bool(ldir[i]>0.6)
    eb=bearing(geom[0], geom[-1])
    if adiff(eb,hd)>90: geom=geom[::-1]; eb=(eb+180)%360
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
for r in csv.reader(open('/tmp/workpoints.tsv'),delimiter='\t'):
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
with open('/tmp/congestion_edge.tsv','w') as f:
    for k,v in spd.items():
        if len(v)<3: continue
        geom=g['edges'][k]['geom']; m=geom[len(geom)//2]
        f.write(f"{m[0]:.6f}\t{m[1]:.6f}\t{edge_speed[k]:.1f}\t{len(v)}\t{g['edges'][k]['len_m']:.1f}\n")
print(f"PUBLISHED nodes {fc['stats']['nodes']} edges {fc['stats']['edges']} {km}km wp {nwp} | speed-colored edges {len(edge_speed)}")
PY

# 4) load congestion (hour stamped by SQL so it's tz-correct)
psql $P <<'SQL'
CREATE TEMP TABLE ce_stage(mlat float8, mlon float8, med real, n int, len real);
\copy ce_stage FROM '/tmp/congestion_edge.tsv'
INSERT INTO congestion_edge(hour,mlat,mlon,med_speed_kmh,n,len_m)
  SELECT date_trunc('hour',now())-interval '1 hour', mlat,mlon,med,n,len FROM ce_stage
  ON CONFLICT (hour,mlat,mlon) DO NOTHING;
DELETE FROM congestion_edge WHERE hour < now() - interval '180 days';
SQL
