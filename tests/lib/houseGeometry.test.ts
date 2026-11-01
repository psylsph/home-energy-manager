import { describe, it, expect } from 'vitest';
import {
  EV_LEG,
  HOUSE,
  HOUSE_ANCHORS,
  HOUSE_VIEWBOX,
  ISO_ORIGIN,
  LEGS,
  PANEL_ARRAY,
  PLOT,
  frontRoofPoint,
  iso,
  isoPoint,
  offsetPolyline,
  panelCells,
  pathD,
  polygonPoints,
  polylineLength,
  routeBetween,
  type HousePart,
} from '../../src/lib/houseGeometry';

/** House view geometry (issue #422): projection, anchors, routes, lanes. */

describe('iso projection', () => {
  it('puts the world origin at the screen origin', () => {
    expect(iso(0, 0, 0)).toEqual(ISO_ORIGIN);
  });

  it('draws +x down-right, +y down-left and +z straight up', () => {
    const o = iso(0, 0, 0);
    const px = iso(1, 0, 0);
    const py = iso(0, 1, 0);
    const pz = iso(0, 0, 1);
    expect(px.x).toBeGreaterThan(o.x);
    expect(px.y).toBeGreaterThan(o.y);
    expect(py.x).toBeLessThan(o.x);
    expect(py.y).toBeGreaterThan(o.y);
    expect(pz.x).toBe(o.x);
    expect(pz.y).toBeLessThan(o.y);
    // 2:1 isometric: one world step moves twice as far across as down.
    expect((px.x - o.x) / (px.y - o.y)).toBeCloseTo(Math.sqrt(3), 1);
  });

  it('isoPoint matches iso', () => {
    expect(isoPoint({ x: 2, y: 3, z: 1 })).toEqual(iso(2, 3, 1));
  });

  it('formats polygon points as SVG pairs', () => {
    const pts = polygonPoints([{ x: 0, y: 0, z: 0 }, { x: 1, y: 0, z: 0 }]);
    expect(pts).toBe(`${ISO_ORIGIN.x},${ISO_ORIGIN.y} ${iso(1, 0).x},${iso(1, 0).y}`);
  });
});

describe('scene fits the viewBox', () => {
  const inside = (p: { x: number; y: number }) =>
    p.x >= 0 && p.x <= HOUSE_VIEWBOX.width && p.y >= 0 && p.y <= HOUSE_VIEWBOX.height;

  it('keeps every plot corner and the chimney inside the viewBox', () => {
    for (const [x, y] of [[PLOT.x0, PLOT.y0], [PLOT.x1, PLOT.y0], [PLOT.x1, PLOT.y1], [PLOT.x0, PLOT.y1]]) {
      expect(inside(iso(x, y, -0.45))).toBe(true);
    }
    expect(inside(iso(1, 3, 9.75))).toBe(true);
  });

  it('keeps every anchor and route point inside the viewBox', () => {
    for (const p of Object.values(HOUSE_ANCHORS)) expect(inside(isoPoint(p))).toBe(true);
    for (const leg of [...Object.values(LEGS), EV_LEG]) {
      for (const p of leg) expect(inside(isoPoint(p))).toBe(true);
    }
  });
});

describe('roof and panels', () => {
  it('runs the front slope from the eave up to the ridge', () => {
    expect(frontRoofPoint(2, 0)).toEqual({ x: 2, y: HOUSE.y1, z: HOUSE.wall });
    const ridge = frontRoofPoint(2, 1);
    expect(ridge.z).toBe(HOUSE.ridge);
    expect(ridge.y).toBe((HOUSE.y0 + HOUSE.y1) / 2);
  });

  it('lays out a 4×2 panel array on the slope', () => {
    const cells = panelCells();
    expect(cells).toHaveLength(PANEL_ARRAY.cols * PANEL_ARRAY.rows);
    for (const cell of cells) {
      expect(cell).toHaveLength(4);
      for (const p of cell) {
        expect(p.x).toBeGreaterThanOrEqual(PANEL_ARRAY.x0);
        expect(p.x).toBeLessThanOrEqual(PANEL_ARRAY.x1);
        expect(p.z).toBeGreaterThan(HOUSE.wall);
      }
    }
  });
});

describe('routes', () => {
  it('ends every leg at the inverter', () => {
    for (const leg of Object.values(LEGS)) {
      expect(leg[leg.length - 1]).toEqual(HOUSE_ANCHORS.inverter);
    }
  });

  it('starts the grid leg at the gate', () => {
    expect(LEGS.grid[0]).toEqual(HOUSE_ANCHORS.grid);
  });

  it('joins two legs through the inverter exactly once', () => {
    const parts: HousePart[] = ['solar', 'battery', 'home', 'grid'];
    for (const from of parts) {
      for (const to of parts) {
        if (from === to) continue;
        const route = routeBetween(from, to);
        expect(route[0]).toEqual(LEGS[from][0]);
        expect(route[route.length - 1]).toEqual(LEGS[to][0]);
        expect(route.filter((p) => p === HOUSE_ANCHORS.inverter)).toHaveLength(1);
        expect(route).toHaveLength(LEGS[from].length + LEGS[to].length - 1);
      }
    }
  });

  it('ends the EV leg at the foot of the charge post', () => {
    const end = EV_LEG[EV_LEG.length - 1];
    expect(end.x).toBe(HOUSE_ANCHORS.ev.x);
    expect(end.y).toBe(HOUSE_ANCHORS.ev.y);
    expect(end.z).toBe(0);
  });
});

describe('polyline helpers', () => {
  it('builds an SVG path', () => {
    expect(pathD([{ x: 0, y: 0 }, { x: 3, y: 4 }])).toBe('M 0 0 L 3 4');
    expect(pathD([])).toBe('');
  });

  it('measures length', () => {
    expect(polylineLength([{ x: 0, y: 0 }, { x: 3, y: 4 }, { x: 3, y: 10 }])).toBe(11);
    expect(polylineLength([{ x: 1, y: 1 }])).toBe(0);
  });

  it('returns a copy unchanged for a zero offset or a single point', () => {
    const line = [{ x: 0, y: 0 }, { x: 10, y: 0 }];
    const out = offsetPolyline(line, 0);
    expect(out).toEqual(line);
    expect(out[0]).not.toBe(line[0]);
    expect(offsetPolyline([{ x: 5, y: 5 }], 3)).toEqual([{ x: 5, y: 5 }]);
  });

  it('shifts a straight line sideways by the offset', () => {
    // Travelling +x, "left" in screen space (y down) is −y.
    expect(offsetPolyline([{ x: 0, y: 0 }, { x: 10, y: 0 }], 2)).toEqual([{ x: 0, y: -2 }, { x: 10, y: -2 }]);
    expect(offsetPolyline([{ x: 0, y: 0 }, { x: 10, y: 0 }], -2)).toEqual([{ x: 0, y: 2 }, { x: 10, y: 2 }]);
  });

  it('keeps lanes parallel round a right-angle corner', () => {
    const out = offsetPolyline([{ x: 0, y: 0 }, { x: 10, y: 0 }, { x: 10, y: 10 }], 2);
    // Corner miter: both legs stay exactly 2 px out.
    expect(out[1].x).toBeCloseTo(12, 5);
    expect(out[1].y).toBeCloseTo(-2, 5);
    expect(out[2]).toEqual({ x: 12, y: 10 });
  });

  it('caps the miter on a sharp corner', () => {
    const out = offsetPolyline([{ x: 0, y: 0 }, { x: 10, y: 0 }, { x: 0, y: 0.5 }], 2);
    const shift = Math.hypot(out[1].x - 10, out[1].y);
    expect(shift).toBeLessThanOrEqual(4.01);
  });

  it('survives a full reversal without NaN', () => {
    const out = offsetPolyline([{ x: 0, y: 0 }, { x: 10, y: 0 }, { x: 0, y: 0 }], 2);
    for (const p of out) {
      expect(Number.isFinite(p.x)).toBe(true);
      expect(Number.isFinite(p.y)).toBe(true);
    }
    expect(out[1]).toEqual({ x: 10, y: -2 });
  });

  it('treats a zero-length segment safely', () => {
    const out = offsetPolyline([{ x: 0, y: 0 }, { x: 0, y: 0 }], 2);
    for (const p of out) expect(Number.isFinite(p.x) && Number.isFinite(p.y)).toBe(true);
  });
});
