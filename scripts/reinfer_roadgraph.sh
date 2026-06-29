#!/usr/bin/env bash
# Re-infer the road graph from ALL available GPS (30s truck_pos_hist + growing 3s truck_pos_hifreq) and
# publish it to the live map. As 3s GPS accumulates the graph gets denser/sharper — run daily to watch it.
# Writes web/{public,dist}/livemap-roadgraph.geojson (with stats in top-level keys) + appends a stats line.
set -euo pipefail
cd /home/tkadmin/projects/wp-tt-dashboard
export PGPASSWORD=wp
PY=.venv-geo/bin/python

# 1) dump moving GPS: 30s history (lots of coverage) + 3s hifreq (sharp where available)
psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -tAF$'\t' -c "
  SELECT ytno, extract(epoch FROM ts), lat, lon FROM truck_pos_hist WHERE state IN ('empty_travel','delivering')
  UNION ALL
  SELECT ytno, extract(epoch FROM ts), lat, lon FROM truck_pos_hifreq
  ORDER BY 1,2" > /tmp/gps_moving.tsv
echo "GPS rows: $(wc -l < /tmp/gps_moving.tsv)"

# 2) infer skeleton + build graph
$PY scripts/infer_road_network.py >/dev/null
$PY scripts/build_road_graph.py   >/dev/null

# 3) geojson (edges as LineStrings) + stats; anchor = learned work-points (n>=30) as a separate layer
psql -h 127.0.0.1 -p 5433 -U wp -d wp_tt -tAF$'\t' -c "
  SELECT lat, lon, topos FROM learn_topos_point WHERE n>=30" > /tmp/workpoints.tsv
$PY - <<'PY'
import json, datetime
g = json.load(open('/tmp/road_graph.json'))
feats = []
for e in g['edges']:
    coords = [[lo, la] for la, lo in e['geom']]
    if len(coords) >= 2:
        feats.append({"type":"Feature","properties":{"kind":"road","len_m":e['len_m']},
                      "geometry":{"type":"LineString","coordinates":coords}})
# graph NODES (junctions/endpoints) — this is what makes it read as a graph, not just roads
for la, lo in g['nodes'].values():
    feats.append({"type":"Feature","properties":{"kind":"node"},
                  "geometry":{"type":"Point","coordinates":[lo, la]}})
nwp = 0
for line in open('/tmp/workpoints.tsv'):
    p = line.rstrip("\n").split("\t")
    if len(p) < 3: continue
    feats.append({"type":"Feature","properties":{"kind":"workpoint","topos":p[2]},
                  "geometry":{"type":"Point","coordinates":[float(p[1]), float(p[0])]}})
    nwp += 1
km = round(sum(e['len_m'] for e in g['edges'])/1000.0, 1)
fc = {"type":"FeatureCollection",
      "stats":{"nodes":len(g['nodes']),"edges":len(g['edges']),"km":km,"workpoints":nwp,
               "generated_at":datetime.datetime.now().strftime("%Y-%m-%d %H:%M")},
      "features":feats}
for path in ("web/public/livemap-roadgraph.geojson","web/dist/livemap-roadgraph.geojson"):
    json.dump(fc, open(path,'w'))
with open("data/roadgraph_stats.tsv","a") as f:
    s = fc["stats"]; f.write(f"{s['generated_at']}\t{s['nodes']}\t{s['edges']}\t{s['km']}\t{s['workpoints']}\n")
print(f"PUBLISHED: nodes {fc['stats']['nodes']}  edges {fc['stats']['edges']}  {km}km  workpoints {nwp}")
PY
