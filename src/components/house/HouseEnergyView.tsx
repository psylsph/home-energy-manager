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
 * Sized so the Status page (house, its two cards, header) fits an iPhone 12
 * Pro screen without scrolling while the Today statistics are hidden.
 */
import { memo, useEffect, useRef, useState } from 'react';
import { useEnergyFlowViewModel, type EnergyFlowSourceProps } from '../../hooks/useEnergyFlowViewModel';
import { useLiveWeather } from '../../hooks/useLiveWeather';
import { useCarbonIntensity } from '../../hooks/useCarbonIntensity';
import {
  CARBON_BAND_COLORS,
  carbonLabel,
  formatCarbon,
  formatCarbonTime,
  type CurrentCarbon,
} from '../../lib/carbonIntensity';
import type { ConnectionState } from '../../lib/types';
import { FLOW_COLORS } from '../../lib/energyFlow';
import {
  READING_HIT_WIDTH,
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
import {
  SOLAR_LEVEL_BARS,
  formatIrradiance,
  formatWeatherTemp,
  solarLevel,
  type CurrentWeather,
  type WeatherIconKind,
} from '../../lib/weatherIcon';
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
  const { solarWm2 } = weather;
  const lit = solarWm2 == null ? 0 : solarLevel(solarWm2);
  const label = `${weather.description}, ${formatWeatherTemp(weather.temperatureC)}`
    + (solarWm2 == null ? '' : `, sunlight ${formatIrradiance(solarWm2)}`);
  return (
    <g data-testid="house-weather" data-kind={weather.kind} aria-label={label}>
      <WeatherGlyph kind={weather.kind} x={10} y={48} />
      <text x={44} y={68} fill={TEXT_PRIMARY} fontSize={16} fontWeight={600} fontFamily={MONO}>
        {formatWeatherTemp(weather.temperatureC)}
      </text>
      {solarWm2 != null && (
        // Solar intensity: five bars (200 W/m² each) plus the reading.
        <g data-testid="house-solar-intensity" data-level={lit}>
          {Array.from({ length: SOLAR_LEVEL_BARS }, (_, i) => (
            <rect
              key={i}
              data-testid="house-solar-bar"
              data-lit={i < lit}
              x={14 + i * 5} y={86 - (i + 1) * 2.2} width={3.5} height={(i + 1) * 2.2} rx={0.8}
              fill={i < lit ? FLOW_COLORS.solar : 'var(--color-bg-elevated)'}
            />
          ))}
          <text x={44} y={86} fill={TEXT_SECONDARY} fontSize={11} fontFamily={MONO} data-testid="house-solar-value">
            {formatIrradiance(solarWm2)}
          </text>
        </g>
      )}
    </g>
  );
}

/**
 * Grid carbon intensity (issue #401) under the weather: a leaf in the band's
 * colour, the intensity now, and when it is lowest ahead (`↓03:30`) — kept
 * short so the row stays clear of the chimney. Moves up into
 * the weather's place when the weather (or its solar line) isn't shown.
 */
function Carbon({ carbon, y }: { carbon: CurrentCarbon; y: number }) {
  const color = carbon.band ? CARBON_BAND_COLORS[carbon.band] : TEXT_SECONDARY;
  const cx = 20;
  const cy = y - 4;
  return (
    <g data-testid="house-carbon" data-band={carbon.band ?? ''} aria-label={carbonLabel(carbon)}>
      <path
        data-testid="house-carbon-leaf"
        d={`M ${cx - 6} ${cy + 5} C ${cx - 6} ${cy - 3} ${cx} ${cy - 6} ${cx + 6} ${cy - 6} C ${cx + 6} ${cy} ${cx + 2} ${cy + 5} ${cx - 6} ${cy + 5} Z`}
        fill={color}
      />
      <line x1={cx - 6} y1={cy + 5} x2={cx + 2} y2={cy - 2} stroke="var(--app-bg-base, #0D1117)" strokeWidth={1} strokeLinecap="round" />
      <text x={44} y={y} fill={TEXT_SECONDARY} fontSize={11} fontFamily={MONO} data-testid="house-carbon-value">
        {formatCarbon(carbon.intensity)}
        {carbon.lowest && (
          <tspan fontSize={10} dx={4} data-testid="house-carbon-lowest">
            ↓{formatCarbonTime(carbon.lowest.at)}
          </tspan>
        )}
      </text>
    </g>
  );
}

function Reading({ r, open, onShow, onHide, onToggle }: {
  r: HouseReading;
  open: boolean;
  onShow: () => void;
  onHide: () => void;
  onToggle: () => void;
}) {
  const { x, valueY } = r.slot;
  const leader = r.leader.map((p) => `${p.x},${p.y}`).join(' ');
  // The pointer that pressed, recorded on pointerdown: not every engine the
  // app runs in (older WebKitGTK) sends click as a PointerEvent.
  const pressedWith = useRef('');
  // Readings with volts / amps show them on hover, on tap (no hover on a
  // phone), or from the keyboard: each is a focusable button.
  const tip = r.detail
    ? {
        role: 'button',
        tabIndex: 0,
        'aria-label': `${r.label} volts and amps`,
        'aria-expanded': open,
        onKeyDown: (e: React.KeyboardEvent) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            onToggle();
          } else if (e.key === 'Escape') {
            onHide();
          }
        },
        onBlur: onHide,
        onPointerEnter: (e: React.PointerEvent) => { if (e.pointerType === 'mouse') onShow(); },
        onPointerLeave: (e: React.PointerEvent) => { if (e.pointerType === 'mouse') onHide(); },
        onPointerDown: (e: React.PointerEvent) => { pressedWith.current = e.pointerType; },
        // A mouse has already opened it by hovering, so its click mustn't close
        // it again; a tap (no hover) toggles.
        onClick: (e: React.MouseEvent) => {
          e.stopPropagation();
          if (pressedWith.current === 'mouse') onShow();
          else onToggle();
        },
        style: { cursor: 'pointer' },
      }
    : {};
  return (
    <g data-testid={`house-reading-${r.id}`} data-active={r.active} {...tip}>
      <polyline points={leader} fill="none" stroke={r.color} strokeWidth={1.2} strokeOpacity={0.85} />
      <circle cx={r.anchor.x} cy={r.anchor.y} r={7} fill={r.color} opacity={0.22} />
      <circle cx={r.anchor.x} cy={r.anchor.y} r={3.5} fill={r.color} />
      {/* Hit area over the value and label, so the gaps between letters still
          count; narrow enough to stay clear of the next reading (Solar's would
          otherwise reach over Home's text). */}
      {r.detail && <rect x={x} y={valueY - 24} width={READING_HIT_WIDTH} height={46} fill="transparent" data-testid={`house-hit-${r.id}`} />}
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
        {r.percent && (
          <tspan fill={TEXT_SECONDARY} fontWeight={500} letterSpacing={0} dx={5} data-testid={`house-percent-${r.id}`}>
            · {r.percent}
          </tspan>
        )}
      </text>
      {r.status && <Pill x={x + 4} y={valueY + 26} text={r.status} color={r.color} testId={`house-status-${r.id}`} />}
    </g>
  );
}

/** The volts / amps bubble: under a top-row reading, above a bottom-row one, kept inside the view. */
function DetailTip({ r }: { r: HouseReading }) {
  const fontSize = 12;
  const width = textWidth(r.detail!, fontSize) + 20;
  const height = 24;
  const below = r.slot.edgeY > r.slot.valueY;
  const x = Math.max(4, Math.min(r.slot.x + 4, HOUSE_VIEWBOX.width - width - 4));
  const y = below ? r.slot.valueY + 24 : r.slot.valueY - 52;
  return (
    <g data-testid="house-detail-tip" data-for={r.id} pointerEvents="none">
      <rect x={x} y={y} width={width} height={height} rx={6} fill="var(--app-bg-elevated, #21262D)" stroke={r.color} strokeOpacity={0.7} strokeWidth={1} />
      <text x={x + 10} y={y + 16} fill={TEXT_PRIMARY} fontSize={fontSize} fontFamily={MONO}>{r.detail}</text>
    </g>
  );
}

function Route({ route, reduced }: { route: HouseRoute; reduced: boolean }) {
  // Calm, length-independent pace: ~18 px/s up to ~40 px/s for the busiest
  // cable, so the cable carrying the most power looks busiest.
  const speed = 18 + 22 * route.strength;
  const dur = `${((DASH + GAP) / speed).toFixed(2)}s`;
  const width = 2 + 1.5 * route.strength;
  return (
    <g data-testid={`house-route-${route.id}`} data-color={route.color} data-supplies={route.supplies}>
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
  const carbon = useCarbonIntensity();
  const carbonY = !weather ? 68 : weather.solarWm2 == null ? 86 : 104;
  const scene = buildHouseScene(vm, { connectionState, gridFault });
  const reduced = reducedMotion();
  const solarColor = FLOW_COLORS.solar;
  const [tipFor, setTipFor] = useState<string | null>(null);
  const tipReading = scene.readings.find((r) => r.id === tipFor && r.detail);
  // The open reading lost its detail (e.g. one poll without a battery
  // voltage): forget it, so the bubble can't reopen by itself when the detail
  // returns. Adjusting state during render is React's pattern for this.
  if (tipFor !== null && !tipReading) setTipFor(null);
  const svgRef = useRef<SVGSVGElement>(null);
  // A tap anywhere outside the diagram (the cards, the header) closes the
  // bubble too; taps inside are handled by the svg's own onClick.
  useEffect(() => {
    if (tipFor === null) return;
    const close = (e: PointerEvent) => {
      if (!svgRef.current?.contains(e.target as Node)) setTipFor(null);
    };
    document.addEventListener('pointerdown', close);
    return () => document.removeEventListener('pointerdown', close);
  }, [tipFor]);
  // The volts / amps only appear on hover or tap, so screen readers get them
  // in the diagram's label instead.
  const details = scene.readings
    .filter((r) => r.detail)
    .map((r) => `${r.label} ${r.detail}.`)
    .join(' ');

  return (
    <div className="flex justify-center w-full" data-testid="house-energy-view">
      <svg
        viewBox={`0 0 ${HOUSE_VIEWBOX.width} ${HOUSE_VIEWBOX.height}`}
        className="w-full"
        style={{ maxWidth: '440px', fontFamily: SANS }}
        ref={svgRef}
        // A labelled group, not an image: an image hides everything inside it
        // from assistive tech, and the readings are buttons.
        role="group"
        aria-label={[`Energy flow. ${vm.summaryText}`, details].filter(Boolean).join(' ')}
        onClick={() => setTipFor(null)}
      >
        <TopPill pill={scene.pill} />
        {weather && <Weather weather={weather} />}
        {carbon && <Carbon carbon={carbon} y={carbonY} />}

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

        {/* Idle wiring, then one moving cable per part with power on it. */}
        <g data-testid="house-idle-tracks">
          {scene.idleTracks.map((d, i) => (
            <path key={i} d={d} fill="none" stroke="var(--app-text-secondary, #8B949E)" strokeOpacity={0.28} strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round" />
          ))}
        </g>
        {scene.routes.map((route) => (
          <Route key={route.id} route={route} reduced={reduced} />
        ))}

        {scene.readings.map((r) => (
          <Reading
            key={r.id}
            r={r}
            open={tipFor === r.id}
            onShow={() => setTipFor(r.id)}
            onHide={() => setTipFor((cur) => (cur === r.id ? null : cur))}
            onToggle={() => setTipFor((cur) => (cur === r.id ? null : r.id))}
          />
        ))}
        {/* Drawn last so the bubble sits over the house and the lines. */}
        {tipReading && <DetailTip r={tipReading} />}
      </svg>
    </div>
  );
}

const HouseEnergyView = memo(HouseEnergyViewInner);
export default HouseEnergyView;
