/**
 * Static isometric illustration for the house view (issue #422): a British
 * two-storey brick house on its own plot — slate roof with grey fascias,
 * chimney with two pots, grey-framed casement windows, a green front door under a
 * canopy, a path, lawn and hedges, and the inverter and consumer unit on the
 * gable wall.
 *
 * It takes no props and is memoised, so it renders once; everything that
 * changes with each poll (panel glow, battery gauge, flows, readings) is drawn
 * on top by `HouseEnergyView`. All colours come from the `--app-house-*`
 * theme variables in `index.css`, so the light theme needs no separate
 * artwork. No SVG filters: the Raspberry Pi renders in software (#298).
 */
import { memo } from 'react';
import { FLOW_COLORS } from '../../lib/energyFlow';
import {
  CHIMNEY,
  CONSUMER_UNIT,
  HEDGES,
  HOUSE,
  INVERTER_BOX,
  PATH,
  PLOT,
  RIDGE_Y,
  frontRoofPoint,
  iso,
  panelCells,
  polygonPoints,
  type WorldPoint,
} from '../../lib/houseGeometry';

const v = (name: string) => `var(--app-house-${name})`;
const w = (x: number, y: number, z = 0): WorldPoint => ({ x, y, z });

/** Roof overhang past the walls. */
const OVERHANG = 0.3;
/** Eave depth past the front wall, measured along y. */
const EAVE = 0.4;
const SLOPE = (HOUSE.ridge - HOUSE.wall) / (HOUSE.y1 - RIDGE_Y);
const EAVE_Z = HOUSE.wall - EAVE * SLOPE;
/** Roof-slope parameter of the eave (negative: past the wall line). */
const EAVE_V = -EAVE / (HOUSE.y1 - RIDGE_Y);

function Poly({ points, fill, stroke, strokeWidth }: { points: WorldPoint[]; fill: string; stroke?: string; strokeWidth?: number }) {
  return <polygon points={polygonPoints(points)} fill={fill} stroke={stroke} strokeWidth={strokeWidth} strokeLinejoin="round" />;
}

function Line({ a, b, stroke, width = 1, cap = 'round' }: { a: WorldPoint; b: WorldPoint; stroke: string; width?: number; cap?: 'round' | 'butt' }) {
  const p = iso(a.x, a.y, a.z);
  const q = iso(b.x, b.y, b.z);
  return <line x1={p.x} y1={p.y} x2={q.x} y2={q.y} stroke={stroke} strokeWidth={width} strokeLinecap={cap} />;
}

/** An axis-aligned box showing its top, front (+y) and right (+x) faces. */
function Box({ x0, x1, y0, y1, z0, z1, top, front, side, stroke, strokeWidth }: {
  x0: number; x1: number; y0: number; y1: number; z0: number; z1: number;
  top: string; front: string; side: string; stroke?: string; strokeWidth?: number;
}) {
  return (
    <g>
      <Poly points={[w(x0, y1, z0), w(x1, y1, z0), w(x1, y1, z1), w(x0, y1, z1)]} fill={front} stroke={stroke} strokeWidth={strokeWidth} />
      <Poly points={[w(x1, y1, z0), w(x1, y0, z0), w(x1, y0, z1), w(x1, y1, z1)]} fill={side} stroke={stroke} strokeWidth={strokeWidth} />
      <Poly points={[w(x0, y0, z1), w(x1, y0, z1), w(x1, y1, z1), w(x0, y1, z1)]} fill={top} stroke={stroke} strokeWidth={strokeWidth} />
    </g>
  );
}

/** A grey-framed casement window on the front wall (y = HOUSE.y1), spanning x0..x1, z0..z1. */
function FrontWindow({ x0, x1, z0, z1 }: { x0: number; x1: number; z0: number; z1: number }) {
  const y = HOUSE.y1 + 0.01;
  const i = 0.12;
  const mx = (x0 + x1) / 2;
  const tz = z1 - (z1 - z0) * 0.33;
  return (
    <g>
      <Poly points={[w(x0, y, z0), w(x1, y, z0), w(x1, y, z1), w(x0, y, z1)]} fill={v('frame')} />
      <Poly points={[w(x0 + i, y, z0 + i), w(x1 - i, y, z0 + i), w(x1 - i, y, z1 - i), w(x0 + i, y, z1 - i)]} fill={v('glass')} />
      <Line a={w(mx, y, z0 + i)} b={w(mx, y, z1 - i)} stroke={v('frame')} width={1.6} cap="butt" />
      <Line a={w(x0 + i, y, tz)} b={w(x1 - i, y, tz)} stroke={v('frame')} width={1.4} cap="butt" />
      <Line a={w(x0 - 0.12, y + 0.08, z0 - 0.06)} b={w(x1 + 0.12, y + 0.08, z0 - 0.06)} stroke={v('frame')} width={2.2} />
    </g>
  );
}

/** A grey-framed casement window on the gable wall (x = HOUSE.x1), spanning y0..y1, z0..z1. */
function SideWindow({ y0, y1, z0, z1 }: { y0: number; y1: number; z0: number; z1: number }) {
  const x = HOUSE.x1 + 0.01;
  const i = 0.12;
  const my = (y0 + y1) / 2;
  const tz = z1 - (z1 - z0) * 0.33;
  return (
    <g>
      <Poly points={[w(x, y1, z0), w(x, y0, z0), w(x, y0, z1), w(x, y1, z1)]} fill={v('frame')} />
      <Poly points={[w(x, y1 - i, z0 + i), w(x, y0 + i, z0 + i), w(x, y0 + i, z1 - i), w(x, y1 - i, z1 - i)]} fill={v('glass')} />
      <Line a={w(x, my, z0 + i)} b={w(x, my, z1 - i)} stroke={v('frame')} width={1.6} cap="butt" />
      <Line a={w(x, y0 + i, tz)} b={w(x, y1 - i, tz)} stroke={v('frame')} width={1.4} cap="butt" />
      <Line a={w(x + 0.08, y1 + 0.12, z0 - 0.06)} b={w(x + 0.08, y0 - 0.12, z0 - 0.06)} stroke={v('frame')} width={2.2} />
    </g>
  );
}

const MORTAR_STEP = 0.36;

function FrontMortar() {
  const lines = [];
  for (let z = MORTAR_STEP; z < HOUSE.wall; z += MORTAR_STEP) {
    lines.push(<Line key={z} a={w(HOUSE.x0, HOUSE.y1, z)} b={w(HOUSE.x1, HOUSE.y1, z)} stroke={v('mortar')} width={0.7} cap="butt" />);
  }
  return <g data-testid="house-mortar">{lines}</g>;
}

function GableMortar() {
  const lines = [];
  const half = (HOUSE.y1 - HOUSE.y0) / 2;
  for (let z = MORTAR_STEP; z < HOUSE.ridge - 0.2; z += MORTAR_STEP) {
    // Above the eaves the gable narrows to its apex.
    const hw = z <= HOUSE.wall ? half : half * (HOUSE.ridge - z) / (HOUSE.ridge - HOUSE.wall);
    lines.push(<Line key={z} a={w(HOUSE.x1, RIDGE_Y + hw, z)} b={w(HOUSE.x1, RIDGE_Y - hw, z)} stroke={v('mortar')} width={0.7} cap="butt" />);
  }
  return <g>{lines}</g>;
}

function HouseIllustrationInner() {
  const { x0, x1, y0, y1, wall, ridge } = HOUSE;
  const roofX0 = x0 - OVERHANG;
  const roofX1 = x1 + OVERHANG;
  const backEaveY = y0 - EAVE;

  const tileLines = [];
  for (let i = 1; i < 7; i++) {
    const t = EAVE_V + ((1 - EAVE_V) * i) / 7;
    tileLines.push(<Line key={i} a={frontRoofPoint(roofX0, t)} b={frontRoofPoint(roofX1, t)} stroke={v('roof-line')} width={0.9} cap="butt" />);
  }

  return (
    <g data-testid="house-illustration">
      {/* Plot slab: front and right edges, then the top. */}
      <Poly points={[w(PLOT.x0, PLOT.y1, 0), w(PLOT.x1, PLOT.y1, 0), w(PLOT.x1, PLOT.y1, -0.45), w(PLOT.x0, PLOT.y1, -0.45)]} fill={v('plot-side')} />
      <Poly points={[w(PLOT.x1, PLOT.y1, 0), w(PLOT.x1, PLOT.y0, 0), w(PLOT.x1, PLOT.y0, -0.45), w(PLOT.x1, PLOT.y1, -0.45)]} fill={v('plot-side')} />
      <Poly points={[w(PLOT.x0, PLOT.y0), w(PLOT.x1, PLOT.y0), w(PLOT.x1, PLOT.y1), w(PLOT.x0, PLOT.y1)]} fill={v('plot-top')} stroke={v('plot-edge')} strokeWidth={1} />

      {/* Front lawn and the path to the door. */}
      <Poly points={[w(PLOT.x0 + 0.3, y1 + 0.6), w(PLOT.x1 - 0.3, y1 + 0.6), w(PLOT.x1 - 0.3, PLOT.y1 - 0.3), w(PLOT.x0 + 0.3, PLOT.y1 - 0.3)]} fill={v('lawn')} />
      <Poly points={[w(PATH.x0, y1), w(PATH.x1, y1), w(PATH.x1, PLOT.y1), w(PATH.x0, PLOT.y1)]} fill={v('path')} />

      {/* Soft footprint shadow. */}
      <Poly points={[w(x0 - 0.3, y0 - 0.3), w(x1 + 0.9, y0 - 0.3), w(x1 + 0.9, y1 + 0.4), w(x0 - 0.3, y1 + 0.4)]} fill={v('shadow')} />

      {/* Back roof slope (seen beyond the ridge). */}
      <Poly points={[w(roofX0, RIDGE_Y, ridge), w(roofX1, RIDGE_Y, ridge), w(roofX1, backEaveY, EAVE_Z), w(roofX0, backEaveY, EAVE_Z)]} fill={v('roof')} stroke={v('roof-line')} strokeWidth={0.8} />

      {/* Gable wall with the attic and upstairs windows; downstairs is the utility wall (inverter, consumer unit, cables). */}
      <Poly points={[w(x1, y1, 0), w(x1, y0, 0), w(x1, y0, wall), w(x1, RIDGE_Y, ridge), w(x1, y1, wall)]} fill={v('brick-side')} />
      <GableMortar />
      <SideWindow y0={2.5} y1={3.5} z0={5.3} z1={6.3} />
      <SideWindow y0={3.8} y1={5.1} z0={3.2} z1={4.4} />

      {/* Front wall: windows, door and canopy. */}
      <Poly points={[w(x0, y1, 0), w(x1, y1, 0), w(x1, y1, wall), w(x0, y1, wall)]} fill={v('brick-front')} />
      <FrontMortar />
      <FrontWindow x0={0.6} x1={2.0} z0={0.8} z1={2.2} />
      <FrontWindow x0={4.4} x1={5.8} z0={0.8} z1={2.2} />
      <FrontWindow x0={6.3} x1={7.5} z0={0.8} z1={2.2} />
      <FrontWindow x0={0.6} x1={2.0} z0={3.0} z1={4.3} />
      <FrontWindow x0={2.6} x1={3.8} z0={3.0} z1={4.3} />
      <FrontWindow x0={4.4} x1={5.8} z0={3.0} z1={4.3} />
      <FrontWindow x0={6.3} x1={7.5} z0={3.0} z1={4.3} />
      <g data-testid="house-door">
        <Poly points={[w(2.75, y1 + 0.01, 0), w(3.65, y1 + 0.01, 0), w(3.65, y1 + 0.01, 2.25), w(2.75, y1 + 0.01, 2.25)]} fill={v('door')} stroke={v('frame')} strokeWidth={1.2} />
        <circle cx={iso(3.5, y1 + 0.02, 1.1).x} cy={iso(3.5, y1 + 0.02, 1.1).y} r={1.4} fill={v('pot')} />
      </g>
      <Box x0={2.45} x1={3.95} y0={y1} y1={y1 + 0.6} z0={2.42} z1={2.58} top={v('canopy')} front={v('fascia')} side={v('fascia')} />

      {/* Front roof slope with tile courses, then the grey fascias. */}
      <Poly points={[frontRoofPoint(roofX0, EAVE_V), frontRoofPoint(roofX1, EAVE_V), w(roofX1, RIDGE_Y, ridge), w(roofX0, RIDGE_Y, ridge)]} fill={v('roof')} />
      {tileLines}
      <Line a={w(roofX0, RIDGE_Y, ridge)} b={w(roofX1, RIDGE_Y, ridge)} stroke={v('roof-line')} width={2.4} />
      <Line a={frontRoofPoint(roofX0, EAVE_V)} b={frontRoofPoint(roofX1, EAVE_V)} stroke={v('fascia')} width={2.4} />
      <Line a={frontRoofPoint(roofX1, EAVE_V)} b={w(roofX1, RIDGE_Y, ridge)} stroke={v('fascia')} width={2.4} />
      <Line a={w(roofX1, RIDGE_Y, ridge)} b={w(roofX1, backEaveY, EAVE_Z)} stroke={v('fascia')} width={2.4} />
      <Line a={frontRoofPoint(roofX0, EAVE_V)} b={w(roofX0, RIDGE_Y, ridge)} stroke={v('fascia')} width={2} />

      {/* Panel array base — the glow is drawn on top while generating. */}
      <g data-testid="house-panels">
        {panelCells().map((cell, i) => (
          <Poly key={i} points={cell} fill={v('panel')} />
        ))}
      </g>

      {/* Chimney stack with two terracotta pots. */}
      <Box x0={CHIMNEY.x0 + 0.05} x1={CHIMNEY.x1 - 0.05} y0={CHIMNEY.y0 + 0.05} y1={CHIMNEY.y1 - 0.05} z0={CHIMNEY.z0} z1={9} top={v('roof-line')} front={v('brick-front')} side={v('brick-side')} />
      <Box x0={CHIMNEY.x0} x1={CHIMNEY.x1} y0={CHIMNEY.y0} y1={CHIMNEY.y1} z0={9} z1={9.2} top={v('roof-line')} front={v('canopy')} side={v('canopy')} />
      <Box x0={0.95} x1={1.2} y0={2.85} y1={3.1} z0={9.2} z1={9.75} top={v('pot')} front={v('pot')} side={v('pot')} />
      <Box x0={1.4} x1={1.65} y0={2.85} y1={3.1} z0={9.2} z1={9.75} top={v('pot')} front={v('pot')} side={v('pot')} />

      {/* Inverter on the gable wall. */}
      <g data-testid="house-inverter">
        <Box
          x0={x1} x1={x1 + INVERTER_BOX.depth}
          y0={INVERTER_BOX.y0} y1={INVERTER_BOX.y1}
          z0={INVERTER_BOX.z0} z1={INVERTER_BOX.z1}
          top={v('device')} front={v('device')} side={v('device')}
          stroke={FLOW_COLORS.inverter} strokeWidth={1.2}
        />
        <Line
          a={w(x1 + INVERTER_BOX.depth + 0.01, INVERTER_BOX.y1 - 0.3, INVERTER_BOX.z1 - 0.3)}
          b={w(x1 + INVERTER_BOX.depth + 0.01, INVERTER_BOX.y0 + 0.3, INVERTER_BOX.z1 - 0.3)}
          stroke={FLOW_COLORS.inverter}
          width={1.6}
        />
      </g>

      {/* Consumer unit (fuse box): where the home cable goes into the house. */}
      <g data-testid="house-consumer-unit">
        <Box
          x0={x1} x1={x1 + CONSUMER_UNIT.depth}
          y0={CONSUMER_UNIT.y0} y1={CONSUMER_UNIT.y1}
          z0={CONSUMER_UNIT.z0} z1={CONSUMER_UNIT.z1}
          top={v('device')} front={v('device')} side={v('device')}
          stroke={v('frame')} strokeWidth={0.8}
        />
      </g>

      {/* Front hedges either side of the path. */}
      {HEDGES.map((h, i) => (
        <Box key={i} x0={h.x0} x1={h.x1} y0={h.y0} y1={h.y1} z0={0} z1={h.z} top={v('hedge-top')} front={v('hedge-side')} side={v('hedge-side')} />
      ))}
    </g>
  );
}

const HouseIllustration = memo(HouseIllustrationInner);
export default HouseIllustration;
