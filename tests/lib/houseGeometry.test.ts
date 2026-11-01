import { describe, it, expect } from 'vitest';
import {
  CONSUMER_UNIT,
  EV_LEG,
  HOUSE,
  HOUSE_ANCHORS,
  HOUSE_VIEWBOX,
  INVERTER_BOX,
  ISO_ORIGIN,
  LEGS,
  PANEL_ARRAY,
  PLOT,
  frontRoofPoint,
  iso,
  isoPoint,
  panelCells,
  pathD,
  polygonPoints,
  polylineLength,
  type HousePart,
} from '../../src/lib/houseGeometry';

/** House view geometry (issue #422): projection, anchors, cables. */

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

describe('cables', () => {
  const parts: HousePart[] = ['solar', 'battery', 'home', 'grid'];
  const onInverter = (p: { x: number; y: number; z: number }) =>
    p.x >= HOUSE.x1 && p.x <= HOUSE.x1 + 0.05 + INVERTER_BOX.depth
    && p.y >= INVERTER_BOX.y0 && p.y <= INVERTER_BOX.y1
    && p.z >= INVERTER_BOX.z0 && p.z <= INVERTER_BOX.z1;

  it('ends every cable on the inverter', () => {
    for (const part of parts) expect(onInverter(LEGS[part][LEGS[part].length - 1])).toBe(true);
  });

  it('meets the inverter at a different point for each part', () => {
    const ends = parts.map((part) => JSON.stringify(LEGS[part][LEGS[part].length - 1]));
    expect(new Set(ends).size).toBe(parts.length);
  });

  it('never runs a cable out of the top of the inverter', () => {
    for (const part of parts) {
      const leg = LEGS[part];
      const end = leg[leg.length - 1];
      const before = leg[leg.length - 2];
      expect(end.z).toBeLessThan(INVERTER_BOX.z1);
      // The last stretch doesn't come straight down onto the inverter.
      expect(before.x === end.x && before.y === end.y && before.z > end.z).toBe(false);
    }
  });

  it('enters the house at the consumer unit', () => {
    const start = LEGS.home[0];
    expect(start.y).toBeGreaterThanOrEqual(CONSUMER_UNIT.y0);
    expect(start.y).toBeLessThanOrEqual(CONSUMER_UNIT.y1);
    expect(start.z).toBe(CONSUMER_UNIT.z1);
  });

  it('starts the solar cable at the panel array', () => {
    expect(LEGS.solar[0]).toEqual(frontRoofPoint(PANEL_ARRAY.x1, PANEL_ARRAY.v0));
  });

  it('starts the grid leg at the gate', () => {
    expect(LEGS.grid[0]).toEqual(HOUSE_ANCHORS.grid);
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
});
