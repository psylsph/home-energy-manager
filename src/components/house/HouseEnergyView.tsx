/**
 * Illustrated house view for the Status page (issue #422) — an optional
 * alternative to the orbit diagram, chosen in Settings → Panel Controls.
 *
 * Another renderer over the shared energy-flow view model: numbers, status
 * words, colours and the noise threshold come from `useEnergyFlowViewModel`
 * (the same hook the orbit uses) via `buildHouseScene`, so the two views
 * always agree. The static house is drawn once by `HouseIllustration`; this
 * component draws what changes with each poll on top of it.
 *
 * Sized so the whole Status page fits an iPhone 12 Pro screen without
 * scrolling.
 */
import { memo } from 'react';
import { useEnergyFlowViewModel, type EnergyFlowSourceProps } from '../../hooks/useEnergyFlowViewModel';
import { useLiveWeather } from '../../hooks/useLiveWeather';
import type { ConnectionState } from '../../lib/types';
import { FLOW_COLORS } from '../../lib/energyFlow';
import {
  buildHouseScene,
  type GridTone,
  type HouseReading,
  type HouseRoute,
  type HouseStatusPill,
} from '../../lib/houseFlow';
import {
  BATTERY_BARS,
  BATTERY_BOX,
  EV_POST,
  HOUSE,
  HOUSE_VIEWBOX,
  panelCells,
  polygonPoints,
  type WorldPoint,
} from '../../lib/houseGeometry';
import { formatWeatherTemp, type CurrentWeather, type WeatherIconKind } from '../../lib/weatherIcon';
import HouseIllustration from './HouseIllustration';

export interface HouseEnergyViewProps extends EnergyFlowSourceProps {
  connectionState: ConnectionState;
  /** The inverter reports a grid fault (`hasGridFault`). */
  gridFault: boolean;
}

const MONO = 'var(--font-mono, monospace)';
const SANS = 'var(--font-sans, sans-serif)';
const TEXT_PRIMARY = 'var(--app-text-primary, #F0F6FC)';
const TEXT_SECONDARY = 'var(--app-text-secondary, #8B949E)';
const TONE_COLORS: Record<GridTone, string> = { ok: '#22C55E', warn: '#F59E0B', fault: '#EF4444' };
/** One dash + gap; the animation moves the dashes one period per cycle. */
const DASH = 6;
const GAP = 5;

const w = (x: number, y: number, z = 0): WorldPoint => ({ x, y, z });

function reducedMotion(): boolean {
  return typeof window !== 'undefined'
    && typeof window.matchMedia === 'function'
    && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

/** Rough text width for pill sizing (SVG text can't be measured in render). */
function textWidth(text: string, fontSize: number): number {
  return text.length * fontSize * 0.56;
}

function Pill({ x, y, text, color, testId }: { x: number; y: number; text: string; color: string; testId: string }) {
  const fontSize = 11;
  const width = textWidth(text, fontSize) + 20;
  return (
    <g data-testid={testId}>
      <rect x={x} y={y} width={width} height={20} rx={10} fill={color} fillOpacity={0.16} stroke={color} strokeOpacity={0.6} strokeWidth={1} />
      <text x={x + 10} y={y + 14} fill={color} fontSize={fontSize} fontWeight={600} fontFamily={SANS}>{text}</text>
    </g>
  );
}

function TopPill({ pill }: { pill: HouseStatusPill }) {
  const fontSize = 11;
  const modeW = textWidth(pill.mode, fontSize);
  const gridW = textWidth(pill.grid, fontSize) + 12;
  const width = modeW + gridW + 46;
  const x = (HOUSE_VIEWBOX.width - width) / 2;
  const tone = TONE_COLORS[pill.tone];
  const sepX = x + 16 + modeW + 7;
  return (
    <g data-testid="house-status-pill" data-tone={pill.tone}>
      <rect x={x} y={4} width={width} height={24} rx={12} fill="var(--app-bg-elevated, #21262D)" fillOpacity={0.7} stroke="var(--app-house-plot-edge, #30363D)" strokeWidth={1} />
      <text x={x + 16} y={20} fill={TEXT_SECONDARY} fontSize={fontSize} fontWeight={600} fontFamily={SANS} data-testid="house-mode">{pill.mode}</text>
      <line x1={sepX} y1={10} x2={sepX} y2={22} stroke={TEXT_SECONDARY} strokeOpacity={0.5} strokeWidth={1} />
      <circle cx={sepX + 12} cy={16} r={3.5} fill={tone} />
      <text x={sepX + 20} y={20} fill={tone} fontSize={fontSize} fontWeight={600} fontFamily={SANS} data-testid="house-grid-state">{pill.grid}</text>
    </g>
  );
}

function WeatherGlyph({ kind, x, y }: { kind: WeatherIconKind; x: number; y: number }) {
  const sun = '#FBBF24';
  const cloud = '#9AA4AF';
  const sunAt = (cx: number, cy: number, r: number) => (
    <g>
      <circle cx={cx} cy={cy} r={r} fill={sun} />
      {Array.from({ length: 8 }).map((_, i) => {
        const a = (i * Math.PI) / 4;
        return <line key={i} x1={cx + Math.cos(a) * (r + 2)} y1={cy + Math.sin(a) * (r + 2)} x2={cx + Math.cos(a) * (r + 5)} y2={cy + Math.sin(a) * (r + 5)} stroke={sun} strokeWidth={1.8} strokeLinecap="round" />;
      })}
    </g>
  );
  const moonAt = (cx: number, cy: number, r: number) => (
    <path d={`M ${cx + r * 0.3} ${cy - r} A ${r} ${r} 0 1 0 ${cx + r} ${cy + r * 0.4} A ${r * 0.8} ${r * 0.8} 0 0 1 ${cx + r * 0.3} ${cy - r} Z`} fill="#CBD5E1" />
  );
  const cloudAt = (cx: number, cy: number, s: number, fill = cloud) => (
    <path d={`M ${cx - 11 * s} ${cy + 6 * s} a ${6 * s} ${6 * s} 0 0 1 ${2 * s} ${-11 * s} a ${8 * s} ${8 * s} 0 0 1 ${15 * s} ${-2 * s} a ${6 * s} ${6 * s} 0 0 1 ${5 * s} ${13 * s} Z`} fill={fill} />
  );
  const drops = (color: string, n: number, long: boolean) => (
    <g stroke={color} strokeWidth={1.8} strokeLinecap="round">
      {Array.from({ length: n }).map((_, i) => (
        <line key={i} x1={x + 6 + i * 6} y1={y + 19} x2={x + 4 + i * 6} y2={y + (long ? 26 : 23)} />
      ))}
    </g>
  );
  const cx = x + 14;
  const cy = y + 12;
  switch (kind) {
    case 'clear-day':
      return sunAt(cx, cy, 6);
    case 'clear-night':
      return moonAt(cx, cy, 8);
    case 'partly-day':
      return <g>{sunAt(cx + 5, cy - 4, 5)}{cloudAt(cx - 1, cy + 3, 0.9)}</g>;
    case 'partly-night':
      return <g>{moonAt(cx + 6, cy - 5, 6)}{cloudAt(cx - 1, cy + 3, 0.9)}</g>;
    case 'fog':
      return (
        <g stroke={cloud} strokeWidth={2} strokeLinecap="round">
          <line x1={x + 2} y1={cy - 4} x2={x + 26} y2={cy - 4} />
          <line x1={x + 5} y1={cy + 2} x2={x + 23} y2={cy + 2} />
          <line x1={x + 2} y1={cy + 8} x2={x + 26} y2={cy + 8} />
        </g>
      );
    case 'drizzle':
      return <g>{cloudAt(cx, cy, 1)}{drops('#60A5FA', 3, false)}</g>;
    case 'rain':
      return <g>{cloudAt(cx, cy, 1)}{drops('#3B82F6', 3, true)}</g>;
    case 'snow':
      return (
        <g>
          {cloudAt(cx, cy, 1)}
          {[0, 1, 2].map((i) => <circle key={i} cx={x + 6 + i * 7} cy={y + 23} r={1.8} fill="#E2E8F0" />)}
        </g>
      );
    case 'thunder':
      return (
        <g>
          {cloudAt(cx, cy, 1, '#6B7280')}
          <path d={`M ${cx} ${cy + 6} L ${cx - 4} ${cy + 13} L ${cx} ${cy + 13} L ${cx - 3} ${cy + 20} L ${cx + 5} ${cy + 10} L ${cx + 1} ${cy + 10} Z`} fill="#FBBF24" />
        </g>
      );
    case 'cloudy':
    default:
      return cloudAt(cx, cy, 1);
  }
}

function Weather({ weather }: { weather: CurrentWeather }) {
  return (
    <g data-testid="house-weather" data-kind={weather.kind} aria-label={`${weather.description}, ${formatWeatherTemp(weather.temperatureC)}`}>
      <WeatherGlyph kind={weather.kind} x={10} y={48} />
      <text x={44} y={68} fill={TEXT_PRIMARY} fontSize={16} fontWeight={600} fontFamily={MONO}>
        {formatWeatherTemp(weather.temperatureC)}
      </text>
    </g>
  );
}

function Reading({ r }: { r: HouseReading }) {
  const { x, valueY } = r.slot;
  const leader = r.leader.map((p) => `${p.x},${p.y}`).join(' ');
  return (
    <g data-testid={`house-reading-${r.id}`} data-active={r.active}>
      <polyline points={leader} fill="none" stroke={r.color} strokeWidth={1.2} strokeOpacity={0.85} />
      <circle cx={r.anchor.x} cy={r.anchor.y} r={7} fill={r.color} opacity={0.22} />
      <circle cx={r.anchor.x} cy={r.anchor.y} r={3.5} fill={r.color} />
      <text x={x + 6} y={valueY} fill={TEXT_PRIMARY} fontFamily={MONO} fontWeight={700} fontSize={22} data-testid={`house-value-${r.id}`}>
        {r.number}
        {r.unit && <tspan fontSize={12} fontWeight={500} fill={TEXT_SECONDARY} dx={3}>{r.unit}{r.suffix}</tspan>}
        {r.soc && <tspan fontSize={12} fontWeight={500} fill={TEXT_SECONDARY} dx={4}>· {r.soc}</tspan>}
      </text>
      <text
        x={x + 6}
        y={valueY + 18}
        fill={r.color}
        fontFamily={SANS}
        fontWeight={700}
        fontSize={11}
        letterSpacing={1.6}
        style={{ textTransform: 'uppercase' }}
      >
        {r.label}
      </text>
      {r.status && <Pill x={x + 4} y={valueY + 26} text={r.status} color={r.color} testId={`house-status-${r.id}`} />}
    </g>
  );
}

function Route({ route, reduced }: { route: HouseRoute; reduced: boolean }) {
  // Calm, length-independent pace: ~18 px/s for a trickle up to ~40 px/s at
  // the largest flow, so a big export still looks busier than a 200 W one.
  const speed = 18 + 22 * route.strength;
  const dur = `${((DASH + GAP) / speed).toFixed(2)}s`;
  const width = 2 + 1.5 * route.strength;
  return (
    <g data-testid={`house-route-${route.id}`} data-color={route.color}>
      <path d={route.d} fill="none" stroke={route.color} strokeWidth={width + 4} strokeOpacity={0.14} strokeLinecap="round" strokeLinejoin="round" />
      <path
        d={route.d}
        fill="none"
        stroke={route.color}
        strokeWidth={width}
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeDasharray={`${DASH} ${GAP}`}
      >
        {!reduced && (
          <animate attributeName="stroke-dashoffset" from="0" to={String(-(DASH + GAP))} dur={dur} repeatCount="indefinite" />
        )}
      </path>
    </g>
  );
}

function BatteryCabinet({ barsLit, color }: { barsLit: number; color: string }) {
  const x0 = HOUSE.x1;
  const x1 = HOUSE.x1 + BATTERY_BOX.depth;
  const { y0, y1, z0, z1 } = BATTERY_BOX;
  const fill = 'var(--app-house-device, #161B22)';
  const barH = (z1 - z0 - 0.7) / BATTERY_BARS;
  const bars = Array.from({ length: BATTERY_BARS }).map((_, i) => {
    const bz0 = z0 + 0.35 + i * barH;
    const bz1 = bz0 + barH - 0.07;
    const lit = i < barsLit;
    return (
      <polygon
        key={i}
        data-testid="house-battery-bar"
        data-lit={lit}
        points={polygonPoints([w(x1 + 0.01, y1 - 0.4, bz0), w(x1 + 0.01, y0 + 0.4, bz0), w(x1 + 0.01, y0 + 0.4, bz1), w(x1 + 0.01, y1 - 0.4, bz1)])}
        fill={color}
        fillOpacity={lit ? 0.9 : 0.12}
      />
    );
  });
  return (
    <g data-testid="house-battery">
      <polygon points={polygonPoints([w(x0, y1, z0), w(x1, y1, z0), w(x1, y1, z1), w(x0, y1, z1)])} fill={fill} stroke={color} strokeWidth={1.3} strokeLinejoin="round" />
      <polygon points={polygonPoints([w(x1, y1, z0), w(x1, y0, z0), w(x1, y0, z1), w(x1, y1, z1)])} fill={fill} stroke={color} strokeWidth={1.3} strokeLinejoin="round" />
      <polygon points={polygonPoints([w(x0, y0, z1), w(x1, y0, z1), w(x1, y1, z1), w(x0, y1, z1)])} fill={fill} stroke={color} strokeWidth={1.3} strokeLinejoin="round" />
      {bars}
    </g>
  );
}

function EvPost() {
  const c = FLOW_COLORS.ev;
  const s = 0.18;
  const { x, y, height } = EV_POST;
  const fill = 'var(--app-house-device, #161B22)';
  return (
    <g data-testid="house-ev-post">
      <polygon points={polygonPoints([w(x - s, y + s, 0), w(x + s, y + s, 0), w(x + s, y + s, height), w(x - s, y + s, height)])} fill={fill} stroke={c} strokeWidth={1.2} />
      <polygon points={polygonPoints([w(x + s, y + s, 0), w(x + s, y - s, 0), w(x + s, y - s, height), w(x + s, y + s, height)])} fill={fill} stroke={c} strokeWidth={1.2} />
      <polygon points={polygonPoints([w(x - s, y - s, height), w(x + s, y - s, height), w(x + s, y + s, height), w(x - s, y + s, height)])} fill={c} />
    </g>
  );
}

function HouseEnergyViewInner({ connectionState, gridFault, ...source }: HouseEnergyViewProps) {
  const vm = useEnergyFlowViewModel(source);
  const weather = useLiveWeather();
  const scene = buildHouseScene(vm, { connectionState, gridFault });
  const reduced = reducedMotion();
  const solarColor = FLOW_COLORS.solar;

  return (
    <div className="flex justify-center w-full" data-testid="house-energy-view">
      <svg
        viewBox={`0 0 ${HOUSE_VIEWBOX.width} ${HOUSE_VIEWBOX.height}`}
        className="w-full"
        style={{ maxWidth: '440px', fontFamily: SANS }}
        role="img"
        aria-label={`Energy flow. ${vm.summaryText}`}
      >
        <TopPill pill={scene.pill} />
        {weather && <Weather weather={weather} />}

        <HouseIllustration />

        {/* Panels glow in the solar colour while generating. */}
        <g data-testid="house-panel-glow" data-active={scene.solarActive}>
          {panelCells().map((cell, i) => (
            <polygon
              key={i}
              points={polygonPoints(cell)}
              fill={solarColor}
              fillOpacity={scene.solarActive ? 0.42 : 0.06}
              stroke={solarColor}
              strokeOpacity={scene.solarActive ? 0.95 : 0.45}
              strokeWidth={1}
              strokeLinejoin="round"
            />
          ))}
        </g>

        <BatteryCabinet barsLit={scene.battery.barsLit} color={scene.battery.color} />
        {source.showEvc && <EvPost />}

        {/* Idle wiring, then one lane per active flow. */}
        <g data-testid="house-idle-tracks">
          {scene.idleTracks.map((d, i) => (
            <path key={i} d={d} fill="none" stroke="var(--app-text-secondary, #8B949E)" strokeOpacity={0.28} strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round" />
          ))}
        </g>
        {scene.routes.map((route) => (
          <Route key={route.id} route={route} reduced={reduced} />
        ))}

        {scene.readings.map((r) => (
          <Reading key={r.id} r={r} />
        ))}
      </svg>
    </div>
  );
}

const HouseEnergyView = memo(HouseEnergyViewInner);
export default HouseEnergyView;
