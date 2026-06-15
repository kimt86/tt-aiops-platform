// LIVE MAP — ESRI satellite (MapLibre GL) + TOS layout layers (areas/nodes/links,
// individually toggleable like wp-tt-data-center) + live equipment markers.
// Data: REPLAY of captured WP-TT GPS (web/public/livemap-replay.json) on a real-time
// loop; TOS layout from web/public/livemap-{layout,nodes,links}.json (extracted from
// wp-tt-data-center reference, mm/m → lat/lon via the fitted projection).
import maplibregl from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
import { useEffect, useMemo, useRef, useState } from "react";
import { type Lang } from "./i18n";
import { api } from "./api";
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
type Dispatch = "idle" | "staging" | "empty_travel" | "delivering" | "soon_idle" | "wait_rtg";
type LiveDev = { id: string; cls: string; lat: number; lon: number; speed: number; engine: number; age_s: number; dispatch?: Dispatch; jobtype?: string; topos1?: string };
type LiveSnap = { source: string; connected: boolean; count: number; as_of: string | null; dispatch_counts?: Record<string, number>; devices: LiveDev[] };

// dispatch-state highlight on the map (TT pool building)
// dispatch pools — filter the map by vehicle-pool type (TT only)
const DISPATCH_POOLS: { key: Dispatch; ko: string; en: string; color: string }[] = [
  { key: "idle", ko: "유휴", en: "Idle", color: "#22c55e" },
  { key: "staging", ko: "배차·대기", en: "Staging", color: "#0ea5e9" },
  { key: "soon_idle", ko: "곧 유휴", en: "Soon", color: "#f59e0b" },
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

// heading-oriented line segments for the learned driving-lane field (geometry, no text glyphs).
function laneSegments(
  grid: { lat: number; lon: number; passes: number; heading_deg: number | null; directionality: number | null }[],
): GeoJSON.Feature[] {
  const L = 11; // meters, half-segment (~half a 22m lane cell)
  return grid.map((c) => {
    const th = ((c.heading_deg ?? 0) * Math.PI) / 180; // 0 = North
    const dLat = (L / 111320) * Math.cos(th);
    const dLon = (L / (111320 * Math.cos((c.lat * Math.PI) / 180))) * Math.sin(th);
    return {
      type: "Feature",
      geometry: { type: "LineString", coordinates: [[c.lon - dLon, c.lat - dLat], [c.lon + dLon, c.lat + dLat]] },
      properties: { dir: c.directionality ?? 0, passes: c.passes },
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
  const [liveInfo, setLiveInfo] = useState<{ connected: boolean; count: number; asOf: string | null }>({ connected: false, count: 0, asOf: null });
  const [dispatchCounts, setDispatchCounts] = useState<Record<string, number>>({});

  // clicked-vehicle detail panel
  const [selDev, setSelDev] = useState<SelVeh | null>(null);
  const selRef = useRef<string | null>(null);
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

      // ── vehicles (top) ──
      // ── learned-layout (② work-points · ③ lanes) + dispatch demand overlays ──
      map.addSource("learn-topos", { type: "geojson", data: EMPTY_FC });
      map.addLayer({
        id: "lt-pt", type: "circle", source: "learn-topos", layout: { visibility: "none" },
        paint: {
          "circle-radius": ["interpolate", ["linear"], ["zoom"], 12, ["case", [">=", ["get", "n"], 30], 3, 1.6], 17, ["case", [">=", ["get", "n"], 30], 7, 4]],
          "circle-color": ["case", ["get", "crane"], "#f59e0b", "#5eead4"],
          "circle-opacity": ["case", [">=", ["get", "n"], 30], 0.85, 0.4],
          "circle-stroke-width": ["case", [">=", ["get", "n"], 30], 1, 0], "circle-stroke-color": "#0a0f1d",
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
    if (map.getLayer("ll-seg")) map.setLayoutProperty("ll-seg", "visibility", vis(toggles.learnLanes));
    if (map.getLayer("dm-bub")) map.setLayoutProperty("dm-bub", "visibility", vis(toggles.demand));
  }, [toggles, ready]);

  // learned-layout (②③) + dispatch demand overlays: fetch + build GeoJSON while toggled on.
  useEffect(() => {
    const map = mapRef.current;
    if (!map || !ready) return;
    const wantTopos = toggles.learnTopos || toggles.demand; // demand reuses topos coords
    if (!wantTopos && !toggles.learnLanes) return;
    let alive = true;
    const load = async () => {
      try {
        if (wantTopos) {
          const t = await api.learnTopos();
          if (!alive) return;
          if (toggles.learnTopos) {
            const feats = t.points.map((p) => ({ type: "Feature", geometry: { type: "Point", coordinates: [p.lon, p.lat] }, properties: { crane: p.is_crane, n: p.n, topos: p.topos } } as GeoJSON.Feature));
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
        if (toggles.learnLanes) {
          const l = await api.learnLanes();
          if (!alive) return;
          (map.getSource("learn-lanes") as maplibregl.GeoJSONSource | undefined)?.setData({ type: "FeatureCollection", features: laneSegments(l.grid) });
        }
      } catch { /* keep last good data */ }
    };
    load();
    const iv = setInterval(load, 15000);
    return () => { alive = false; clearInterval(iv); };
  }, [toggles.learnTopos, toggles.learnLanes, toggles.demand, ready]);

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
        let moving = 0, idle = 0, off = 0;
        const live = liveRef.current;
        const liveOn = useLiveRef.current && live != null && live.devices.length > 0;
        if (liveOn) {
          // live GPS: place each device at its latest fix (no interpolation).
          for (const d of live!.devices) {
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
  const setGroup = (keys: LayerKey[], v: boolean) => setToggles((t) => { const n = { ...t }; for (const k of keys) n[k] = v; return n; });
  const pointKeys = Object.values(NODE_LAYERS).map((n) => n.key);
  const linkKeys = Object.values(LINK_LAYERS).map((l) => l.key);
  const allPoints = pointKeys.every((k) => toggles[k]);
  const allLinks = linkKeys.every((k) => toggles[k]);
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
        <span className="map-count mono">{counts.total} / {liveActive ? liveInfo.count : ndev}</span>
        {liveActive ? (
          <span className="map-clock mono" title={liveInfo.asOf ?? ""}>⟳ {asOfAge != null ? `${asOfAge}s` : "—"}</span>
        ) : (
          <span className="map-clock mono">▶ t+{tpos}s / {win}s</span>
        )}
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
            </section>
            <section className="llp-sec">
              <header>{ko ? "포인트 (노드)" : "Points (nodes)"}</header>
              {Object.values(NODE_LAYERS).map((n) => (
                <Row key={n.key} on={toggles[n.key]} color={n.color} label={ko ? n.ko : n.en} onChange={(v) => set(n.key, v)} />
              ))}
              <button className="llp-meta" onClick={() => setGroup(pointKeys, !allPoints)}>{allPoints ? (ko ? "모든 포인트 OFF" : "All OFF") : (ko ? "모든 포인트 ON" : "All ON")}</button>
            </section>
            <section className="llp-sec">
              <header>{ko ? "링크 (arc)" : "Links (arcs)"}</header>
              {Object.values(LINK_LAYERS).map((l) => (
                <Row key={l.key} on={toggles[l.key]} color={l.color} label={ko ? l.ko : l.en} onChange={(v) => set(l.key, v)} />
              ))}
              <button className="llp-meta" onClick={() => setGroup(linkKeys, !allLinks)}>{allLinks ? (ko ? "모든 링크 OFF" : "All OFF") : (ko ? "모든 링크 ON" : "All ON")}</button>
            </section>
            <section className="llp-sec">
              <header>{ko ? "학습·배차 (GPS·실시간)" : "Learned · Dispatch"}</header>
              <Row on={toggles.learnTopos} color="#5eead4" label={ko ? "② 작업지점 좌표 (학습)" : "② Work-points (learned)"} onChange={(v) => set("learnTopos", v)} />
              <Row on={toggles.learnLanes} color="#34d399" label={ko ? "③ 주행 차선 (학습)" : "③ Driving lanes (learned)"} onChange={(v) => set("learnLanes", v)} />
              <Row on={toggles.demand} color="#fb923c" label={ko ? "작업 수요 · 미배정 (DS/LD)" : "Demand · unassigned"} onChange={(v) => set("demand", v)} />
              <div className="llp-hint">{ko ? "②원: 채움=확신(n≥30)·청록=블록·주황=크레인 · ③선: 초록=일방·주황=양방 · 수요원: 크기=대수" : "② circles (filled=confident) · ③ lines (green=one-way) · demand bubbles sized by count"}</div>
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
