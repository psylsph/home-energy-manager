/**
 * The two cards under the house view (issue #422, round 2): the charge or
 * export slot running now or next, and the battery's stored energy with a
 * segmented level bar in the SOC colour. With Show Car Charging Slots on, the
 * schedule card adds the car's planned Octopus charge as a second line (issue
 * #474); it never changes the battery's own event.
 */
import type { InverterSnapshot } from '../../lib/types';
import { batteryEvent, batteryStoredKwh, doingIt, holdWindow, type BatteryEventKind, type TimedExportSchedule } from '../../lib/chargeSchedule';
import { useTimedExportSchedule } from '../../hooks/useTimedExportSchedule';
import { useCarChargeSlots } from '../../hooks/useCarChargeSlots';
import { carChargeLine, type CarChargeLine } from '../../lib/carChargeSlots';
import { useBatteryRuntime } from '../../hooks/useBatteryRuntime';
import { batteryRuntimeLine } from '../../lib/batteryRuntime';
import HelpTip from '../HelpTip';
import { inverterMinuteOfDay } from '../../lib/ecoTimedExport';
import { batteryBarsLit } from '../../lib/houseFlow';
import { CAP_MARKER_COLOR, capLineSoc, capTitle } from '../../lib/chargeCap';
import { BATTERY_BARS } from '../../lib/houseGeometry';
import { formatPercent } from '../../lib/format';
import { BATTERY_OUTPUT_COLOR, SOC_MID_COLOR, socColor } from '../../lib/energyFlow';
import { useInverterStore } from '../../store/useInverterStore';
import { deviceHasBatteryControls } from '../../lib/deviceCapabilities';

const CHARGE_GREEN = BATTERY_OUTPUT_COLOR;
/** The level bars' `gap-0.5`, which the cap marker sits in. */
const SEGMENT_GAP_PX = 2;
const EXPORT_AMBER = SOC_MID_COLOR;
/** Compact: the bars leave room for the Today statistics on the same screen (issue #429 round 2). */
const CARD = 'bg-bg-surface rounded-lg p-2 flex flex-col gap-1 min-w-0';
const LABEL = 'text-[11px] font-semibold uppercase tracking-wider text-text-secondary';

/** Battery outline with a plus (charge), a minus (export / discharge), or plain grey when nothing is scheduled. */
function BatteryGlyph({ kind, pulsing }: { kind: BatteryEventKind | 'none'; pulsing: boolean }) {
  const color = kind === 'charge' ? CHARGE_GREEN : kind === 'none' ? 'var(--color-text-secondary)' : EXPORT_AMBER;
  const mark = kind === 'charge' ? 'M13 5v8M9 9h8' : kind === 'none' ? null : 'M9 9h8';
  return (
    <svg
      width="26" height="16" viewBox="0 0 30 18" aria-hidden="true"
      data-testid="house-charge-icon" data-kind={kind} data-pulsing={pulsing}
      className={pulsing ? 'motion-safe:animate-pulse' : undefined}
    >
      <rect x="1" y="1" width="24" height="16" rx="3" fill="none" stroke={color} strokeWidth="2" />
      <rect x="26" y="6" width="3" height="6" rx="1" fill={color} />
      {mark && <path d={mark} stroke={color} strokeWidth="2.2" strokeLinecap="round" />}
    </svg>
  );
}

/** A small car, coloured like the charge it's waiting for. */
function CarGlyph({ active }: { active: boolean }) {
  const color = active ? CHARGE_GREEN : 'currentColor';
  return (
    <svg width="14" height="10" viewBox="0 0 14 10" aria-hidden="true" className="shrink-0">
      <path d="M2 6.5 3.4 3h7.2L12 6.5V8.5H2z" fill="none" stroke={color} strokeWidth="1.3" strokeLinejoin="round" />
      <circle cx="4.2" cy="8.6" r="1.1" fill={color} />
      <circle cx="9.8" cy="8.6" r="1.1" fill={color} />
    </svg>
  );
}

/** The car's planned charge, under the battery's window (issue #474). */
function CarChargeRow({ car }: { car: CarChargeLine | null }) {
  if (!car) return null;
  return (
    <span
      className="flex items-center gap-1 min-w-0 text-[11px] text-text-secondary"
      data-testid="house-car-charge"
      data-state={car.state}
      title={car.title || undefined}
    >
      <CarGlyph active={car.state === 'active'} />
      <span className="truncate">{car.text}</span>
    </span>
  );
}

const LABELS = {
  charge: { next: 'Next Charge', doing: 'Charging Now', window: 'Charge Window' },
  export: { next: 'Next Export', doing: 'Exporting Now', window: 'Export Window' },
  discharge: { next: 'Next Discharge', doing: 'Discharging Now', window: 'Discharge Window' },
} as const;

/** The charge or export slot running now, or the next to start. */
function ScheduleCard({ snapshot, timedExport, nowMinute, car }: {
  snapshot: InverterSnapshot;
  timedExport: TimedExportSchedule | null;
  nowMinute: number;
  car: CarChargeLine | null;
}) {
  const noise = useInverterStore((s) => s.visualNoiseThreshold);
  const event = batteryEvent(snapshot, timedExport, nowMinute);
  const kind = event?.kind ?? 'none';
  // Inside the window but not doing it (e.g. a charge short of its target, or
  // export paused): say so instead of claiming it, and don't pulse.
  const doing = event?.state === 'active' && doingIt(event.kind, snapshot, noise);
  const active = event?.state === 'active';
  // A charge slot whose target is reached holds the battery until it ends
  // (issue #470), so name the end rather than the window.
  const hold = active && !doing && event.kind === 'charge' ? holdWindow(snapshot, nowMinute) : null;
  const allDay = hold ? hold.start === hold.end : event != null && event.start === event.end;
  const label = !event
    ? 'Schedule'
    : hold ? (allDay ? 'Holding' : 'Holding Until')
    : active ? (doing ? LABELS[event.kind].doing : LABELS[event.kind].window) : LABELS[event.kind].next;
  return (
    <section className={CARD} data-testid="house-charge-card" data-kind={event?.kind ?? 'none'} data-state={event?.state ?? 'none'}>
      <BatteryGlyph kind={kind} pulsing={doing} />
      <span className={LABEL}>{label}</span>
      {event ? (
        <span className="font-mono text-base font-semibold text-text-primary whitespace-nowrap" data-testid="house-charge-window">
          {/* Merged slots can cover the whole day (00:00–00:00). */}
          {allDay ? 'All day' : hold ? hold.end : `${event.start}–${event.end}`}
        </span>
      ) : (
        <span className="text-sm text-text-secondary" data-testid="house-charge-window">None scheduled</span>
      )}
      <CarChargeRow car={car} />
    </section>
  );
}

/**
 * When the battery reaches its reserve (issue #472), with one to three dots
 * for how sure the forecast is. Off unless Show Battery Runtime is on.
 */
function RuntimeRow({ now }: { now: Date }) {
  const line = batteryRuntimeLine(useBatteryRuntime(), now);
  if (!line) return null;
  const confidence = line.confidence
    ? `${line.confidence[0]!.toUpperCase()}${line.confidence.slice(1)} confidence`
    : null;
  return (
    <span
      // Wraps rather than truncating: the time is the point, so on a phone
      // the dots and the ⓘ go to the next line instead of cutting it off.
      className="flex flex-wrap items-center gap-x-1.5 gap-y-0.5 min-w-0 text-[11px] leading-tight text-text-secondary"
      data-testid="house-battery-runtime"
      title={line.title || undefined}
    >
      <span className="min-w-0" data-testid="house-runtime-text">{line.text}</span>
      {line.dots != null && (
        <span className="flex gap-0.5 shrink-0" role="img" aria-label={confidence ?? undefined} data-testid="house-runtime-dots">
          {[1, 2, 3].map((n) => (
            <span
              key={n}
              data-testid="house-runtime-dot"
              data-lit={n <= line.dots!}
              className="h-1.5 w-1.5 rounded-full"
              style={{ backgroundColor: n <= line.dots! ? 'var(--color-text-secondary)' : 'var(--color-bg-elevated)' }}
            />
          ))}
        </span>
      )}
      {/* The dots need explaining, and phones have no hover title. */}
      {line.help && <HelpTip label="Battery Runtime" className="shrink-0">{line.help}</HelpTip>}
    </span>
  );
}

function BatteryCard({ snapshot, now }: { snapshot: InverterSnapshot; now: Date }) {
  const stored = batteryStoredKwh(snapshot.soc, snapshot.battery_capacity_kwh);
  const color = socColor(snapshot.soc);
  const lit = batteryBarsLit(snapshot.soc);
  const cap = capLineSoc(snapshot);
  return (
    <section className={CARD} data-testid="house-battery-card">
      <span className="flex items-baseline gap-1">
        <span className="font-mono text-xl font-semibold text-text-primary" data-testid="house-battery-stored">
          {stored === null ? '—' : stored.toFixed(1)}
        </span>
        <span className="font-mono text-xs text-text-secondary">kWh</span>
      </span>
      <span className={LABEL}>
        Battery Level · <span style={{ color }} data-testid="house-battery-level">{formatPercent(snapshot.soc)}</span>
      </span>
      <span className="relative flex gap-0.5" aria-hidden="true">
        {Array.from({ length: BATTERY_BARS }, (_, i) => (
          <span
            key={i}
            data-testid="house-battery-segment"
            data-lit={i < lit}
            className="h-2 flex-1 rounded-sm"
            style={{ backgroundColor: i < lit ? color : 'var(--color-bg-elevated)' }}
          />
        ))}
        {cap != null && (
          <span
            data-testid="house-battery-cap"
            title={capTitle(cap)}
            className="absolute -top-1.5 -bottom-1.5 w-[3px] -translate-x-1/2 rounded-full"
            // Centred on the gap between bars: n bars with a 2px gap put the
            // boundary at p% of (width + gap), less half a gap.
            style={{ left: `calc(${cap}% + ${(cap / 100) * SEGMENT_GAP_PX - SEGMENT_GAP_PX / 2}px)`, backgroundColor: CAP_MARKER_COLOR }}
          />
        )}
      </span>
      <RuntimeRow now={now} />
    </section>
  );
}

/**
 * On a Gateway (issue #428): its schedule registers are its own copy, not the
 * AIO's, so they can't say when the battery will next charge or export.
 */
function ScheduleCardUnavailable({ car }: { car: CarChargeLine | null }) {
  return (
    <section className={CARD} data-testid="house-charge-card" data-kind="none" data-state="unavailable">
      <BatteryGlyph kind="none" pulsing={false} />
      <span className={LABEL}>Schedule</span>
      <span className="text-sm text-text-secondary" data-testid="house-charge-window">Not available on a Gateway</span>
      {/* The car doesn't depend on the battery controls. */}
      <CarChargeRow car={car} />
    </section>
  );
}

/** Until the Timed Export schedule has loaded, so the card doesn't flick from Charge to Export. */
function ScheduleCardLoading() {
  return (
    <section className={CARD} data-testid="house-charge-card" data-kind="none" data-state="loading">
      {/* Matches the shrunken glyph, so the loading card is the same height. */}
      <span className="h-[16px]" aria-hidden="true" />
      <span className={LABEL}>Schedule</span>
      <span className="text-sm text-text-secondary" data-testid="house-charge-window">…</span>
    </section>
  );
}

/** The schedule card once the Timed Export schedule is fetched (not on a Gateway, where it isn't shown). */
function LiveScheduleCard({ snapshot, now, car, carLoaded }: {
  snapshot: InverterSnapshot;
  now: Date;
  car: CarChargeLine | null;
  carLoaded: boolean;
}) {
  const { schedule, loaded } = useTimedExportSchedule();
  // Slots are in inverter local time and HEM may run in a UTC container, so
  // the minute comes from the inverter's clock. The browser clock is only
  // the fallback for a snapshot with no readable `inverter_time` (older
  // backend), where it's all we have — windows may be off if the two
  // disagree.
  const nowMinute = inverterMinuteOfDay(snapshot) ?? now.getHours() * 60 + now.getMinutes();
  // Wait for the car check too, so its line doesn't make the card jump.
  return loaded && carLoaded
    ? <ScheduleCard snapshot={snapshot} timedExport={schedule} nowMinute={nowMinute} car={car} />
    : <ScheduleCardLoading />;
}

export default function HouseStatusCards({ snapshot, now = new Date() }: { snapshot: InverterSnapshot; now?: Date }) {
  // Dispatches are UTC instants, shown in the browser's time like the
  // Octopus page, not on the inverter's clock.
  const { response, loaded: carLoaded } = useCarChargeSlots();
  const car = carChargeLine(response, now);
  return (
    <div className="grid grid-cols-2 gap-3" data-testid="house-status-cards">
      {deviceHasBatteryControls(snapshot.device_type_code)
        ? <LiveScheduleCard snapshot={snapshot} now={now} car={car} carLoaded={carLoaded} />
        : <ScheduleCardUnavailable car={carLoaded ? car : null} />}
      <BatteryCard snapshot={snapshot} now={now} />
    </div>
  );
}
