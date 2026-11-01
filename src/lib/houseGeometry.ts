/**
 * Geometry for the illustrated house view (issue #422).
 *
 * The scene is modelled in "world" units (roughly metres) and drawn with a
 * standard 2:1 isometric projection. World axes:
 *
 * - `x` runs along the front of the house, drawn down-and-right on screen;
 * - `y` runs from the back of the plot to the front, drawn down-and-left;
 * - `z` is height, drawn straight up.
 *
 * So the front wall (door, windows, roof slope with the panels) faces
 * down-left and the right-hand gable wall (inverter, battery) faces
 * down-right — the same composition as the mock-up.
 *
 * Pure: no React. The illustration, the flow routes and the leader lines all
 * read their coordinates from here, so a line always meets the thing it
 * points at.
 */

/** A point in world space. */
export interface WorldPoint {
  x: number;
  y: number;
  z: number;
}

/** A point in SVG user space. */
export interface ScreenPoint {
  x: number;
  y: number;
}

/** SVG viewBox. Sized so header + view + Summary tiles fit an iPhone 12 Pro without scrolling. */
export const HOUSE_VIEWBOX = { width: 390, height: 520 } as const;

/** Pixels per world unit. */
export const ISO_SCALE = 16;
/** Screen origin of world (0, 0, 0). */
export const ISO_ORIGIN: ScreenPoint = { x: 195, y: 222 };
const COS30 = Math.cos(Math.PI / 6);

/** Project a world point to SVG coordinates. */
export function iso(x: number, y: number, z = 0): ScreenPoint {
  return {
    x: round2(ISO_ORIGIN.x + (x - y) * COS30 * ISO_SCALE),
    y: round2(ISO_ORIGIN.y + (x + y) * 0.5 * ISO_SCALE - z * ISO_SCALE),
  };
}

/** Project a [`WorldPoint`]. */
export function isoPoint(p: WorldPoint): ScreenPoint {
  return iso(p.x, p.y, p.z);
}

/** SVG `points` attribute for a polygon of world points. */
export function polygonPoints(points: WorldPoint[]): string {
  return points.map((p) => {
    const s = isoPoint(p);
    return `${s.x},${s.y}`;
  }).join(' ');
}

function round2(n: number): number {
  return Math.round(n * 100) / 100;
}

const w = (x: number, y: number, z = 0): WorldPoint => ({ x, y, z });

// ---------------------------------------------------------------------------
// Scene dimensions (world units)
// ---------------------------------------------------------------------------

/** The plot the house sits on. */
export const PLOT = { x0: -3, x1: 11, y0: -2, y1: 10 } as const;
/** The main house body. */
export const HOUSE = { x0: 0, x1: 8, y0: 0, y1: 6, wall: 5, ridge: 7.5 } as const;
/** Ridge line runs along x at the middle of the house depth. */
export const RIDGE_Y = (HOUSE.y0 + HOUSE.y1) / 2;
/** The garden path from the front door to the gate. */
export const PATH = { x0: 2.6, x1: 3.8 } as const;
/** Battery cabinet on the gable wall (protrudes in +x). */
export const BATTERY_BOX = { y0: 1.0, y1: 2.6, z0: 0, z1: 3.8, depth: 0.6 } as const;
/** Inverter on the gable wall, nearer the front corner than the battery. */
export const INVERTER_BOX = { y0: 3.6, y1: 4.8, z0: 1.8, z1: 2.9, depth: 0.35 } as const;
/** EV charge post on the front-left lawn, clear of the hedge. */
export const EV_POST = { x: -1.8, y: 7.6, height: 1.6 } as const;
/** Front hedges either side of the path, along the front of the plot. */
export const HEDGES = [
  { x0: -2.6, x1: 2.2, y0: 9.3, y1: 9.9, z: 0.8 },
  { x0: 4.2, x1: 10.6, y0: 9.3, y1: 9.9, z: 0.8 },
] as const;
/** The grid cable runs this far in from the front of the plot, behind the hedge. */
const GRID_RUN_Y = 8.8;
/** Number of SOC bars drawn on the battery cabinet. */
export const BATTERY_BARS = 10;

/** World point on the front roof slope: `u` along the ridge (x), `v` 0 = eave → 1 = ridge. */
export function frontRoofPoint(x: number, v: number): WorldPoint {
  return w(x, HOUSE.y1 - v * (HOUSE.y1 - RIDGE_Y), HOUSE.wall + v * (HOUSE.ridge - HOUSE.wall));
}

/** The 4×2 panel array on the front slope. */
export const PANEL_ARRAY = { x0: 3.0, x1: 7.4, v0: 0.14, v1: 0.9, cols: 4, rows: 2 } as const;

/** World polygons for each cell of the panel array (row-major, eave row first). */
export function panelCells(): WorldPoint[][] {
  const { x0, x1, v0, v1, cols, rows } = PANEL_ARRAY;
  const gap = 0.08;
  const cw = (x1 - x0) / cols;
  const rh = (v1 - v0) / rows;
  const cells: WorldPoint[][] = [];
  for (let r = 0; r < rows; r++) {
    for (let c = 0; c < cols; c++) {
      const cx0 = x0 + c * cw + gap / 2;
      const cx1 = x0 + (c + 1) * cw - gap / 2;
      const cv0 = v0 + r * rh + gap / 6;
      const cv1 = v0 + (r + 1) * rh - gap / 6;
      cells.push([frontRoofPoint(cx0, cv0), frontRoofPoint(cx1, cv0), frontRoofPoint(cx1, cv1), frontRoofPoint(cx0, cv1)]);
    }
  }
  return cells;
}

// ---------------------------------------------------------------------------
// Anchors — where lines meet the house
// ---------------------------------------------------------------------------

const GABLE_X = HOUSE.x1;
const INV_MID_Y = (INVERTER_BOX.y0 + INVERTER_BOX.y1) / 2;
const INV_MID_Z = (INVERTER_BOX.z0 + INVERTER_BOX.z1) / 2;
const GATE_X = (PATH.x0 + PATH.x1) / 2;

/** Named points in world space. */
export const HOUSE_ANCHORS = {
  /** Centre of the panel array — the Solar leader line ends here. */
  solar: frontRoofPoint((PANEL_ARRAY.x0 + PANEL_ARRAY.x1) / 2, (PANEL_ARRAY.v0 + PANEL_ARRAY.v1) / 2),
  /** Gable apex — the Home leader line ends here. */
  home: w(GABLE_X, RIDGE_Y, HOUSE.ridge),
  /** Foot of the battery cabinet — the Battery leader line ends here. */
  battery: w(GABLE_X + BATTERY_BOX.depth, (BATTERY_BOX.y0 + BATTERY_BOX.y1) / 2, 0),
  /** Front gate where the grid line comes in — the Grid leader line ends here. */
  grid: w(GATE_X, PLOT.y1, 0),
  /** EV charge post at the front-left corner of the plot. */
  ev: w(EV_POST.x, EV_POST.y, EV_POST.height),
  /** Face of the inverter, where every route meets. */
  inverter: w(GABLE_X + INVERTER_BOX.depth, INV_MID_Y, INV_MID_Z),
} as const satisfies Record<string, WorldPoint>;

// ---------------------------------------------------------------------------
// Route legs — each runs from a part to the inverter (or, for the EV, from the
// house to the charge post). A flow's route is `leg(from)` reversed into
// `leg(to)`, so lines bend around the building rather than cutting through it.
// ---------------------------------------------------------------------------

/** Parts with a leg to the inverter. */
export type HousePart = 'solar' | 'battery' | 'home' | 'grid';

const SIDE = GABLE_X + 0.05;
/** Height the wiring runs at along the gable wall. */
const WIRE_Z = INV_MID_Z;

/** Each leg runs from the part *to* the inverter. */
export const LEGS: Record<HousePart, WorldPoint[]> = {
  // Panel array → down the front-right roof edge → down the corner → along the wall.
  solar: [
    frontRoofPoint(PANEL_ARRAY.x1, PANEL_ARRAY.v0),
    w(SIDE, HOUSE.y1, HOUSE.wall),
    w(SIDE, HOUSE.y1, WIRE_Z),
    HOUSE_ANCHORS.inverter,
  ],
  // Battery cabinet side → along the wall → inverter.
  battery: [
    w(SIDE + BATTERY_BOX.depth, BATTERY_BOX.y1, WIRE_Z),
    HOUSE_ANCHORS.inverter,
  ],
  // Into the house high on the wall → down to the inverter.
  home: [
    w(SIDE, INV_MID_Y, HOUSE.wall - 0.6),
    w(SIDE, INV_MID_Y, INVERTER_BOX.z1),
    HOUSE_ANCHORS.inverter,
  ],
  // Gate → along the front of the plot → up the right-hand side → up the wall.
  grid: [
    HOUSE_ANCHORS.grid,
    w(GATE_X, GRID_RUN_Y, 0),
    w(PLOT.x1 - 1.2, GRID_RUN_Y, 0),
    w(PLOT.x1 - 1.2, INV_MID_Y, 0),
    w(SIDE + INVERTER_BOX.depth, INV_MID_Y, 0),
    w(SIDE + INVERTER_BOX.depth, INV_MID_Y, INVERTER_BOX.z0),
    HOUSE_ANCHORS.inverter,
  ],
};

/** House → EV post: out of the front-left corner and across the lawn. */
export const EV_LEG: WorldPoint[] = [
  w(HOUSE.x0 + 0.3, HOUSE.y1 + 0.05, 0.6),
  w(HOUSE.x0 + 0.3, HOUSE.y1 + 0.8, 0),
  w(EV_POST.x, HOUSE.y1 + 0.8, 0),
  w(EV_POST.x, EV_POST.y, 0),
];

/** World route between two parts, via the inverter. */
export function routeBetween(from: HousePart, to: HousePart): WorldPoint[] {
  const out = LEGS[from];
  const back = [...LEGS[to]].reverse();
  // Both legs end at the inverter; drop the duplicate.
  return [...out, ...back.slice(1)];
}

// ---------------------------------------------------------------------------
// Polyline helpers (screen space)
// ---------------------------------------------------------------------------

/** SVG path `d` for a screen polyline. */
export function pathD(points: ScreenPoint[]): string {
  return points.map((p, i) => `${i === 0 ? 'M' : 'L'} ${p.x} ${p.y}`).join(' ');
}

/** Total length of a screen polyline. */
export function polylineLength(points: ScreenPoint[]): number {
  let len = 0;
  for (let i = 1; i < points.length; i++) {
    len += Math.hypot(points[i].x - points[i - 1].x, points[i].y - points[i - 1].y);
  }
  return len;
}

/**
 * Offset a screen polyline sideways by `d` pixels (positive = left of the
 * direction of travel). Used to give each concurrent flow its own lane where
 * routes share wall wiring, so every flow's colour stays visible. Interior
 * vertices use the averaged normal, scaled so lanes stay parallel.
 */
export function offsetPolyline(points: ScreenPoint[], d: number): ScreenPoint[] {
  if (d === 0 || points.length < 2) return points.map((p) => ({ ...p }));
  const normals: ScreenPoint[] = [];
  for (let i = 1; i < points.length; i++) {
    const dx = points[i].x - points[i - 1].x;
    const dy = points[i].y - points[i - 1].y;
    const len = Math.hypot(dx, dy) || 1;
    normals.push({ x: dy / len, y: -dx / len });
  }
  return points.map((p, i) => {
    let n: ScreenPoint;
    if (i === 0) n = normals[0];
    else if (i === points.length - 1) n = normals[normals.length - 1];
    else {
      const a = normals[i - 1];
      const b = normals[i];
      const sum = { x: a.x + b.x, y: a.y + b.y };
      const sumLen = Math.hypot(sum.x, sum.y);
      if (sumLen < 1e-6) {
        n = a; // 180° turn: no sensible miter, keep the incoming normal.
      } else {
        const m = { x: sum.x / sumLen, y: sum.y / sumLen };
        // Miter length 1/cos(θ/2), capped so sharp corners don't spike.
        const cos = Math.max(0.5, m.x * a.x + m.y * a.y);
        n = { x: m.x / cos, y: m.y / cos };
      }
    }
    return { x: round2(p.x + n.x * d), y: round2(p.y + n.y * d) };
  });
}
