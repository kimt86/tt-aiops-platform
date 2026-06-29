// LIVE MAP — ESRI satellite (MapLibre GL) + TOS layout layers (areas/nodes/links,
// individually toggleable like wp-tt-data-center) + live equipment markers.
// Data: REPLAY of captured WP-TT GPS (web/public/livemap-replay.json) on a real-time
// loop; TOS layout from web/public/livemap-{layout,nodes,links}.json (extracted from
// wp-tt-data-center reference, mm/m → lat/lon via the fitted projection).
import maplibregl from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
import { useEffect, useMemo, useRef, useState } from "react";
import { type Lang } from "./i18n";
import { api, type WorkPoint, type WharfPoint } from "./api";
import { LiveVehicleDetail, type SelVeh } from "./LiveVehicleDetail";

const ESRI = "https://services.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}";

// Map rotation. The Westports quay (QC line) runs ~NNE–SSW (azimuth ~30°); a bearing of
// 300° lays that berth line horizontal with the sea toward the top and the terminal below.
// The user can free-rotate (compass / right-drag); we persist their last bearing.
const QUAY_BEARING = 300;
const BEARING_KEY = "wp-map-bearing";
function initialBearing(): number {
  try { const v = Number(localStorage.getItem(BEARING_KEY)); if (Number.isFinite(v)) return v; } catch { /* ignore */ }
  return QUAY_BEARING;
}

type Pt = [number, number, number, number, number]; // [t, lat, lon, speed, engine]
type Device = { id: string; cls: string; pts: Pt[] };
type Replay = { meta: { window_s: number; center: [number, number]; n_devices: number }; devices: Device[] };

// live feed from /api/livemap/positions (GPS via the SSH tunnel)
type Dispatch = "idle" | "staging" | "empty_travel" | "delivering" | "soon_idle" | "approaching" | "wait_rtg";
type LiveDev = { id: string; cls: string; lat: number; lon: number; speed: number; engine: number; age_s: number; dispatch?: Dispatch; jobtype?: string; topos1?: string };
type LiveSnap = { source: string; connected: boolean; count: number; as_of: string | null; dispatch_counts?: Record<string, number>; devices: LiveDev[] };

// smooth (delayed) playback: show the map N minutes in the past so we can interpolate each
// device between the GPS fixes that have arrived since — turning the sparse "teleporting" feed
// into smooth motion. Presets in minutes (0 = realtime, no interpolation).
// delay presets in MINUTES (fractions = seconds, e.g. 5/60 = 5s). 0 = realtime (no interpolation).
const DELAY_OPTS = [0, 5 / 60, 10 / 60, 30 / 60, 1, 3, 5] as const;
const MAX_DELAY_MIN = 5;
const delayLbl = (m: number, ko: boolean) => m === 0 ? (ko ? "실시간" : "live") : m < 1 ? `${Math.round(m * 60)}s` : `${m}m`;
// yard equipment tops out ~25-30 km/h; a fix implying faster than this (over its real fix-time
// gap) is a GPS spike/teleport, not motion — reject it so smooth playback doesn't "fly".
const MAX_PLAUSIBLE_KMH = 50;

function metersBetween(la1: number, lo1: number, la2: number, lo2: number): number {
  const R = 6371000, p = Math.PI / 180;
  const dphi = (la2 - la1) * p, dl = (lo2 - lo1) * p;
  const a = Math.sin(dphi / 2) ** 2 + Math.cos(la1 * p) * Math.cos(la2 * p) * Math.sin(dl / 2) ** 2;
  return 2 * R * Math.asin(Math.sqrt(a));
}

// dispatch-state highlight on the map (TT pool building)
// dispatch pools — filter the map by vehicle-pool type (TT only)
const DISPATCH_POOLS: { key: Dispatch; ko: string; en: string; color: string }[] = [
  { key: "idle", ko: "유휴", en: "Idle", color: "#22c55e" },
  { key: "staging", ko: "배차·대기", en: "Staging", color: "#0ea5e9" },
  { key: "soon_idle", ko: "곧유휴·임박", en: "Imminent", color: "#f59e0b" },
  { key: "approaching", ko: "접근·적재됨", en: "Approaching", color: "#fcd34d" },
  { key: "delivering", ko: "적재이동", en: "Deliver", color: "#38bdf8" },
  { key: "wait_rtg", ko: "RTG대기", en: "Wait RTG", color: "#ef4444" },
  { key: "empty_travel", ko: "공차 주행 중", en: "Empty traveling", color: "#94a3b8" },
];

type EquipKey = "TT" | "RTG" | "QC" | "ETC";
function equip(cls: string): EquipKey {
  if (cls === "TT") return "TT";
  if (cls === "RTG") return "RTG";
  if (cls === "C") return "QC";
  return "ETC";
}
const EQUIP_TABS: { key: EquipKey; ko: string; en: string }[] = [
  { key: "TT", ko: "야드트럭", en: "TT" },
  { key: "RTG", ko: "야드크레인", en: "RTG" },
  { key: "QC", ko: "안벽크레인", en: "QC" },
  { key: "ETC", ko: "기타", en: "Other" },
];
const ALL_EQUIP: EquipKey[] = ["TT", "RTG", "QC", "ETC"];

function stateOf(spd: number, eng: number): "moving" | "idle" | "off" {
  if (spd > 0) return "moving";
  if (eng === 1) return "idle";
  return "off";
}
const STATE_COLOR: Record<string, string> = { moving: "#22c55e", idle: "#f59e0b", off: "#64748b" };
const STATES: { key: "moving" | "idle" | "off"; ko: string; en: string }[] = [
  { key: "moving", ko: "이동 중", en: "Moving" }, { key: "idle", ko: "대기", en: "Idle" }, { key: "off", ko: "정지", en: "Stopped" },
];

// equipment → a little ICON drawn from primitives (24×24 design space). Body parts fill
// with the STATE color; `dark` parts (wheels, spreaders, cables) are near-black for
// contrast. The same spec renders both the map markers (canvas raster) and the tab legend
// (SVG), so they always match. Shapes evoke the real equipment:
//   TT = terminal tractor (cab + chassis + wheels), RTG = rubber-tyred gantry (portal +
//   spreader + tyres), QC = ship-to-shore crane (apex + long boom + hanging spreader),
//   ETC = generic hex with hub.
type Prim =
  | { k: "rect"; x: number; y: number; w: number; h: number; r?: number; dark?: boolean }
  | { k: "poly"; pts: [number, number][]; dark?: boolean }
  | { k: "circle"; cx: number; cy: number; r: number; dark?: boolean };

const EQUIP_ICON: Record<string, Prim[]> = {
  TT: [
    { k: "rect", x: 1.5, y: 6, w: 12.5, h: 8, r: 1.6 }, // chassis / container box
    { k: "poly", pts: [[14, 9], [18, 9], [21, 12], [21, 14], [14, 14]] }, // cab
    { k: "circle", cx: 5.5, cy: 14.6, r: 2.1, dark: true },
    { k: "circle", cx: 16.6, cy: 14.6, r: 2.1, dark: true },
  ],
  RTG: [
    { k: "rect", x: 2, y: 3.6, w: 20, h: 2.8, r: 0.9 }, // top beam
    { k: "rect", x: 3.4, y: 5, w: 2.4, h: 12, r: 0.4 }, // left leg
    { k: "rect", x: 18.2, y: 5, w: 2.4, h: 12, r: 0.4 }, // right leg
    { k: "rect", x: 10.3, y: 6.6, w: 3.4, h: 2.6, r: 0.4, dark: true }, // trolley / spreader
    { k: "circle", cx: 4.6, cy: 17.6, r: 1.8, dark: true },
    { k: "circle", cx: 19.4, cy: 17.6, r: 1.8, dark: true },
  ],
  QC: [
    { k: "poly", pts: [[8.4, 4.2], [12, 1], [15.6, 4.2]] }, // apex tower
    { k: "rect", x: 1, y: 4, w: 22, h: 2.4, r: 0.6 }, // long boom over the water
    { k: "rect", x: 6.8, y: 6, w: 2.2, h: 11, r: 0.4 }, // leg 1
    { k: "rect", x: 12.6, y: 6, w: 2.2, h: 11, r: 0.4 }, // leg 2
    { k: "circle", cx: 7.9, cy: 17.6, r: 1.5, dark: true },
    { k: "circle", cx: 13.7, cy: 17.6, r: 1.5, dark: true },
    { k: "rect", x: 18.4, y: 6.4, w: 1.2, h: 5.6, dark: true }, // hoist cable
    { k: "rect", x: 17, y: 11.6, w: 4, h: 2, r: 0.3, dark: true }, // spreader over ship
  ],
  ETC: [
    { k: "poly", pts: [[12, 2.4], [19.6, 7], [19.6, 15.4], [12, 20], [4.4, 15.4], [4.4, 7]] }, // hex
    { k: "circle", cx: 12, cy: 11.2, r: 2.5, dark: true }, // hub
  ],
};

function rrPath(path: Path2D, x: number, y: number, w: number, h: number, r: number) {
  r = Math.min(r, w / 2, h / 2);
  path.moveTo(x + r, y); path.arcTo(x + w, y, x + w, y + h, r); path.arcTo(x + w, y + h, x, y + h, r);
  path.arcTo(x, y + h, x, y, r); path.arcTo(x, y, x + w, y, r); path.closePath();
}
function primPath(p: Prim): Path2D {
  const path = new Path2D();
  if (p.k === "rect") rrPath(path, p.x, p.y, p.w, p.h, p.r ?? 0);
  else if (p.k === "circle") path.arc(p.cx, p.cy, p.r, 0, Math.PI * 2);
  else p.pts.forEach(([x, y], i) => (i ? path.lineTo(x, y) : path.moveTo(x, y)));
  if (p.k === "poly") path.closePath();
  return path;
}
function drawEquipIcon(ctx: CanvasRenderingContext2D, prims: Prim[], s: number, color: string) {
  ctx.save();
  ctx.scale(s / 24, s / 24);
  ctx.lineJoin = "round";
  ctx.lineWidth = 1.3;
  ctx.strokeStyle = "rgba(0,0,0,0.85)";
  for (const p of prims) {
    const path = primPath(p);
    ctx.fillStyle = p.dark ? "#0b1220" : color;
    ctx.fill(path);
    ctx.stroke(path);
  }
  ctx.restore();
}
// register one raster per (equipment icon × state color) so icon-image can pick by feature.
function addEquipIcons(map: maplibregl.Map) {
  const S = 46; // canvas px; pixelRatio 2 → 23px logical source
  for (const [eq, prims] of Object.entries(EQUIP_ICON)) {
    for (const st of ["moving", "idle", "off"]) {
      const name = `${eq}-${st}`;
      if (map.hasImage(name)) continue;
      const cv = document.createElement("canvas"); cv.width = S; cv.height = S;
      const ctx = cv.getContext("2d"); if (!ctx) continue;
      drawEquipIcon(ctx, prims, S, STATE_COLOR[st]);
      map.addImage(name, ctx.getImageData(0, 0, S, S), { pixelRatio: 2 });
    }
  }
}

// ── TOS layer toggles (mirrors wp-tt-data-center LiveLayerPanel) ──
type LayerKey =
  | "areas" | "pointsQuay" | "pointsBlock" | "pointsGateIn" | "pointsGateOut" | "pointsOther"
  | "linksStraight" | "linksTurn" | "linksLaneSwitch"
  | "learnTopos" | "learnLanes" | "demand";
type Toggles = Record<LayerKey, boolean>;
const DEFAULT_TOGGLES: Toggles = {
  areas: true, pointsQuay: false, pointsBlock: false, pointsGateIn: false, pointsGateOut: false,
  pointsOther: false, linksStraight: false, linksTurn: false, linksLaneSwitch: false,
  learnTopos: false, learnLanes: false, demand: false,
};
const LAYER_TOTAL = 12; // toggle count shown in the panel header
const EMPTY_FC: GeoJSON.FeatureCollection = { type: "FeatureCollection", features: [] };

// Learned-wharf zones as rectangles ALIGNED to the local quay direction: for each point, estimate
// the quay bearing from its 2 nearest wharf neighbours (axial mean), then box it — long axis along
// the quay (length ≈ neighbour spacing), short axis = its learned depth (spread).
function wharfZonesFC(pts: WharfPoint[]): GeoJSON.FeatureCollection {
  const M = 111320;
  const distM = (a: WharfPoint, b: WharfPoint) => {
    const dn = (a.lat - b.lat) * M;
    const de = (a.lon - b.lon) * M * Math.cos(((a.lat + b.lat) / 2) * Math.PI / 180);
    return Math.hypot(dn, de);
  };
  const bearingTo = (p: WharfPoint, q: WharfPoint) => {
    const de = (q.lon - p.lon) * M * Math.cos(((p.lat + q.lat) / 2) * Math.PI / 180);
    return Math.atan2(de, (q.lat - p.lat) * M); // from north, clockwise
  };
  // global quay direction = axial mean of every point's nearest-neighbour bearing (robust fallback)
  let gr = 0, gi = 0;
  for (const p of pts) {
    const nn = pts.filter((q) => q !== p).map((q) => ({ q, d: distM(p, q) })).sort((a, b) => a.d - b.d)[0];
    if (nn) { const th = bearingTo(p, nn.q); gr += Math.cos(2 * th); gi += Math.sin(2 * th); }
  }
  const global = pts.length > 1 ? Math.atan2(gi, gr) / 2 : 0;
  const feats: GeoJSON.Feature[] = pts.map((p) => {
    const near = pts.filter((q) => q !== p).map((q) => ({ q, d: distM(p, q) })).sort((a, b) => a.d - b.d).slice(0, 3);
    // axial mean of bearings to the nearest neighbours (a line has no head/tail → double-angle mean)
    let zr = 0, zi = 0;
    for (const { q } of near) {
      const th = bearingTo(p, q);
      zr += Math.cos(2 * th);
      zi += Math.sin(2 * th);
    }
    let theta = near.length ? Math.atan2(zi, zr) / 2 : global;
    // reject a local estimate that deviates > 25° (line-angle) from the quay → use the global direction
    const diff = Math.abs((((theta - global) + Math.PI / 2) % Math.PI + Math.PI) % Math.PI - Math.PI / 2);
    if (diff > (25 * Math.PI) / 180) theta = global;
    const L = Math.max(30, Math.min(90, near.length ? near[0].d : 50)); // along quay
    const W = Math.max(18, Math.min(45, (p.spread_m ?? 30) * 0.9)); // depth
    const u = [Math.sin(theta), Math.cos(theta)]; // along (east, north)
    const v = [Math.cos(theta), -Math.sin(theta)]; // perpendicular
    const ring = [[1, 1], [1, -1], [-1, -1], [-1, 1], [1, 1]].map(([su, sv]) => {
      const e = su * (L / 2) * u[0] + sv * (W / 2) * v[0];
      const n = su * (L / 2) * u[1] + sv * (W / 2) * v[1];
      return [p.lon + e / (M * Math.cos((p.lat * Math.PI) / 180)), p.lat + n / M];
    });
    return { type: "Feature", properties: { topos: p.topos }, geometry: { type: "Polygon", coordinates: [ring] } };
  });
  return { type: "FeatureCollection", features: feats };
}

// Dispatched work points: one marker per currently-dispatched work point (last hour). Clicking it
// shows TOS's dispatched truck beside ours (from the timing-skew-free comparison). Replaces the old
// truck→work advisory lines.
function workPointsFC(pts: WorkPoint[]): GeoJSON.FeatureCollection {
  return {
    type: "FeatureCollection",
    features: pts.filter((p) => p.lat && p.lon).map((p, i) => ({
      type: "Feature",
      geometry: { type: "Point", coordinates: [p.lon, p.lat] },
      properties: {
        i, qc: p.qc, queuename: p.queuename, jobtype: p.jobtype ?? "",
        tos: p.tos_ytno ?? "", our: p.our_ytno ?? "",
        tos_arr: p.tos_arrival_s ?? -1, our_arr: p.our_arrival_s ?? -1,
        agree: p.agree ? 1 : 0, delta: p.delta_s ?? 0, n: p.n, agree_n: p.agree_n,
        ntrucks: p.tos_trucks?.length ?? 0,
      },
    })),
  };
}

// classify every truck involved at a work point: dispatched by TOS, picked by us, or both.
function workTrucks(p: WorkPoint): { yt: string; kind: "tos" | "our" | "both" }[] {
  const tos = new Set(p.tos_trucks ?? []);
  const our = new Set(p.our_trucks ?? []);
  return [...new Set([...tos, ...our])].map((yt) => ({ yt, kind: tos.has(yt) && our.has(yt) ? "both" : tos.has(yt) ? "tos" : "our" }));
}

// lines from a work point to each involved truck (TOS=cyan, ours=purple, both=green).
// `pos` = each truck's CURRENTLY DISPLAYED position (smoothed), so lines stay glued to the markers.
function workLinesFC(p: WorkPoint, pos: Map<string, [number, number]>): GeoJSON.FeatureCollection {
  const feats: GeoJSON.Feature[] = [];
  for (const { yt, kind } of workTrucks(p)) {
    const c = pos.get(yt);
    if (c) feats.push({ type: "Feature", properties: { kind, ytno: yt }, geometry: { type: "LineString", coordinates: [[p.lon, p.lat], c] } });
  }
  return { type: "FeatureCollection", features: feats };
}

// a ring marker on each involved truck (same color coding), at its displayed (smoothed) position.
function workTruckPtsFC(p: WorkPoint, pos: Map<string, [number, number]>): GeoJSON.FeatureCollection {
  const feats: GeoJSON.Feature[] = [];
  for (const { yt, kind } of workTrucks(p)) {
    const c = pos.get(yt);
    if (c) feats.push({ type: "Feature", properties: { kind, ytno: yt }, geometry: { type: "Point", coordinates: c } });
  }
  return { type: "FeatureCollection", features: feats };
}

// a teardrop map-pin icon (filled + white center), drawn to a canvas for map.addImage.
function pinImage(fill: string): ImageData {
  const S = 40;
  const c = document.createElement("canvas");
  c.width = S; c.height = S;
  const x = c.getContext("2d")!;
  const cx = S / 2, cy = S * 0.38, r = S * 0.3;
  x.beginPath();
  x.moveTo(cx, S - 3);
  x.quadraticCurveTo(cx - r * 1.3, cy + r, cx - r, cy);
  x.arc(cx, cy, r, Math.PI, 0, false);
  x.quadraticCurveTo(cx + r * 1.3, cy + r, cx, S - 3);
  x.closePath();
  x.fillStyle = fill; x.fill();
  x.lineWidth = 2; x.strokeStyle = "#0a0f1d"; x.stroke();
  x.beginPath(); x.arc(cx, cy, r * 0.42, 0, Math.PI * 2);
  x.fillStyle = "#ffffff"; x.fill();
  return x.getImageData(0, 0, S, S);
}

// Metric grid: terminal split into uniform `m`-metre cells; each occupied cell becomes a filled
// polygon colored by a chosen live metric (avg speed / vehicle count). Cell = (round(lat/deg),
// round(lon/deg)) — same cell def as the density collector. Computed client-side from the live feed.
const GRID_MIN = 50, GRID_MAX = 200; // adjustable cell size (m)
// fill-color ramps (module consts so addLayer + the metric switch stay in sync)
const SPEED_COLOR: maplibregl.ExpressionSpecification = // slow=red → fast=green (congestion view)
  ["interpolate", ["linear"], ["get", "speed"], 0, "#ef4444", 8, "#f59e0b", 18, "#22c55e"];
const COUNT_COLOR: maplibregl.ExpressionSpecification = // few=blue → many=red (density view)
  ["interpolate", ["linear"], ["get", "count"], 1, "#1e3a8a", 4, "#22d3ee", 7, "#f59e0b", 12, "#ef4444"];
function buildMetricGrid(devs: { cls: string; lat: number; lon: number; speed: number }[], m: number): GeoJSON.FeatureCollection {
  const deg = m / 111320;
  const agg = new Map<string, { cx: number; cy: number; count: number; sumSpeed: number }>();
  for (const d of devs) {
    if (d.cls !== "TT" || !d.lat || !d.lon) continue; // road traffic = trucks
    const cx = Math.round(d.lat / deg), cy = Math.round(d.lon / deg);
    const key = `${cx},${cy}`;
    const a = agg.get(key) ?? { cx, cy, count: 0, sumSpeed: 0 };
    a.count++; a.sumSpeed += d.speed ?? 0; agg.set(key, a);
  }
  const feats: GeoJSON.Feature[] = [];
  for (const a of agg.values()) {
    const latLo = (a.cx - 0.5) * deg, latHi = (a.cx + 0.5) * deg;
    const lonLo = (a.cy - 0.5) * deg, lonHi = (a.cy + 0.5) * deg;
    feats.push({
      type: "Feature",
      properties: { count: a.count, speed: Math.round((a.sumSpeed / a.count) * 10) / 10 },
      geometry: { type: "Polygon", coordinates: [[[lonLo, latLo], [lonHi, latLo], [lonHi, latHi], [lonLo, latHi], [lonLo, latLo]]] },
    });
  }
  return { type: "FeatureCollection", features: feats };
}

// heading-oriented ARROWS for the learned driving-lane field — pure geometry (shaft + 2 barbs at
// the head), no text glyphs. The arrowhead shows which way traffic actually flows through the cell.
function laneSegments(
  grid: { lat: number; lon: number; passes: number; heading_deg: number | null; directionality: number | null; mean_speed: number | null }[],
): GeoJSON.Feature[] {
  const L = 11; // half-shaft (m) — ~22m arrow per 22m cell
  const B = 6;  // arrowhead barb length (m)
  // step `m` metres from (lat,lon) along compass bearing `degB` (0=N, 90=E) → [lon, lat]
  const step = (lat: number, lon: number, degB: number, m: number): [number, number] => {
    const b = (degB * Math.PI) / 180;
    const dLat = (m / 111320) * Math.cos(b);
    const dLon = (m / (111320 * Math.cos((lat * Math.PI) / 180))) * Math.sin(b);
    return [lon + dLon, lat + dLat];
  };
  return grid.map((c) => {
    const h = c.heading_deg ?? 0;
    const tail = step(c.lat, c.lon, h, -L);
    const head = step(c.lat, c.lon, h, L);
    const barbA = step(head[1], head[0], h + 145, B); // barbs splay backward from the head
    const barbB = step(head[1], head[0], h - 145, B);
    return {
      type: "Feature",
      geometry: { type: "MultiLineString", coordinates: [[tail, head], [barbA, head, barbB]] },
      properties: {
        dir: c.directionality ?? 0, passes: c.passes,
        heading: Math.round(c.heading_deg ?? 0),
        speed: c.mean_speed != null ? Math.round(c.mean_speed * 10) / 10 : null,
      },
    } as GeoJSON.Feature;
  });
}
// toggle → maplibre layer ids + swatch color
const NODE_LAYERS: Record<string, { key: LayerKey; cat: string; color: string; ko: string; en: string }> = {
  q: { key: "pointsQuay", cat: "quay", color: "#ffae6e", ko: "안벽 작업", en: "Quay" },
  b: { key: "pointsBlock", cat: "block", color: "#5eead4", ko: "블록 작업", en: "Block" },
  gi: { key: "pointsGateIn", cat: "gatein", color: "#4ade80", ko: "게이트 IN", en: "Gate IN" },
  go: { key: "pointsGateOut", cat: "gateout", color: "#ef4444", ko: "게이트 OUT", en: "Gate OUT" },
  o: { key: "pointsOther", cat: "other", color: "#facc15", ko: "그 외", en: "Other" },
};
const LINK_LAYERS: Record<string, { key: LayerKey; t: number; color: string; ko: string; en: string }> = {
  s: { key: "linksStraight", t: 0, color: "#e2e8f0", ko: "직진", en: "Straight" },
  tn: { key: "linksTurn", t: 1, color: "#fb923c", ko: "회전", en: "Turn" },
  ls: { key: "linksLaneSwitch", t: 2, color: "#34d399", ko: "차선변경", en: "Lane switch" },
};
const AREA_LAYERS = ["lay-road-fill", "lay-road-line", "lay-block-fill", "lay-block-line", "lay-block-label"];

function posAt(d: Device, t: number) {
  const pts = d.pts;
  if (t < pts[0][0] || t > pts[pts.length - 1][0]) return null;
  let i = 0;
  while (i < pts.length - 1 && pts[i + 1][0] <= t) i++;
  const a = pts[i], b = pts[Math.min(i + 1, pts.length - 1)];
  const span = b[0] - a[0], f = span > 0 ? (t - a[0]) / span : 0;
  return { lat: a[1] + (b[1] - a[1]) * f, lon: a[2] + (b[2] - a[2]) * f, state: stateOf(a[3], a[4]), speed: a[3] };
}

// ── game-like weather effect overlay (rain / cloud / sun), driven by /api/weather ──
type WxData = { precip_mm_hr: number | null; visibility_km: number | null; wind_ms: number | null; weather_code: number | null; age_s: number };
type FxMode = "clear" | "cloud" | "rain";
// Tomorrow.io weather codes → real condition label + icon + on-map effect + its base strength (i).
// i = how strong that effect plays (구름 조금 옅게 ↔ 흐림 짙게, 약한 비 ↔ 강한 비), so the look
// changes per code, not just per the 3 groups.
const WX_CODES: Record<number, { ko: string; en: string; icon: string; mode: FxMode; i: number }> = {
  1000: { ko: "맑음", en: "Clear", icon: "☀️", mode: "clear", i: 1.0 },
  1100: { ko: "대체로 맑음", en: "Mostly clear", icon: "🌤️", mode: "clear", i: 0.7 },
  1101: { ko: "구름 조금", en: "Partly cloudy", icon: "⛅", mode: "cloud", i: 0.3 },
  1102: { ko: "구름 많음", en: "Mostly cloudy", icon: "🌥️", mode: "cloud", i: 0.6 },
  1001: { ko: "흐림", en: "Cloudy", icon: "☁️", mode: "cloud", i: 0.85 },
  2000: { ko: "안개", en: "Fog", icon: "🌫️", mode: "cloud", i: 1.0 },
  2100: { ko: "옅은 안개", en: "Light fog", icon: "🌫️", mode: "cloud", i: 0.55 },
  4000: { ko: "이슬비", en: "Drizzle", icon: "🌦️", mode: "rain", i: 0.35 },
  4001: { ko: "비", en: "Rain", icon: "🌧️", mode: "rain", i: 0.7 },
  4200: { ko: "약한 비", en: "Light rain", icon: "🌦️", mode: "rain", i: 0.45 },
  4201: { ko: "강한 비", en: "Heavy rain", icon: "🌧️", mode: "rain", i: 1.0 },
  5000: { ko: "눈", en: "Snow", icon: "🌨️", mode: "cloud", i: 0.7 },
  5100: { ko: "약한 눈", en: "Light snow", icon: "🌨️", mode: "cloud", i: 0.5 },
  5101: { ko: "강한 눈", en: "Heavy snow", icon: "❄️", mode: "cloud", i: 0.9 },
  8000: { ko: "뇌우", en: "Thunderstorm", icon: "⛈️", mode: "rain", i: 1.0 },
};
// single source of truth — the chip AND the on-map effect both read this, so they always agree
function wxInfo(wx: WxData): { ko: string; en: string; icon: string; mode: FxMode; intensity: number; storm: boolean } {
  const code = wx.weather_code ?? -1;
  const rain = wx.precip_mm_hr ?? 0;
  const vis = wx.visibility_km ?? 20;
  let info = WX_CODES[code] ?? (rain > 0.03
    ? { ko: "비", en: "Rain", icon: "🌧️", mode: "rain" as FxMode, i: 0.6 }
    : vis < 8 ? { ko: "흐림", en: "Cloudy", icon: "☁️", mode: "cloud" as FxMode, i: 0.8 }
    : { ko: "맑음", en: "Clear", icon: "☀️", mode: "clear" as FxMode, i: 1.0 });
  // if it's actually precipitating, trust the live precip over a possibly-stale code
  if (rain > 0.05 && info.mode !== "rain") info = { ko: rain >= 2 ? "강한 비" : "비", en: rain >= 2 ? "Heavy rain" : "Rain", icon: rain >= 2 ? "⛈️" : "🌧️", mode: "rain", i: 0.7 };
  // effect strength: code's base, but rain also scales up with live precip amount
  const intensity = info.mode === "rain"
    ? Math.min(1, Math.max(info.i, rain / 6))
    : info.i;
  return { ko: info.ko, en: info.en, icon: info.icon, mode: info.mode, intensity, storm: code === 8000 };
}

function WeatherFx({ mode, intensity, storm }: { mode: "rain" | "cloud" | "clear"; intensity: number; storm?: boolean }) {
  const cv = useRef<HTMLCanvasElement>(null);
  const flashRef = useRef<HTMLDivElement>(null);
  // lightning: random strikes (sometimes a quick double flash). Driven via a ref so it never
  // re-renders the component; the flash fades out each animation frame.
  useEffect(() => {
    if (!storm) return;
    let alive = true, raf = 0, schedule = 0, dbl = 0, level = 0;
    const tick = () => {
      level = level > 0.012 ? level * 0.78 - 0.006 : 0;
      if (flashRef.current) flashRef.current.style.opacity = String(Math.max(0, level));
      raf = requestAnimationFrame(tick);
    };
    const strike = () => {
      if (!alive) return;
      level = 0.7 + Math.random() * 0.3;
      if (Math.random() < 0.45) dbl = window.setTimeout(() => { if (alive) level = 0.55 + Math.random() * 0.3; }, 90 + Math.random() * 80);
      schedule = window.setTimeout(strike, 2600 + Math.random() * 6500);
    };
    const start = window.setTimeout(strike, 700 + Math.random() * 1800);
    raf = requestAnimationFrame(tick);
    return () => { alive = false; cancelAnimationFrame(raf); clearTimeout(start); clearTimeout(schedule); clearTimeout(dbl); };
  }, [storm]);
  useEffect(() => {
    if (mode !== "rain") return;
    const c = cv.current;
    if (!c) return;
    const ctx = c.getContext("2d");
    if (!ctx) return;
    let raf = 0;
    const fit = () => { c.width = c.clientWidth; c.height = c.clientHeight; };
    fit();
    const ro = new ResizeObserver(fit);
    ro.observe(c);
    const n = Math.round(140 + intensity * 260);
    const wind = 1.5 + intensity * 2.5;
    const drops = Array.from({ length: n }, () => ({ x: Math.random(), y: Math.random(), l: 0.012 + Math.random() * 0.022, v: 0.011 + Math.random() * 0.013 + intensity * 0.012 }));
    const loop = () => {
      const w = c.width, h = c.height;
      ctx.clearRect(0, 0, w, h);
      ctx.strokeStyle = `rgba(185,205,235,${0.22 + intensity * 0.28})`;
      ctx.lineWidth = 1;
      ctx.beginPath();
      for (const d of drops) {
        const x = d.x * w, y = d.y * h;
        ctx.moveTo(x, y);
        ctx.lineTo(x + wind, y + d.l * h);
        d.y += d.v;
        d.x += wind * 0.0015;
        if (d.y > 1) { d.y = -0.05; d.x = Math.random(); }
        if (d.x > 1.06) d.x -= 1.12;
      }
      ctx.stroke();
      raf = requestAnimationFrame(loop);
    };
    loop();
    return () => { cancelAnimationFrame(raf); ro.disconnect(); };
  }, [mode, intensity]);

  const fill = { position: "absolute", inset: 0 } as const;
  return (
    <div style={{ position: "absolute", inset: 0, pointerEvents: "none", zIndex: 4, overflow: "hidden" }}>
      {mode === "rain" && (
        <>
          <div style={{ ...fill, background: `linear-gradient(180deg, rgba(18,26,44,${0.15 + intensity * 0.18}), rgba(10,18,36,${0.24 + intensity * 0.2}))` }} />
          <canvas ref={cv} style={{ ...fill, width: "100%", height: "100%" }} />
        </>
      )}
      {mode === "cloud" && (
        <>
          <div style={{ ...fill, background: `radial-gradient(140% 110% at 50% -15%, rgba(160,170,190,${0.10 + intensity * 0.12}), rgba(56,66,88,${0.30 + intensity * 0.22}))` }} />
          <div style={{ ...fill, background: "rgba(110,120,140,0.10)", mixBlendMode: "saturation" }} />
        </>
      )}
      {mode === "clear" && (
        <>
          {/* the sun: a bright disc + soft halo, lower-left so the right-side panel never hides it.
              opacities scale with intensity → 대체로 맑음(0.7) 보다 맑음(1.0)이 더 쨍하게 */}
          <div style={{ position: "absolute", top: "16%", left: "12%", width: 130, height: 130, borderRadius: "50%",
            background: `radial-gradient(circle, rgba(255,252,225,${0.98 * intensity}) 0%, rgba(255,232,150,${0.85 * intensity}) 26%, rgba(255,212,120,${0.35 * intensity}) 52%, rgba(255,200,110,0) 74%)`,
            filter: "blur(1px)", mixBlendMode: "screen" }} />
          <div style={{ ...fill, background: `radial-gradient(75% 70% at 14% 16%, rgba(255,234,165,${0.42 * intensity}), rgba(255,214,135,0) 68%)`, mixBlendMode: "screen" }} />
          <div style={{ ...fill, background: `linear-gradient(135deg, rgba(255,243,200,${0.16 * intensity}), rgba(255,238,195,0.03) 55%)` }} />
        </>
      )}
      {/* lightning flash (thunderstorm) — opacity driven by the ref, fades each frame */}
      {storm && <div ref={flashRef} style={{ ...fill, background: "radial-gradient(95% 80% at 50% 26%, rgba(228,238,255,0.95), rgba(175,200,255,0.5) 58%, rgba(150,180,255,0) 84%)", mixBlendMode: "screen", opacity: 0 }} />}
    </div>
  );
}

export default function LiveMapPage({ lang }: { lang: Lang }) {
  const ko = lang === "ko";
  const mapEl = useRef<HTMLDivElement>(null);
  const mapRef = useRef<maplibregl.Map | null>(null);
  const replayRef = useRef<Replay | null>(null);
  const layoutRef = useRef<GeoJSON.FeatureCollection | null>(null);
  const nodesLoaded = useRef(false);
  const linksLoaded = useRef(false);
  const [ready, setReady] = useState(false);

  const [equipSet, setEquipSet] = useState<Set<EquipKey>>(() => new Set(ALL_EQUIP)); // multi-select
  const [equipCounts, setEquipCounts] = useState<Record<string, number>>({});
  const [stateFilter, setStateFilter] = useState<string | null>(null);
  const [dispatchFilter, setDispatchFilter] = useState<Dispatch | null>(null);
  const [toggles, setToggles] = useState<Toggles>(DEFAULT_TOGGLES);
  const [showGrid, setShowGrid] = useState(false); // metric grid overlay
  const [showWorkPts, setShowWorkPts] = useState(false); // dispatched work points (TOS vs ours); replaces the old advisory
  const workPtsRef = useRef<WorkPoint[]>([]);
  const selectedWpRef = useRef<WorkPoint | null>(null); // the clicked work point (for re-anchoring its truck lines)
  const dispPosRef = useRef<Map<string, [number, number]>>(new Map()); // each device's currently DISPLAYED (smoothed) position
  const [showWharf, setShowWharf] = useState(false); // learned wharf/quay positions overlay
  const [showRoadGraph, setShowRoadGraph] = useState(false); // GPS-inferred road network (replaces imported links)
  const roadGraphLoaded = useRef(false);
  const [roadGraphStats, setRoadGraphStats] = useState<{ nodes: number; edges: number; km: number; workpoints: number; generated_at: string } | null>(null);
  const [showWeatherFx, setShowWeatherFx] = useState(true); // game-like weather effect overlay (default on; toggle via the weather chip)
  const [gridM, setGridM] = useState(100); // grid cell size (m), adjustable
  const [gridMetric, setGridMetric] = useState<"speed" | "count">("speed"); // what the cell color shows
  const [panelOpen, setPanelOpen] = useState(true);
  const [counts, setCounts] = useState({ total: 0, moving: 0, idle: 0, off: 0 });
  const [tpos, setTpos] = useState(0);
  const filterRef = useRef<{ equip: Set<EquipKey>; state: string | null; dispatch: Dispatch | null }>({ equip: equipSet, state: stateFilter, dispatch: null });
  filterRef.current = { equip: equipSet, state: stateFilter, dispatch: dispatchFilter };
  const toggleEquip = (k: EquipKey) => setEquipSet((s) => { const n = new Set(s); n.has(k) ? n.delete(k) : n.add(k); return n; });

  // live feed: poll /api/livemap/positions; fall back to replay when it's empty/down.
  const liveRef = useRef<LiveSnap | null>(null);
  const [useLive, setUseLive] = useState(true);
  const useLiveRef = useRef(useLive);
  useLiveRef.current = useLive;
  const [delayMin, setDelayMin] = useState(5 / 60); // default 5s smooth: interpolate + hold through brief GPS gaps/spikes (less flicker than realtime)
  const delayMinRef = useRef(5 / 60); delayMinRef.current = delayMin;
  const histRef = useRef<Map<string, { cls: string; pts: Pt[] }>>(new Map()); // per-device fix buffer
  const clockOffsetRef = useRef(0); // server(as_of) − client(Date.now): a continuous, skew-free time base
  const gpsEventsRef = useRef<Array<[number, boolean]>>([]); // [t, isOutlier] over a rolling window
  const [gpsHealth, setGpsHealth] = useState({ outliers: 0, total: 0 }); // impossible-jump rate (5 min)
  const [wx, setWx] = useState<{ precip_mm_hr: number | null; visibility_km: number | null; wind_ms: number | null; weather_code: number | null; age_s: number } | null>(null);
  const [liveInfo, setLiveInfo] = useState<{ connected: boolean; count: number; asOf: string | null }>({ connected: false, count: 0, asOf: null });
  const [dispatchCounts, setDispatchCounts] = useState<Record<string, number>>({});

  // clicked-vehicle detail panel
  const [selDev, setSelDev] = useState<SelVeh | null>(null);
  const selRef = useRef<string | null>(null);
  const koRef = useRef(ko); koRef.current = ko;        // fresh lang for once-bound map click handlers
  const gridMRef = useRef(gridM); gridMRef.current = gridM;
  const pickRef = useRef<(id: string, lon: number, lat: number, speed: number) => void>(() => {});
  pickRef.current = (id, lon, lat, speed) => {
    selRef.current = id;
    const live = liveRef.current?.devices.find((d) => d.id === id);
    if (live) setSelDev(live);
    else setSelDev({ id, cls: id.match(/^[A-Za-z]+/)?.[0] ?? "", lat, lon, speed, engine: 0, age_s: 0 });
  };
  const closePanel = () => { selRef.current = null; setSelDev(null); };
  // e2e/debug hook (only with ?debug): open the detail panel for a device id without a
  // map click — the map canvas can't render markers on this GPU-less server.
  if (typeof window !== "undefined" && new URLSearchParams(window.location.search).has("debug")) {
    const w = window as unknown as { __wpPick?: (id: string) => void; __wpmap?: maplibregl.Map | null };
    w.__wpPick = (id: string) => pickRef.current(id, 0, 0, 0);
    w.__wpmap = mapRef.current;
  }

  // live weather chip (Tomorrow.io 1-min → /api/weather); null until the collector has a key
  useEffect(() => {
    const poll = () => fetch("/api/weather").then((r) => r.json()).then(setWx).catch(() => {});
    poll();
    const iv = setInterval(poll, 60000);
    return () => clearInterval(iv);
  }, []);

  // metric grid overlay: apply color (by metric) + visibility, and rebuild cells from the live
  // feed (current TT density / avg speed per cell) on a timer + on any control change.
  useEffect(() => {
    const m = mapRef.current;
    if (!m || !m.getLayer("mgrid-fill")) return;
    m.setPaintProperty("mgrid-fill", "fill-color", gridMetric === "speed" ? SPEED_COLOR : COUNT_COLOR);
    const vis = showGrid ? "visible" : "none";
    m.setLayoutProperty("mgrid-fill", "visibility", vis);
    m.setLayoutProperty("mgrid-line", "visibility", vis);
    if (!showGrid) return;
    const rebuild = () =>
      (m.getSource("mgrid") as maplibregl.GeoJSONSource | undefined)?.setData(
        buildMetricGrid(liveRef.current?.devices ?? [], gridM),
      );
    rebuild();
    const iv = setInterval(rebuild, 2500);
    return () => clearInterval(iv);
  }, [showGrid, gridM, gridMetric, ready]);

  // Dispatched work points overlay: poll the current points (15s) when shown. The selected point's
  // truck lines + rings are re-drawn every frame by the render loop (glued to smoothed positions).
  useEffect(() => {
    if (!ready || !showWorkPts) return;
    let alive = true;
    const poll = () => api.stage2WorkPoints().then((pts) => {
      if (!alive) return;
      workPtsRef.current = pts;
      const src = mapRef.current?.getSource("workpts") as maplibregl.GeoJSONSource | undefined;
      if (src) src.setData(workPointsFC(pts));
      // keep the selected point in sync with fresh data
      const sel = selectedWpRef.current;
      if (sel) selectedWpRef.current = pts.find((p) => p.qc === sel.qc && p.queuename === sel.queuename) ?? sel;
    }).catch(() => {});
    poll();
    const iv = setInterval(poll, 15000);
    return () => { alive = false; clearInterval(iv); selectedWpRef.current = null; };
  }, [ready, showWorkPts]);

  // work-points overlay visibility
  useEffect(() => {
    const map = mapRef.current;
    if (!map) return;
    for (const id of ["workpts-lines", "workpts-trucks", "workpts-pt", "workpts-label"]) {
      if (map.getLayer(id)) map.setLayoutProperty(id, "visibility", showWorkPts ? "visible" : "none");
    }
    if (!showWorkPts) {
      selectedWpRef.current = null;
      for (const sid of ["workpts-lines", "workpts-trucks"]) { const s = map.getSource(sid) as maplibregl.GeoJSONSource | undefined; if (s) s.setData(EMPTY_FC); }
    }
  }, [showWorkPts, ready]);

  // wharf overlay: learned quay-segment positions (changes slowly → poll 60s when shown)
  useEffect(() => {
    if (!ready || !showWharf) return;
    let alive = true;
    const load = () =>
      api.livemapWharf().then((pts) => {
        if (!alive) return;
        const feats: GeoJSON.Feature[] = pts.map((p) => ({
          type: "Feature",
          geometry: { type: "Point", coordinates: [p.lon, p.lat] },
          properties: { topos: p.topos, n: p.n, spread: p.spread_m != null ? Math.round(p.spread_m) : null },
        }));
        // zone = rectangle aligned to the local quay direction (from neighbouring wharf points)
        const src = mapRef.current?.getSource("wharf") as maplibregl.GeoJSONSource | undefined;
        src?.setData({ type: "FeatureCollection", features: feats });
        (mapRef.current?.getSource("wharf-zone") as maplibregl.GeoJSONSource | undefined)?.setData(wharfZonesFC(pts));
      }).catch(() => {});
    load();
    const iv = setInterval(load, 60000);
    return () => { alive = false; clearInterval(iv); };
  }, [showWharf, ready]);

  // wharf overlay visibility
  useEffect(() => {
    const map = mapRef.current;
    if (!map) return;
    for (const id of ["wharf-zone-fill", "wharf-zone-line", "wharf-pt", "wharf-label"]) {
      if (map.getLayer(id)) map.setLayoutProperty(id, "visibility", showWharf ? "visible" : "none");
    }
  }, [showWharf, ready]);

  // GPS-inferred road graph: centerlines (static GeoJSON) + direction arrows (learned lane field) —
  // both loaded once when first shown. The lane field IS the road's learned directionality (one-way/flow).
  useEffect(() => {
    if (!ready || !showRoadGraph || roadGraphLoaded.current) return;
    roadGraphLoaded.current = true;
    fetch("/livemap-roadgraph.geojson", { cache: "no-store" }).then((r) => r.json()).then((fc) => {
      (mapRef.current?.getSource("roadgraph") as maplibregl.GeoJSONSource | undefined)?.setData(fc);
      if (fc.stats) setRoadGraphStats(fc.stats);
    }).catch(() => { roadGraphLoaded.current = false; });
    api.learnLanes().then((l) => {
      (mapRef.current?.getSource("learn-lanes") as maplibregl.GeoJSONSource | undefined)?.setData({ type: "FeatureCollection", features: laneSegments(l.grid) });
    }).catch(() => {});
  }, [showRoadGraph, ready]);
  useEffect(() => {
    const map = mapRef.current;
    const v = showRoadGraph ? "visible" : "none";
    if (map?.getLayer("roadgraph-line")) map.setLayoutProperty("roadgraph-line", "visibility", v);
    if (map?.getLayer("roadgraph-wp")) map.setLayoutProperty("roadgraph-wp", "visibility", v);
    if (map?.getLayer("roadgraph-node")) map.setLayoutProperty("roadgraph-node", "visibility", v);
    if (map?.getLayer("ll-seg")) map.setLayoutProperty("ll-seg", "visibility", v);
  }, [showRoadGraph, ready]);

  // init map once
  useEffect(() => {
    if (!mapEl.current) return;
    const map = new maplibregl.Map({
      container: mapEl.current,
      style: { version: 8, sources: { esri: { type: "raster", tiles: [ESRI], tileSize: 256 } }, layers: [{ id: "esri", type: "raster", source: "esri" }] },
      center: [101.2919, 2.9263], zoom: 14.3, bearing: initialBearing(), attributionControl: false, preserveDrawingBuffer: true,
    } as maplibregl.MapOptions);
    mapRef.current = map;
    // compass = free user rotation (drag the needle / right-click-drag) + click to reset north
    map.addControl(new maplibregl.NavigationControl({ showCompass: true }), "bottom-right");
    // persist the user's last rotation so the map opens at the same orientation next time
    map.on("rotateend", () => { try { localStorage.setItem(BEARING_KEY, String(Math.round(map.getBearing() * 10) / 10)); } catch { /* ignore */ } });
    const ro = new ResizeObserver(() => map.resize());
    ro.observe(mapEl.current);

    map.on("load", () => {
      map.resize();
      // ── areas (blocks + roads) ──
      map.addSource("layout", { type: "geojson", data: layoutRef.current ?? { type: "FeatureCollection", features: [] } });
      map.addLayer({ id: "lay-road-fill", type: "fill", source: "layout", filter: ["==", ["get", "kind"], "road"], paint: { "fill-color": "#cbd5e1", "fill-opacity": 0.22 } });
      map.addLayer({ id: "lay-road-line", type: "line", source: "layout", filter: ["==", ["get", "kind"], "road"], paint: { "line-color": "#e2e8f0", "line-opacity": 0.45, "line-width": 0.5 } });
      map.addLayer({ id: "lay-block-fill", type: "fill", source: "layout", filter: ["==", ["get", "kind"], "block"], paint: { "fill-color": "#7eb6ff", "fill-opacity": 0.13 } });
      map.addLayer({ id: "lay-block-line", type: "line", source: "layout", filter: ["==", ["get", "kind"], "block"], paint: { "line-color": "#7eb6ff", "line-opacity": 0.5, "line-width": 0.6 } });
      map.addLayer({ id: "lay-block-label", type: "symbol", source: "layout", filter: ["==", ["get", "kind"], "block"], minzoom: 16, layout: { "text-field": ["get", "id"], "text-size": 9 }, paint: { "text-color": "#cfe3ff", "text-halo-color": "#0a0f1d", "text-halo-width": 1 } });

      // ── links (arcs) — empty until lazy-loaded ──
      map.addSource("links", { type: "geojson", data: { type: "FeatureCollection", features: [] } });
      for (const k of Object.keys(LINK_LAYERS)) {
        const L = LINK_LAYERS[k];
        map.addLayer({ id: `lnk-${k}`, type: "line", source: "links", filter: ["==", ["get", "t"], L.t], layout: { visibility: "none" }, paint: { "line-color": L.color, "line-opacity": 0.55, "line-width": ["interpolate", ["linear"], ["zoom"], 13, 0.4, 17, 1.4] } });
      }
      // ── nodes (points) — empty until lazy-loaded ──
      map.addSource("nodes", { type: "geojson", data: { type: "FeatureCollection", features: [] } });
      for (const k of Object.keys(NODE_LAYERS)) {
        const N = NODE_LAYERS[k];
        map.addLayer({ id: `nd-${k}`, type: "circle", source: "nodes", filter: ["==", ["get", "cat"], N.cat], layout: { visibility: "none" }, paint: { "circle-radius": ["interpolate", ["linear"], ["zoom"], 13, 1.3, 17, 3.5], "circle-color": N.color, "circle-opacity": 0.85 } });
      }

      // ── Dispatched work points (TOS vs ours) — click a point to draw lines to its trucks + compare.
      // Pin icon: green = we'd pick the same truck as TOS; magenta = we differ. (Replaces advisory.)
      if (!map.hasImage("wp-agree")) map.addImage("wp-agree", pinImage("#22c55e"), { pixelRatio: 2 });
      if (!map.hasImage("wp-diff")) map.addImage("wp-diff", pinImage("#e0218a"), { pixelRatio: 2 });
      map.addSource("workpts-lines", { type: "geojson", data: EMPTY_FC });
      map.addLayer({
        id: "workpts-lines",
        type: "line",
        source: "workpts-lines",
        layout: { visibility: "none", "line-cap": "round" },
        paint: {
          "line-color": ["match", ["get", "kind"], "both", "#22c55e", "our", "#a78bfa", "#22d3ee"],
          "line-width": ["match", ["get", "kind"], "tos", 1.6, 2.4],
          "line-opacity": 0.9,
        },
      });
      // a ring around each truck dispatched to the selected work point
      map.addSource("workpts-trucks", { type: "geojson", data: EMPTY_FC });
      map.addLayer({
        id: "workpts-trucks",
        type: "circle",
        source: "workpts-trucks",
        layout: { visibility: "none" },
        paint: {
          "circle-radius": ["interpolate", ["linear"], ["zoom"], 12, 7, 17, 15],
          "circle-color": ["match", ["get", "kind"], "both", "rgba(34,197,94,0.18)", "our", "rgba(167,139,250,0.18)", "rgba(34,211,238,0.16)"],
          "circle-stroke-width": 2.5,
          "circle-stroke-color": ["match", ["get", "kind"], "both", "#22c55e", "our", "#a78bfa", "#22d3ee"],
          "circle-stroke-opacity": 0.95,
        },
      });
      map.addSource("workpts", { type: "geojson", data: EMPTY_FC });
      map.addLayer({
        id: "workpts-pt",
        type: "symbol",
        source: "workpts",
        layout: {
          visibility: "none",
          "icon-image": ["case", ["==", ["get", "agree"], 1], "wp-agree", "wp-diff"],
          "icon-size": ["interpolate", ["linear"], ["zoom"], 12, 0.6, 17, 1.05],
          "icon-anchor": "bottom",
          "icon-allow-overlap": true,
        },
      });
      map.addLayer({
        id: "workpts-label",
        type: "symbol",
        source: "workpts",
        minzoom: 14,
        layout: { visibility: "none", "text-field": ["concat", ["get", "qc"], " ", ["get", "queuename"]], "text-size": 10, "text-offset": [0, 0.6], "text-anchor": "top", "text-allow-overlap": false },
        paint: { "text-color": "#e2e8f0", "text-halo-color": "#0a0f1d", "text-halo-width": 1.2 },
      });

      // ── learned wharf/quay positions (from cur_loc=WHARF_*) — empty until polled ──
      // zone (filled area sized by the learned spread) renders first, point+label on top.
      map.addSource("wharf-zone", { type: "geojson", data: EMPTY_FC });
      map.addLayer({ id: "wharf-zone-fill", type: "fill", source: "wharf-zone", layout: { visibility: "none" }, paint: { "fill-color": "#38bdf8", "fill-opacity": 0.13 } });
      map.addLayer({ id: "wharf-zone-line", type: "line", source: "wharf-zone", layout: { visibility: "none" }, paint: { "line-color": "#38bdf8", "line-opacity": 0.55, "line-width": 1 } });
      // GPS-inferred road network (static GeoJSON from scripts/build_road_graph.py) — replaces imported links
      map.addSource("roadgraph", { type: "geojson", data: EMPTY_FC });
      // work-points first (subtle anchor cloud), then edges, then junction NODES on top (the graph reads
      // as nodes+edges, not just roads).
      map.addLayer({ id: "roadgraph-wp", type: "circle", source: "roadgraph", filter: ["==", ["get", "kind"], "workpoint"], layout: { visibility: "none" }, paint: { "circle-radius": ["interpolate", ["linear"], ["zoom"], 13, 0.5, 17, 1.6], "circle-color": "#22d3ee", "circle-opacity": 0.28 } });
      map.addLayer({ id: "roadgraph-line", type: "line", source: "roadgraph", filter: ["==", ["get", "kind"], "road"], layout: { visibility: "none" }, paint: { "line-color": "#a78bfa", "line-opacity": 0.9, "line-width": ["interpolate", ["linear"], ["zoom"], 13, 1.2, 17, 3] } });
      map.addLayer({ id: "roadgraph-node", type: "circle", source: "roadgraph", filter: ["==", ["get", "kind"], "node"], layout: { visibility: "none" }, paint: { "circle-radius": ["interpolate", ["linear"], ["zoom"], 13, 2, 17, 5], "circle-color": "#fde68a", "circle-stroke-color": "#7c3aed", "circle-stroke-width": 1.3, "circle-opacity": 0.95 } });
      map.addSource("wharf", { type: "geojson", data: EMPTY_FC });
      map.addLayer({
        id: "wharf-pt",
        type: "circle",
        source: "wharf",
        layout: { visibility: "none" },
        paint: {
          "circle-radius": ["interpolate", ["linear"], ["zoom"], 13, 3, 17, 7],
          "circle-color": "#38bdf8",
          "circle-opacity": 0.85,
          "circle-stroke-width": 1.2,
          "circle-stroke-color": "#0ea5e9",
        },
      });
      map.addLayer({
        id: "wharf-label",
        type: "symbol",
        source: "wharf",
        minzoom: 14,
        layout: { visibility: "none", "text-field": ["get", "topos"], "text-size": 9, "text-offset": [0, 1.1], "text-anchor": "top" },
        paint: { "text-color": "#bae6fd", "text-halo-color": "#0a0f1d", "text-halo-width": 1.2 },
      });

      // ── vehicles (top) ──
      // ── learned-layout (work-points · driving lanes) + dispatch demand overlays ──
      map.addSource("learn-topos", { type: "geojson", data: EMPTY_FC });
      map.addLayer({
        id: "lt-pt", type: "circle", source: "learn-topos", layout: { visibility: "none" },
        paint: {
          "circle-radius": ["interpolate", ["linear"], ["zoom"], 12, ["case", [">=", ["get", "n"], 30], 3, 1.6], 17, ["case", [">=", ["get", "n"], 30], 7, 4]],
          // fill = learned accuracy/confidence (green high · amber med · red low); ring = block/crane
          "circle-color": ["match", ["get", "conf"], "high", "#22c55e", "med", "#f59e0b", "#ef4444"],
          "circle-opacity": 0.9,
          "circle-stroke-width": 1.4, "circle-stroke-color": ["case", ["get", "crane"], "#f59e0b", "#5eead4"],
        },
      });
      map.addSource("learn-lanes", { type: "geojson", data: EMPTY_FC });
      map.addLayer({
        id: "ll-seg", type: "line", source: "learn-lanes", layout: { visibility: "none", "line-cap": "round" },
        paint: {
          "line-color": ["step", ["get", "dir"], "#64748b", 0.5, "#f59e0b", 0.8, "#34d399"],
          "line-width": ["interpolate", ["linear"], ["zoom"], 13, 1, 17, 2.6],
          "line-opacity": 0.8,
        },
      });
      map.addSource("demand", { type: "geojson", data: EMPTY_FC });
      map.addLayer({
        id: "dm-bub", type: "circle", source: "demand", layout: { visibility: "none" },
        paint: {
          "circle-radius": ["interpolate", ["linear"], ["sqrt", ["get", "n"]], 1, 5, 9, 24],
          "circle-color": ["case", ["==", ["get", "jt"], "DS"], "#fb923c", "#22d3ee"],
          "circle-opacity": 0.26,
          "circle-stroke-width": 1.3, "circle-stroke-color": ["case", ["==", ["get", "jt"], "DS"], "#fb923c", "#22d3ee"],
        },
      });

      // metric grid: filled cells colored by a live metric (avg speed / density), client-side.
      map.addSource("mgrid", { type: "geojson", data: EMPTY_FC });
      map.addLayer({ id: "mgrid-fill", type: "fill", source: "mgrid", layout: { visibility: "none" }, paint: { "fill-color": SPEED_COLOR, "fill-opacity": 0.45 } });
      map.addLayer({ id: "mgrid-line", type: "line", source: "mgrid", layout: { visibility: "none" }, paint: { "line-color": "#334155", "line-opacity": 0.35, "line-width": 0.5 } });

      map.addSource("vehicles", { type: "geojson", data: { type: "FeatureCollection", features: [] } });
      addEquipIcons(map); // equipment shapes × state colors
      // marker shape = equipment (eq), color = state — icon name is "<eq>-<state>".
      map.addLayer({
        id: "veh", type: "symbol", source: "vehicles",
        layout: {
          "icon-image": ["concat", ["get", "eq"], "-", ["get", "state"]],
          "icon-size": ["interpolate", ["linear"], ["zoom"], 13, 0.5, 16, 0.8, 18, 1.1],
          "icon-allow-overlap": true, "icon-ignore-placement": true,
        },
      });
      map.addLayer({ id: "veh-label", type: "symbol", source: "vehicles", minzoom: 15.5, layout: { "text-field": ["get", "id"], "text-size": 9, "text-offset": [0, 1.1], "text-anchor": "top" }, paint: { "text-color": "#e2e8f0", "text-halo-color": "#0a0f1d", "text-halo-width": 1 } });

      map.on("click", "veh", (e) => {
        const f = e.features?.[0]; if (!f) return;
        const pr = f.properties as { id: string; speed: number };
        const c = (f.geometry as GeoJSON.Point).coordinates as [number, number];
        pickRef.current(pr.id, c[0], c[1], pr.speed);
      });
      map.on("mouseenter", "veh", () => { map.getCanvas().style.cursor = "pointer"; });
      map.on("mouseleave", "veh", () => { map.getCanvas().style.cursor = ""; });

      // click popups for learned overlays + metric-grid cells (koRef/gridMRef = fresh values)
      const lpop = new maplibregl.Popup({ closeButton: true, closeOnClick: true, maxWidth: "260px", className: "lm-popup" });
      const showPopup = (at: [number, number] | maplibregl.LngLat, html: string) => lpop.setLngLat(at).setHTML(html).addTo(map);
      // closing the popup clears the selected work point's truck lines + rings
      lpop.on("close", () => {
        selectedWpRef.current = null;
        for (const sid of ["workpts-lines", "workpts-trucks"]) { const s = map.getSource(sid) as maplibregl.GeoJSONSource | undefined; if (s) s.setData(EMPTY_FC); }
      });
      map.on("click", "lt-pt", (e) => {
        const f = e.features?.[0]; if (!f) return; const k = koRef.current;
        const p = f.properties as { topos: string; crane: boolean; n: number; obs: number; spread: number | null; conf: string };
        const kind = p.crane ? (k ? "안벽 크레인" : "quay crane") : (k ? "블록 작업점" : "block");
        const cf: Record<string, string> = k ? { high: "높음", med: "보통", low: "낮음" } : { high: "high", med: "med", low: "low" };
        showPopup((f.geometry as GeoJSON.Point).coordinates as [number, number],
          `<div class="lmp-t">${p.topos}</div>`
          + `<div class="lmp-r">${k ? "종류" : "kind"}: ${kind}</div>`
          + `<div class="lmp-r">${k ? "표본" : "samples"}: n=${p.n} · obs ${p.obs}</div>`
          + `<div class="lmp-r">${k ? "위치 정확도" : "precision"}: ${p.spread != null ? "±" + p.spread + "m" : "—"}</div>`
          + `<div class="lmp-r">${k ? "신뢰도" : "confidence"}: <b>${cf[p.conf] ?? p.conf}</b></div>`);
      });
      map.on("click", "ll-seg", (e) => {
        const f = e.features?.[0]; if (!f) return; const k = koRef.current;
        const p = f.properties as { dir: number; passes: number; heading: number; speed: number | null };
        const flow = p.dir >= 0.8 ? (k ? "일방통행" : "one-way") : p.dir >= 0.5 ? (k ? "대체로 일방" : "mostly one-way") : (k ? "양방/혼재" : "two-way/mixed");
        showPopup(e.lngLat,
          `<div class="lmp-t">${k ? "학습 차선" : "learned lane"}</div>`
          + `<div class="lmp-r">${k ? "방향" : "heading"}: ${p.heading}° (${flow})</div>`
          + `<div class="lmp-r">${k ? "방향성" : "directionality"}: ${(p.dir ?? 0).toFixed(2)}</div>`
          + `<div class="lmp-r">${k ? "평균속도" : "avg speed"}: ${p.speed != null ? p.speed + " km/h" : "—"}</div>`
          + `<div class="lmp-r">${k ? "통과 수" : "passes"}: ${p.passes}</div>`);
      });
      map.on("click", "mgrid-fill", (e) => {
        const f = e.features?.[0]; if (!f) return; const k = koRef.current;
        const p = f.properties as { count: number; speed: number };
        showPopup(e.lngLat,
          `<div class="lmp-t">${k ? "격자 셀" : "grid cell"} (${gridMRef.current}m)</div>`
          + `<div class="lmp-r">${k ? "트럭" : "trucks"}: ${p.count}${k ? "대" : ""}</div>`
          + `<div class="lmp-r">${k ? "평균속도" : "avg speed"}: ${p.speed} km/h</div>`);
      });
      // click a dispatched work point → draw lines to its dispatched trucks + TOS-vs-ours compare
      map.on("click", "workpts-pt", (e) => {
        const f = e.features?.[0]; if (!f) return; const k = koRef.current;
        const p = f.properties as { qc: string; queuename: string; jobtype: string; tos: string; our: string; tos_arr: number; our_arr: number; agree: number; delta: number; n: number; agree_n: number };
        const wp = workPtsRef.current.find((w) => w.qc === p.qc && w.queuename === p.queuename) ?? null;
        selectedWpRef.current = wp; // the render loop draws/updates its lines+rings each frame (smoothed)
        const pos = dispPosRef.current;
        const ls = map.getSource("workpts-lines") as maplibregl.GeoJSONSource | undefined;
        if (ls) ls.setData(wp ? workLinesFC(wp, pos) : EMPTY_FC);
        const ts = map.getSource("workpts-trucks") as maplibregl.GeoJSONSource | undefined;
        if (ts) ts.setData(wp ? workTruckPtsFC(wp, pos) : EMPTY_FC);
        const fmt = (s: number) => s >= 0 ? `${Math.floor(s / 60)}:${String(Math.round(s % 60)).padStart(2, "0")}` : "—";
        const job = p.jobtype === "LD" ? (k ? "적하" : "LD") : p.jobtype === "DS" ? (k ? "양하" : "DS") : p.jobtype;
        const tosN = wp?.tos_trucks?.length ?? 0;
        const ourN = wp?.our_trucks?.length ?? 0;
        const common = wp ? (wp.tos_trucks ?? []).filter((t) => (wp.our_trucks ?? []).includes(t)).length : 0;
        const avg = wp?.avg_delta_s ?? null;
        const avgTxt = avg == null ? "" : avg > 0 ? (k ? `평균 우리가 ${fmt(Math.abs(avg))} 빠름` : `avg ours ${fmt(Math.abs(avg))} sooner`) : (k ? `평균 우리가 ${fmt(Math.abs(avg))} 늦음` : `avg ours ${fmt(Math.abs(avg))} later`);
        showPopup((f.geometry as GeoJSON.Point).coordinates as [number, number],
          `<div class="lmp-t">${p.qc} · ${p.queuename} <span style="color:#94a3b8;font-weight:400">${job}</span></div>`
          + `<div class="lmp-r" style="margin-top:4px">🔗 ${k ? "최근 1시간 배차 트럭" : "trucks (last hour)"}</div>`
          + `<div class="lmp-r" style="display:flex;gap:11px;font-weight:600"><span style="color:#22d3ee">TOS ${tosN}${k ? "대" : ""}</span><span style="color:#a78bfa">🤖 ${k ? "우리" : "ours"} ${ourN}${k ? "대" : ""}</span><span style="color:#22c55e">${k ? "공통" : "both"} ${common}</span></div>`
          + `<div class="lmp-r" style="color:#64748b;font-size:10px">${k ? `배차 결정 ${p.n}건 · 동일 트럭 ${p.agree_n}건` : `${p.n} decisions · ${p.agree_n} same`}${avgTxt ? ` · ${avgTxt}` : ""}</div>`);
      });
      for (const id of ["lt-pt", "ll-seg", "mgrid-fill", "workpts-pt"]) {
        map.on("mouseenter", id, () => { map.getCanvas().style.cursor = "pointer"; });
        map.on("mouseleave", id, () => { map.getCanvas().style.cursor = ""; });
      }
      setReady(true);
    });
    return () => { ro.disconnect(); map.remove(); mapRef.current = null; };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // fetch replay + layout(areas) immediately
  useEffect(() => {
    fetch("/livemap-replay.json").then((r) => r.json()).then((j: Replay) => { replayRef.current = j; });
    fetch("/livemap-layout.json").then((r) => r.json()).then((j: GeoJSON.FeatureCollection) => {
      layoutRef.current = j;
      (mapRef.current?.getSource("layout") as maplibregl.GeoJSONSource | undefined)?.setData(j);
    });
  }, []);
  useEffect(() => {
    if (ready && layoutRef.current) (mapRef.current?.getSource("layout") as maplibregl.GeoJSONSource | undefined)?.setData(layoutRef.current);
  }, [ready]);

  // poll the live GPS feed (~2.5s). Keeps the last good snapshot on a transient error.
  useEffect(() => {
    let alive = true;
    const poll = async () => {
      try {
        const r = await fetch("/api/livemap/positions");
        if (!r.ok) throw new Error(String(r.status));
        const j: LiveSnap = await r.json();
        if (!alive) return;
        liveRef.current = j;
        // buffer each device's DISTINCT fixes (keyed by fix time) for smooth delayed playback,
        // and track the server clock so the render loop can advance a continuous display time.
        {
          const asOfMs = j.as_of ? Date.parse(j.as_of) : Date.now();
          clockOffsetRef.current = asOfMs - Date.now();
          const nowMs = Date.now();
          const hist = histRef.current;
          const ev = gpsEventsRef.current;
          const keepMs = asOfMs - (MAX_DELAY_MIN * 60 + 30) * 1000;
          for (const d of j.devices) {
            const fixMs = asOfMs - Math.max(0, d.age_s) * 1000; // actual GPS fix time
            let e = hist.get(d.id);
            if (!e) { e = { cls: d.cls, pts: [] }; hist.set(d.id, e); }
            const last = e.pts[e.pts.length - 1];
            if (!last) { e.pts.push([fixMs, d.lat, d.lon, d.speed, d.engine]); continue; } // first fix
            if (fixMs <= last[0] + 500) continue; // stationary repeat — not a new fix
            // NEW fix: reject physically-impossible jumps (GPS spikes) over the real fix-time gap.
            const gapS = (fixMs - last[0]) / 1000;
            const impliedKmh = gapS > 0 ? metersBetween(last[1], last[2], d.lat, d.lon) / gapS * 3.6 : Infinity;
            const outlier = impliedKmh > MAX_PLAUSIBLE_KMH;
            ev.push([nowMs, outlier]);
            if (outlier) continue; // snap: hold the last good position, drop the spike (no "flying")
            e.pts.push([fixMs, d.lat, d.lon, d.speed, d.engine]);
            while (e.pts.length > 1 && e.pts[0][0] < keepMs) e.pts.shift();
          }
          for (const [id, e] of hist) if (e.pts.length && e.pts[e.pts.length - 1][0] < keepMs) hist.delete(id);
          // GPS-quality indicator: impossible-jump rate over a rolling 5-minute window.
          const cut = nowMs - 5 * 60000;
          while (ev.length && ev[0][0] < cut) ev.shift();
          let bad = 0; for (const [, o] of ev) if (o) bad++;
          setGpsHealth({ outliers: bad, total: ev.length });
        }
        setLiveInfo({ connected: j.connected, count: j.count, asOf: j.as_of });
        setDispatchCounts(j.dispatch_counts ?? {});
        const ec: Record<string, number> = { TT: 0, RTG: 0, QC: 0, ETC: 0 };
        for (const d of j.devices) ec[equip(d.cls)]++;
        setEquipCounts(ec);
        // keep the open detail panel fresh
        if (selRef.current) {
          const d = j.devices.find((x) => x.id === selRef.current);
          if (d) setSelDev(d);
        }
      } catch {
        if (alive) setLiveInfo((p) => ({ ...p, connected: false }));
      }
    };
    poll();
    const iv = setInterval(poll, 2500);
    return () => { alive = false; clearInterval(iv); };
  }, []);

  // lazy-load nodes/links on first use, then apply visibility for every toggle change
  useEffect(() => {
    const map = mapRef.current;
    if (!map || !ready) return;
    const anyNode = NODE_LAYERS && Object.values(NODE_LAYERS).some((n) => toggles[n.key]);
    const anyLink = Object.values(LINK_LAYERS).some((l) => toggles[l.key]);
    if (anyNode && !nodesLoaded.current) {
      nodesLoaded.current = true;
      fetch("/livemap-nodes.json").then((r) => r.json()).then((j) => (map.getSource("nodes") as maplibregl.GeoJSONSource).setData(j));
    }
    if (anyLink && !linksLoaded.current) {
      linksLoaded.current = true;
      fetch("/livemap-links.json").then((r) => r.json()).then((j) => (map.getSource("links") as maplibregl.GeoJSONSource).setData(j));
    }
    const vis = (on: boolean) => (on ? "visible" : "none");
    for (const id of AREA_LAYERS) if (map.getLayer(id)) map.setLayoutProperty(id, "visibility", vis(toggles.areas));
    for (const k of Object.keys(NODE_LAYERS)) if (map.getLayer(`nd-${k}`)) map.setLayoutProperty(`nd-${k}`, "visibility", vis(toggles[NODE_LAYERS[k].key]));
    for (const k of Object.keys(LINK_LAYERS)) if (map.getLayer(`lnk-${k}`)) map.setLayoutProperty(`lnk-${k}`, "visibility", vis(toggles[LINK_LAYERS[k].key]));
    if (map.getLayer("lt-pt")) map.setLayoutProperty("lt-pt", "visibility", vis(toggles.learnTopos));
    if (map.getLayer("dm-bub")) map.setLayoutProperty("dm-bub", "visibility", vis(toggles.demand));
  }, [toggles, ready]);

  // learned-layout (work-points / lanes) + dispatch demand overlays: fetch + build GeoJSON while toggled on.
  useEffect(() => {
    const map = mapRef.current;
    if (!map || !ready) return;
    const wantTopos = toggles.learnTopos || toggles.demand; // demand reuses topos coords
    if (!wantTopos) return;
    let alive = true;
    const load = async () => {
      try {
        if (wantTopos) {
          const t = await api.learnTopos();
          if (!alive) return;
          if (toggles.learnTopos) {
            const feats = t.points.map((p) => {
            // accuracy/confidence: tight cluster + enough samples = high. spread_m = positional precision.
            const sp = p.spread_m ?? 99;
            const conf = p.n >= 30 && sp <= 8 ? "high" : p.n >= 10 && sp <= 20 ? "med" : "low";
            return {
              type: "Feature",
              geometry: { type: "Point", coordinates: [p.lon, p.lat] },
              properties: { crane: p.is_crane, n: p.n, topos: p.topos, obs: p.obs, spread: p.spread_m != null ? Math.round(p.spread_m * 10) / 10 : null, conf },
            } as GeoJSON.Feature;
          });
            (map.getSource("learn-topos") as maplibregl.GeoJSONSource | undefined)?.setData({ type: "FeatureCollection", features: feats });
          }
          if (toggles.demand) {
            // resolve unassigned demand to learned coords: DS → its QC crane, LD → its source block.
            const crane = new Map<string, [number, number]>();
            const blk = new Map<string, { lon: number; lat: number; k: number }>();
            for (const p of t.points) {
              if (p.is_crane) crane.set(p.topos, [p.lon, p.lat]);
              else { const pre = p.topos.split("-")[0]; const a = blk.get(pre) ?? { lon: 0, lat: 0, k: 0 }; a.lon += p.lon; a.lat += p.lat; a.k++; blk.set(pre, a); }
            }
            const wp = await api.workpool();
            if (!alive) return;
            const feats: GeoJSON.Feature[] = [];
            for (const c of wp.candidates) {
              let coord: [number, number] | null = null;
              if (c.jobtype === "DS" && c.qc) coord = crane.get(c.qc) ?? null;
              else if (c.src_block) { const a = blk.get(c.src_block); if (a && a.k) coord = [a.lon / a.k, a.lat / a.k]; }
              if (!coord) continue;
              feats.push({ type: "Feature", geometry: { type: "Point", coordinates: coord }, properties: { jt: c.jobtype ?? "?", n: c.n } } as GeoJSON.Feature);
            }
            (map.getSource("demand") as maplibregl.GeoJSONSource | undefined)?.setData({ type: "FeatureCollection", features: feats });
          }
        }
      } catch { /* keep last good data */ }
    };
    load();
    const iv = setInterval(load, 15000);
    return () => { alive = false; clearInterval(iv); };
  }, [toggles.learnTopos, toggles.demand, ready]);

  // render loop — prefers the live feed, falls back to the captured replay.
  useEffect(() => {
    if (!ready) return;
    let raf = 0;
    const start = performance.now();
    const tick = () => {
      const map = mapRef.current;
      if (map && map.getSource("vehicles")) {
        const { equip: ef, state: sf, dispatch: df } = filterRef.current;
        const feats: GeoJSON.Feature[] = [];
        const dispPos = new Map<string, [number, number]>(); // displayed (smoothed) position per device id
        let moving = 0, idle = 0, off = 0;
        const live = liveRef.current;
        const liveOn = useLiveRef.current && live != null && live.devices.length > 0;
        if (liveOn && delayMinRef.current > 0) {
          // smooth playback: render at (server now − N min), interpolating each device's buffered track.
          // displayMs advances every frame via the continuous client clock → motion is smooth, not stepped.
          const hist = histRef.current;
          const serverNow = Date.now() + clockOffsetRef.current;
          let oldest = Infinity;
          for (const e of hist.values()) if (e.pts.length) oldest = Math.min(oldest, e.pts[0][0]);
          const displayMs = Math.max(serverNow - delayMinRef.current * 60000, oldest); // clamp → ramps up as buffer fills
          const disp = new Map<string, Dispatch | undefined>();
          for (const d of live!.devices) disp.set(d.id, d.dispatch);
          for (const [id, e] of hist) {
            const arr = e.pts;
            if (arr.length === 0) continue;
            const firstT = arr[0][0], lastT = arr[arr.length - 1][0];
            // HOLD through GPS gaps: keep the device at its last position for up to HOLD_MS after its
            // last fix (instead of dropping at 60s → flicker when fixes arrive >60s apart). Beyond
            // that it's genuinely stale and drops. (Spikes are already rejected upstream.)
            const HOLD_MS = 180000; // 3 min
            if (displayMs < firstT - 60000 || displayMs > lastT + HOLD_MS) continue;
            const t = Math.min(Math.max(displayMs, firstT), lastT);
            const p = posAt({ id, cls: e.cls, pts: arr }, t);
            if (!p) continue;
            dispPos.set(id, [p.lon, p.lat]);
            if (p.state === "moving") moving++; else if (p.state === "idle") idle++; else off++;
            const eq = equip(e.cls);
            if (!ef.has(eq)) continue;
            const dsp = disp.get(id);
            if (df && eq === "TT" && dsp !== df) continue;
            if (sf && p.state !== sf) continue;
            feats.push({ type: "Feature", geometry: { type: "Point", coordinates: [p.lon, p.lat] }, properties: { id, state: p.state, eq, speed: p.speed, dispatch: dsp ?? "" } });
          }
        } else if (liveOn) {
          // live GPS: place each device at its latest fix (no interpolation).
          for (const d of live!.devices) {
            dispPos.set(d.id, [d.lon, d.lat]);
            const st = stateOf(d.speed, d.engine);
            if (st === "moving") moving++; else if (st === "idle") idle++; else off++;
            const eq = equip(d.cls);
            if (!ef.has(eq)) continue; // equipment multi-select
            if (df && eq === "TT" && d.dispatch !== df) continue; // pool filter — TT only
            if (sf && st !== sf) continue;
            feats.push({ type: "Feature", geometry: { type: "Point", coordinates: [d.lon, d.lat] }, properties: { id: d.id, state: st, eq, speed: d.speed, dispatch: d.dispatch ?? "" } });
          }
        } else {
          // replay: interpolate along captured tracks on a real-time loop.
          const rep = replayRef.current;
          if (rep) {
            const win = rep.meta.window_s;
            const t = ((performance.now() - start) / 1000) % win;
            setTpos(Math.round(t));
            for (const d of rep.devices) {
              const p = posAt(d, t);
              if (!p) continue;
              if (p.state === "moving") moving++; else if (p.state === "idle") idle++; else off++;
              if (!ef.has(equip(d.cls))) continue;
              if (sf && p.state !== sf) continue;
              feats.push({ type: "Feature", geometry: { type: "Point", coordinates: [p.lon, p.lat] }, properties: { id: d.id, state: p.state, eq: equip(d.cls), speed: p.speed } });
            }
          }
        }
        (map.getSource("vehicles") as maplibregl.GeoJSONSource).setData({ type: "FeatureCollection", features: feats });
        setCounts({ total: moving + idle + off, moving, idle, off });
        // keep the selected work point's truck lines + rings glued to the smoothed marker positions
        dispPosRef.current = dispPos;
        const sel = selectedWpRef.current;
        if (sel) {
          const ls = map.getSource("workpts-lines") as maplibregl.GeoJSONSource | undefined;
          if (ls) ls.setData(workLinesFC(sel, dispPos));
          const ts = map.getSource("workpts-trucks") as maplibregl.GeoJSONSource | undefined;
          if (ts) ts.setData(workTruckPtsFC(sel, dispPos));
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [ready]);

  const win = replayRef.current?.meta.window_s ?? 180;
  const ndev = useMemo(() => replayRef.current?.meta.n_devices ?? 0, [ready]);
  const liveActive = useLive && liveInfo.connected && liveInfo.count > 0;
  const asOfAge = liveInfo.asOf ? Math.max(0, Math.round((Date.now() - Date.parse(liveInfo.asOf)) / 1000)) : null;
  const set = (k: LayerKey, v: boolean) => setToggles((t) => ({ ...t, [k]: v }));
  const activeCount = Object.values(toggles).filter(Boolean).length;

  return (
    <div className="map-page">
      {/* top: equipment multi-select filter (teal accent) with per-type counts */}
      <div className="map-top">
        <div className="map-equip">
          <button
            className={`meq all${equipSet.size === ALL_EQUIP.length ? " active" : ""}`}
            onClick={() => setEquipSet(new Set(ALL_EQUIP))}
            title={ko ? "전체 표시" : "Show all"}
          >
            {ko ? "전체" : "All"}<span className="meq-n">{liveActive ? liveInfo.count : ndev}</span>
          </button>
          {EQUIP_TABS.map((e) => (
            <button key={e.key} className={`meq${equipSet.has(e.key) ? " active" : ""}`} onClick={() => toggleEquip(e.key)} title={ko ? `${e.ko} 표시 전환` : `toggle ${e.en}`}>
              <EqGlyph eq={e.key} />{ko ? e.ko : e.en}<span className="meq-n">{equipCounts[e.key] ?? 0}</span>
            </button>
          ))}
        </div>
        <span className="spacer" />
        <button
          className="map-rotate"
          onClick={() => { const m = mapRef.current; if (m) { m.easeTo({ bearing: QUAY_BEARING, duration: 500 }); try { localStorage.setItem(BEARING_KEY, String(QUAY_BEARING)); } catch { /* ignore */ } } }}
          title={ko ? "선석을 수평으로 정렬 (바다 위 · 터미널 아래)" : "Align quay horizontal (sea up, terminal down)"}
        >
          ⟲ {ko ? "선석 수평" : "Align quay"}
        </button>
        <button
          className={`map-live ${liveActive ? "on" : "off"}`}
          onClick={() => setUseLive((v) => !v)}
          title={ko ? "라이브/리플레이 전환" : "Toggle live / replay"}
        >
          <span className="dot" />
          {liveActive ? (ko ? "라이브" : "LIVE") : (ko ? "리플레이" : "REPLAY")}
        </button>
        {liveActive && (
          <div className="map-delay" title={ko ? "지연 재생 — 그 사이 도착한 GPS로 차량 움직임을 부드럽게 보간" : "Delayed playback — interpolate motion from fixes that arrive during the delay"}>
            <span className="mdl-lbl">{ko ? "부드럽게" : "Smooth"}</span>
            {DELAY_OPTS.map((m) => (
              <button key={m} className={`mdl${delayMin === m ? " on" : ""}`} onClick={() => setDelayMin(m)}>
                {delayLbl(m, ko)}
              </button>
            ))}
          </div>
        )}
        <span className="map-count mono">{counts.total} / {liveActive ? liveInfo.count : ndev}</span>
        {liveActive ? (
          <span className="map-clock mono" title={liveInfo.asOf ?? ""}>⟳ {asOfAge != null ? `${asOfAge}s` : "—"}{delayMin > 0 ? ` · −${delayLbl(delayMin, ko)}` : ""}</span>
        ) : (
          <span className="map-clock mono">▶ t+{tpos}s / {win}s</span>
        )}
        {liveActive && gpsHealth.total > 0 && (() => {
          const pct = (gpsHealth.outliers / gpsHealth.total) * 100;
          const cls = pct >= 2 ? "bad" : pct >= 0.5 ? "warn" : "ok";
          return (
            <span className={`map-gps mono ${cls}`} title={ko ? `물리적으로 불가능한 GPS 점프(>${MAX_PLAUSIBLE_KMH}km/h) 비율 — 최근 5분 ${gpsHealth.outliers}/${gpsHealth.total}건` : `Impossible GPS jumps (>${MAX_PLAUSIBLE_KMH}km/h), last 5 min: ${gpsHealth.outliers}/${gpsHealth.total}`}>
              GPS튐 {pct.toFixed(1)}% <span className="map-gps-n">({gpsHealth.outliers})</span>
            </span>
          );
        })()}
        {wx && wx.age_s < 1800 && (() => {
          const rain = wx.precip_mm_hr ?? 0;
          const vis = wx.visibility_km;
          const squall = rain >= 2 || (vis != null && vis < 2); // heavy rain or crashed visibility
          // icon + label come from the SAME source as the on-map effect (wxInfo), so they always agree
          const info = wxInfo(wx);
          const icon = info.icon;
          const lbl = ko ? info.ko : info.en;
          return (
            <button className={`map-gps mono ${squall ? "bad" : "ok"}`}
              onClick={() => setShowWeatherFx((v) => !v)}
              style={{ cursor: "pointer", ...(showWeatherFx ? { borderColor: "#fcd34d", boxShadow: "0 0 0 1px rgba(252,211,77,0.7), 0 0 12px rgba(252,211,77,0.55)" } : {}) }}
              title={ko
                ? `클릭 = 날씨 효과 ${showWeatherFx ? "끄기" : "켜기"} (현재 ${showWeatherFx ? "ON" : "OFF"}) · 날씨 ${wx.age_s}s 전 · 강수 ${rain.toFixed(1)}mm/h · 시정 ${vis?.toFixed(1) ?? "—"}km · 바람 ${wx.wind_ms?.toFixed(0) ?? "—"}m/s${squall ? " · 스콜" : ""}`
                : `click to ${showWeatherFx ? "disable" : "enable"} weather effect · weather ${wx.age_s}s ago · rain ${rain.toFixed(1)}mm/h · vis ${vis?.toFixed(1) ?? "—"}km${squall ? " · SQUALL" : ""}`}>
              {icon} {lbl}{info.mode === "rain" ? ` ${rain.toFixed(1)}mm/h` : ""}{vis != null && vis < 5 ? ` · ${vis.toFixed(1)}km` : ""}
            </button>
          );
        })()}
      </div>

      {/* TT dispatch-pool filter — narrows TTs only (other equipment unaffected) */}
      {liveActive && equipSet.has("TT") && (
        <div className="map-dpf">
          <span className="map-dpf-lbl">{ko ? "TT 풀" : "TT pool"}</span>
          <button className={`dpf${dispatchFilter === null ? " active" : ""}`} onClick={() => setDispatchFilter(null)}>
            {ko ? "전체" : "All"}<span className="dpf-n">{equipCounts.TT ?? 0}</span>
          </button>
          {DISPATCH_POOLS.map((pl) => (
            <button
              key={pl.key}
              className={`dpf${dispatchFilter === pl.key ? " active" : ""}`}
              style={dispatchFilter === pl.key ? { borderColor: pl.color, background: `${pl.color}22`, color: pl.color } : undefined}
              onClick={() => setDispatchFilter(dispatchFilter === pl.key ? null : pl.key)}
            >
              <i style={{ background: pl.color }} />{ko ? pl.ko : pl.en}<span className="dpf-n">{dispatchCounts[pl.key] ?? 0}</span>
            </button>
          ))}
        </div>
      )}

      <div className="map-canvas" ref={mapEl} />
      {showWeatherFx && wx && wx.age_s < 1800 && (() => { const f = wxInfo(wx); return <WeatherFx mode={f.mode} intensity={f.intensity} storm={f.storm} />; })()}

      {/* right: TOS layer panel (areas / nodes / links) */}
      <aside className={`llp ${panelOpen ? "open" : "closed"}`}>
        <button className="llp-head" onClick={() => setPanelOpen((v) => !v)}>
          <span className="llp-title">{ko ? "레이어" : "Layers"}</span>
          <span className="llp-count">{activeCount} / {LAYER_TOTAL}</span>
          <span className="llp-chev">{panelOpen ? "▾" : "▸"}</span>
        </button>
        {panelOpen && (
          <div className="llp-body">
            <section className="llp-sec">
              <header>{ko ? "영역" : "Areas"}</header>
              <Row on={toggles.areas} color="#7eb6ff" label={ko ? "도로/블록 영역" : "Road/Block"} onChange={(v) => set("areas", v)} />
              <Row on={showGrid} color={gridMetric === "speed" ? "#22c55e" : "#22d3ee"} label={ko ? `메트릭 격자 (${gridM}m)` : `Metric grid (${gridM}m)`} onChange={setShowGrid} />
              {showGrid && (
                <div className="llp-gridctl" style={{ padding: "2px 0 6px 18px", display: "flex", flexDirection: "column", gap: 5 }}>
                  <div style={{ display: "flex", gap: 4 }}>
                    <button className={`mdl${gridMetric === "speed" ? " on" : ""}`} onClick={() => setGridMetric("speed")}>{ko ? "평균속도" : "Avg speed"}</button>
                    <button className={`mdl${gridMetric === "count" ? " on" : ""}`} onClick={() => setGridMetric("count")}>{ko ? "차량 수" : "Density"}</button>
                  </div>
                  <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 11, color: "var(--text-dim)" }}>
                    <input type="range" min={GRID_MIN} max={GRID_MAX} step={25} value={gridM} onChange={(e) => setGridM(Number(e.target.value))} style={{ flex: 1 }} />
                    <span className="mono" style={{ minWidth: 34, textAlign: "right" }}>{gridM}m</span>
                  </label>
                  <div style={{ fontSize: 10, color: "var(--text-mute)" }}>
                    {gridMetric === "speed" ? (ko ? "느림 🔴 → 빠름 🟢 (정체 구간)" : "slow 🔴 → fast 🟢") : (ko ? "적음 🔵 → 많음 🔴 (혼잡 구간)" : "few 🔵 → many 🔴")}
                  </div>
                </div>
              )}
            </section>
            <section className="llp-sec">
              <header>{ko ? "학습 (GPS)" : "Learned (GPS)"}</header>
              <Row on={toggles.learnTopos} color="#5eead4" label={ko ? "작업지점 좌표 (학습)" : "Work-points (learned)"} onChange={(v) => set("learnTopos", v)} />
              <Row on={showWharf} color="#38bdf8" label={ko ? "안벽 위치 (WHARF)" : "Wharf positions"} onChange={setShowWharf} />
              <Row on={showRoadGraph} color="#a78bfa" label={ko ? "추론 도로망 (GPS, 방향)" : "Inferred roads (GPS, directed)"} onChange={setShowRoadGraph} />
              {showRoadGraph && roadGraphStats && (
                <div className="llp-hint" style={{ paddingLeft: 18 }}>
                  {ko
                    ? `밀도: 노드 ${roadGraphStats.nodes} · 엣지 ${roadGraphStats.edges} · ${roadGraphStats.km}km · 작업지점 ${roadGraphStats.workpoints.toLocaleString()} · 갱신 ${roadGraphStats.generated_at}`
                    : `density: ${roadGraphStats.nodes} nodes · ${roadGraphStats.edges} edges · ${roadGraphStats.km}km · ${roadGraphStats.workpoints.toLocaleString()} work-pts · ${roadGraphStats.generated_at}`}
                </div>
              )}
              <div className="llp-hint">{ko ? "작업점: 채움=신뢰도(🟢높음·🟠보통·🔴낮음)·테두리=블록(청록)/크레인(주황) · 안벽: ARRIVED GPS로 학습한 선석 위치 · 도로망: GPS 궤적 추론(보라 선) + 방향 화살표(초록=일방·회색=양방)" : "work-points: fill=confidence (🟢🟠🔴), ring=block/crane · wharf: berth positions from ARRIVED GPS · roads: inferred centerlines (purple) + direction arrows (green=one-way, grey=two-way)"}</div>
            </section>
            <section className="llp-sec">
              <header>{ko ? "배차 (DISPATCH)" : "Dispatch"}</header>
              <Row on={showWorkPts} color="#34d399" label={ko ? "배차 작업지점 (클릭: TOS vs 우리)" : "Dispatched work points (click: TOS vs ours)"} onChange={setShowWorkPts} />
              <Row on={toggles.demand} color="#fb923c" label={ko ? "작업 수요 · 미배정 (DS/LD)" : "Demand · unassigned"} onChange={(v) => set("demand", v)} />
              <div className="llp-hint">{ko ? "작업지점: 클릭=TOS vs 우리 배차 비교 · 수요: 크기=대수(주황 DS·청록 LD)·클릭=상세" : "work points: click = TOS vs ours · demand: size = count (orange DS · teal LD), click for details"}</div>
            </section>
          </div>
        )}
      </aside>

      {/* bottom: state display filter (distinct teal accent) */}
      <div className="map-chips">
        {STATES.map((s) => (
          <button key={s.key} className={`mchip${stateFilter === s.key ? " active" : ""}`} onClick={() => setStateFilter(stateFilter === s.key ? null : s.key)}>
            <span className="sw" style={{ background: STATE_COLOR[s.key] }} />{ko ? s.ko : s.en}<span className="cn mono">{counts[s.key]}</span>
          </button>
        ))}
        {stateFilter && <button className="mchip clear" onClick={() => setStateFilter(null)}>{ko ? "필터 해제" : "Clear"}</button>}
      </div>

      {/* clicked-vehicle detail panel (bottom-right) */}
      {selDev && <LiveVehicleDetail v={selDev} lang={lang} onClose={closePanel} />}
    </div>
  );
}

// equipment icon shown on each tab (same spec as the map markers → built-in legend).
function EqGlyph({ eq }: { eq?: string }) {
  const prims = eq ? EQUIP_ICON[eq] : undefined;
  if (!prims) return null;
  return (
    <svg className="meq-glyph" viewBox="0 0 24 24" width="14" height="14" aria-hidden>
      {prims.map((p, i) => {
        const op = p.dark ? 0.5 : 1; // dark parts dimmer (so they read on a dark tab)
        if (p.k === "rect") return <rect key={i} x={p.x} y={p.y} width={p.w} height={p.h} rx={p.r ?? 0} fill="currentColor" fillOpacity={op} />;
        if (p.k === "circle") return <circle key={i} cx={p.cx} cy={p.cy} r={p.r} fill="currentColor" fillOpacity={op} />;
        return <polygon key={i} points={p.pts.map(([x, y]) => `${x},${y}`).join(" ")} fill="currentColor" fillOpacity={op} />;
      })}
    </svg>
  );
}

function Row({ on, color, label, onChange }: { on: boolean; color: string; label: string; onChange: (v: boolean) => void }) {
  return (
    <label className={`llp-row${on ? " on" : ""}`}>
      <input type="checkbox" checked={on} onChange={(e) => onChange(e.target.checked)} />
      <span className="llp-sw" style={{ background: color }} />
      <span className="llp-label">{label}</span>
    </label>
  );
}
