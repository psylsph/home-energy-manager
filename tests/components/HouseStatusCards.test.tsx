import { describe, it, expect, afterEach, beforeEach, vi } from 'vitest';
import { render, screen, cleanup, act } from '@testing-library/react';

const timedExport = vi.hoisted(() => ({
  value: null as null | { schedule_enabled: boolean; slots: unknown[] },
  loaded: true,
  calls: 0,
}));
// The car's planned charges (issue #474), as `GET /api/octopus/dispatches`
// returns them; `null` while the switch is off or nothing usable came back.
const carSlots = vi.hoisted(() => ({
  value: null as null | Record<string, unknown>,
  loaded: true,
}));
vi.mock('../../src/hooks/useCarChargeSlots', () => ({
  useCarChargeSlots: () => ({ response: carSlots.value, loaded: carSlots.loaded }),
}));
// How long the battery lasts (issue #472); `null` while off or unknown.
const runtime = vi.hoisted(() => ({ value: null as null | Record<string, unknown> }));
vi.mock('../../src/hooks/useBatteryRuntime', () => ({
  useBatteryRuntime: () => runtime.value,
}));
vi.mock('../../src/hooks/useTimedExportSchedule', () => ({
  useTimedExportSchedule: () => {
    timedExport.calls += 1;
    return { schedule: timedExport.value, loaded: timedExport.loaded };
  },
}));

import HouseStatusCards from '../../src/components/house/HouseStatusCards';
import { socColor } from '../../src/lib/energyFlow';
import { LOCAL_FORMAT } from '../../src/lib/carChargeSlots';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot, ScheduleSlot } from '../../src/lib/types';

/** House view cards (issue #422, round 2): charge slot and battery level. */

const slot = (sh: number, sm: number, eh: number, em: number): ScheduleSlot => ({
  enabled: true, start_hour: sh, start_minute: sm, end_hour: eh, end_minute: em, target_soc: 100,
});

function snap(over: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    soc: 33.5,
    battery_capacity_kwh: 8.06,
    battery_state: 'idle',
    battery_power: 0,
    grid_power: 0,
    max_charge_slots: 2,
    max_discharge_slots: 2,
    enable_charge: true,
    charge_slots: [slot(23, 30, 5, 30)],
    inverter_time: '2026-11-01 18:00:00',
    ...over,
  } as InverterSnapshot;
}

const tid = (id: string) => screen.getByTestId(id);
/** A colour as jsdom normalises it in inline styles. */
const css = (color: string) => {
  const probe = document.createElement('span');
  probe.style.color = color;
  return probe.style.color;
};

describe('<HouseStatusCards/>', () => {
  beforeEach(() => {
    timedExport.value = null;
    timedExport.loaded = true;
    carSlots.value = null;
    carSlots.loaded = true;
    runtime.value = null;
    useInverterStore.setState({ visualNoiseThreshold: 20 });
  });
  afterEach(cleanup);

  it('shows the next charge slot with a still battery icon', () => {
    render(<HouseStatusCards snapshot={snap()} />);
    expect(tid('house-charge-card').getAttribute('data-state')).toBe('next');
    expect(screen.getByText('Next Charge')).toBeDefined();
    expect(tid('house-charge-window').textContent).toBe('23:30–05:30');
    expect(tid('house-charge-icon').getAttribute('data-pulsing')).toBe('false');
  });

  it('pulses the battery while a slot is running and the battery is charging', () => {
    render(<HouseStatusCards snapshot={snap({ inverter_time: '2026-11-01 01:00:00', battery_state: 'charging', battery_power: -3000 })} />);
    expect(tid('house-charge-card').getAttribute('data-state')).toBe('active');
    expect(screen.getByText('Charging Now')).toBeDefined();
    const icon = tid('house-charge-icon');
    expect(icon.getAttribute('data-pulsing')).toBe('true');
    // motion-safe: no pulse for people who ask for reduced motion.
    expect(icon.getAttribute('class')).toBe('motion-safe:animate-pulse');
  });

  it('says Holding Until the slot end, without pulsing, once the slot target is reached', () => {
    // Issue #470: a planner hold charges to its target, then sits in the
    // slot until it ends; the card says when that is.
    render(<HouseStatusCards snapshot={snap({ inverter_time: '2026-11-01 01:00:00', soc: 75, charge_slots: [{ ...slot(23, 30, 5, 30), target_soc: 75 }] })} />);
    expect(tid('house-charge-card').getAttribute('data-state')).toBe('active');
    expect(screen.getByText('Holding Until')).toBeDefined();
    expect(tid('house-charge-window').textContent).toBe('05:30');
    expect(tid('house-charge-icon').getAttribute('data-pulsing')).toBe('false');
  });

  it('says Charge Window, not holding, when the battery is idle below the slot target', () => {
    // Not charging short of the target is a stalled charge (charge cap, cold
    // battery, first poll of the slot), not a hold.
    render(<HouseStatusCards snapshot={snap({ inverter_time: '2026-11-01 01:00:00', battery_state: 'idle' })} />);
    expect(screen.getByText('Charge Window')).toBeDefined();
    expect(screen.queryByText('Holding Until')).toBeNull();
    expect(tid('house-charge-window').textContent).toBe('23:30–05:30');
    expect(tid('house-charge-icon').getAttribute('data-pulsing')).toBe('false');
  });

  it('says Charge Window, not holding, inside a slot while the battery discharges below target', () => {
    render(<HouseStatusCards snapshot={snap({ inverter_time: '2026-11-01 01:00:00', battery_state: 'discharging', battery_power: 800 })} />);
    expect(screen.getByText('Charge Window')).toBeDefined();
    expect(screen.queryByText('Holding Until')).toBeNull();
    expect(tid('house-charge-window').textContent).toBe('23:30–05:30');
  });

  it('says none scheduled, with a plain grey battery, when Timed Charge is off', () => {
    render(<HouseStatusCards snapshot={snap({ enable_charge: false })} />);
    expect(tid('house-charge-card').getAttribute('data-state')).toBe('none');
    expect(screen.getByText('Schedule')).toBeDefined();
    expect(screen.queryByText('Next Charge')).toBeNull();
    expect(tid('house-charge-window').textContent).toBe('None scheduled');
    const icon = tid('house-charge-icon');
    expect(icon.getAttribute('data-kind')).toBe('none');
    expect(icon.querySelector('path')).toBeNull();
  });

  it("says the schedule isn't available on a Gateway rather than none scheduled", () => {
    // Issue #428: a Gateway's schedule registers are its own copy, not the
    // AIO's, so neither they nor HEM's Timed Export schedule say what the
    // battery will do.
    timedExport.value = { schedule_enabled: true, slots: [slot(16, 0, 19, 0)] };
    render(<HouseStatusCards snapshot={snap({ device_type_code: '7001' })} />);
    expect(tid('house-charge-card').getAttribute('data-state')).toBe('unavailable');
    expect(screen.getByText('Schedule')).toBeDefined();
    expect(screen.queryByText('Next Charge')).toBeNull();
    expect(screen.queryByText('Next Export')).toBeNull();
    expect(tid('house-charge-window').textContent).toBe('Not available on a Gateway');
    expect(tid('house-charge-icon').getAttribute('data-kind')).toBe('none');
    // The battery card is unaffected.
    expect(tid('house-battery-card')).toBeDefined();
  });

  it("doesn't fetch the Timed Export schedule on a Gateway, since it isn't shown", () => {
    timedExport.calls = 0;
    render(<HouseStatusCards snapshot={snap({ device_type_code: '7001' })} />);
    expect(timedExport.calls).toBe(0);
    cleanup();
    render(<HouseStatusCards snapshot={snap({ device_type_code: '8001' })} />);
    expect(timedExport.calls).toBeGreaterThan(0);
  });

  it('says All day when the merged slots cover the whole day', () => {
    render(<HouseStatusCards snapshot={snap({ charge_slots: [slot(0, 0, 12, 0), slot(12, 0, 0, 0)] })} />);
    expect(tid('house-charge-card').getAttribute('data-state')).toBe('active');
    expect(tid('house-charge-window').textContent).toBe('All day');
  });

  it('says Holding, all day, when an all-day charge has reached its target', () => {
    render(<HouseStatusCards snapshot={snap({ soc: 100, charge_slots: [slot(0, 0, 12, 0), slot(12, 0, 0, 0)] })} />);
    expect(screen.getByText('Holding')).toBeDefined();
    expect(tid('house-charge-window').textContent).toBe('All day');
  });

  describe('discharge slots', () => {
    const exportSnap = (over: Partial<InverterSnapshot> = {}) => snap({
      enable_discharge: true,
      discharge_slots: [slot(16, 0, 19, 0)],
      battery_mode: 'timed_export',
      inverter_time: '2026-11-01 12:00:00',
      ...over,
    });

    it('says Next Export, with a minus battery, when an export comes first', () => {
      render(<HouseStatusCards snapshot={exportSnap()} />);
      expect(tid('house-charge-card').getAttribute('data-kind')).toBe('export');
      expect(screen.getByText('Next Export')).toBeDefined();
      expect(tid('house-charge-window').textContent).toBe('16:00–19:00');
      expect(tid('house-charge-icon').getAttribute('data-kind')).toBe('export');
      expect(tid('house-charge-icon').querySelector('path')!.getAttribute('d')).toBe('M9 9h8');
    });

    it('says Exporting Now and pulses while the export runs', () => {
      render(<HouseStatusCards snapshot={exportSnap({ inverter_time: '2026-11-01 17:00:00', battery_state: 'discharging', battery_power: 2500, grid_power: 2200 })} />);
      expect(screen.getByText('Exporting Now')).toBeDefined();
      expect(tid('house-charge-icon').getAttribute('data-pulsing')).toBe('true');
    });

    it('says Export Window when nothing is moving inside the window', () => {
      render(<HouseStatusCards snapshot={exportSnap({ inverter_time: '2026-11-01 17:00:00', battery_state: 'idle', battery_power: 10, grid_power: 5 })} />);
      expect(screen.getByText('Export Window')).toBeDefined();
      expect(tid('house-charge-icon').getAttribute('data-pulsing')).toBe('false');
    });

    it('says Export Window, without pulsing, while export is paused', () => {
      render(<HouseStatusCards snapshot={exportSnap({ inverter_time: '2026-11-01 17:00:00', battery_mode: 'export_paused', battery_state: 'idle' })} />);
      expect(screen.getByText('Export Window')).toBeDefined();
      expect(tid('house-charge-icon').getAttribute('data-pulsing')).toBe('false');
    });

    it('calls Timed Demand slots discharges, not exports', () => {
      render(<HouseStatusCards snapshot={exportSnap({ battery_mode: 'timed_demand' })} />);
      expect(tid('house-charge-card').getAttribute('data-kind')).toBe('discharge');
      expect(screen.getByText('Next Discharge')).toBeDefined();
      cleanup();
      render(<HouseStatusCards snapshot={exportSnap({ battery_mode: 'timed_demand', inverter_time: '2026-11-01 17:00:00', battery_state: 'discharging', battery_power: 800 })} />);
      expect(screen.getByText('Discharging Now')).toBeDefined();
      cleanup();
      render(<HouseStatusCards snapshot={exportSnap({ battery_mode: 'timed_demand', inverter_time: '2026-11-01 17:00:00' })} />);
      expect(screen.getByText('Discharge Window')).toBeDefined();
    });

    it('shows the managed Timed Export while the inverter sits in Eco with its slots cleared', async () => {
      timedExport.value = { schedule_enabled: true, slots: [slot(16, 0, 19, 0)] };
      render(<HouseStatusCards snapshot={snap({ inverter_time: '2026-11-01 12:00:00', battery_mode: 'eco', enable_discharge: false, discharge_slots: [] })} />);
      await act(async () => {});
      expect(tid('house-charge-card').getAttribute('data-kind')).toBe('export');
      expect(screen.getByText('Next Export')).toBeDefined();
      expect(tid('house-charge-window').textContent).toBe('16:00–19:00');
    });

    it('goes back to the charge once the export has passed', () => {
      render(<HouseStatusCards snapshot={exportSnap({ inverter_time: '2026-11-01 20:00:00' })} />);
      expect(tid('house-charge-card').getAttribute('data-kind')).toBe('charge');
      expect(screen.getByText('Next Charge')).toBeDefined();
    });
  });

  it('keeps saying Holding Until while a held battery wobbles a few tens of watts', () => {
    const held = { inverter_time: '2026-11-01 01:00:00', soc: 75, charge_slots: [{ ...slot(23, 30, 5, 30), target_soc: 75 }] };
    for (const battery_power of [-5, 40, 0, 15]) {
      render(<HouseStatusCards snapshot={snap({ ...held, battery_power })} />);
      expect(screen.getByText('Holding Until')).toBeDefined();
      expect(tid('house-charge-window').textContent).toBe('05:30');
      cleanup();
    }
  });

  it('ends the hold where a higher-target slot starts charging again, not at the merged window end', () => {
    render(<HouseStatusCards snapshot={snap({
      inverter_time: '2026-11-01 05:00:00', soc: 75,
      charge_slots: [{ ...slot(4, 0, 5, 30), target_soc: 75 }, { ...slot(5, 30, 7, 0), target_soc: 100 }],
    })} />);
    expect(screen.getByText('Holding Until')).toBeDefined();
    expect(tid('house-charge-window').textContent).toBe('05:30');
  });

  it('waits for the Timed Export schedule before saying what comes next', () => {
    timedExport.loaded = false;
    render(<HouseStatusCards snapshot={snap()} />);
    expect(tid('house-charge-card').getAttribute('data-state')).toBe('loading');
    expect(tid('house-charge-window').textContent).toBe('…');
    expect(screen.queryByText('Next Charge')).toBeNull();
    // The battery card doesn't wait.
    expect(tid('house-battery-stored').textContent).toBe('2.7');
  });

  it("falls back to the browser clock when the inverter's time is missing", () => {
    render(<HouseStatusCards snapshot={snap({ inverter_time: '' })} now={new Date(2026, 10, 1, 2, 0)} />);
    expect(tid('house-charge-card').getAttribute('data-state')).toBe('active');
  });

  it('shows the stored energy and the level in the SOC colour', () => {
    render(<HouseStatusCards snapshot={snap()} />);
    expect(tid('house-battery-stored').textContent).toBe('2.7');
    const level = tid('house-battery-level');
    expect(level.textContent).toBe('34%');
    expect(level.style.color).toBe(css(socColor(33.5)));
    const segments = screen.getAllByTestId('house-battery-segment');
    // 5% each, so a cap on a multiple of 5 falls between two bars.
    expect(segments).toHaveLength(20);
    const lit = segments.filter((s) => s.getAttribute('data-lit') === 'true');
    expect(lit).toHaveLength(7);
    for (const s of lit) expect(s.style.backgroundColor).toBe(css(socColor(33.5)));
    expect(segments[19].style.backgroundColor).toBe('var(--color-bg-elevated)');
  });

  it('shows a dash when the capacity is unknown', () => {
    render(<HouseStatusCards snapshot={snap({ battery_capacity_kwh: 0, soc: 0 })} />);
    expect(tid('house-battery-stored').textContent).toBe('—');
    expect(screen.getAllByTestId('house-battery-segment').filter((s) => s.getAttribute('data-lit') === 'true')).toHaveLength(0);
  });

  // Charge cap (issue #426).
  it('marks the charge cap on the level bars only while it is on', () => {
    render(<HouseStatusCards snapshot={snap({ charge_cap_enabled: true, charge_cap_soc: 90 })} />);
    const cap = tid('house-battery-cap');
    // In the 2px gap after the 18th bar: 90% of (width + gap), less half a gap.
    expect(cap.style.left).toBe('calc(90% + 0.8px)');
    expect(cap.style.backgroundColor).toBe('var(--app-accent, #22D3EE)');
    expect(cap.getAttribute('title')).toBe('Charge cap 90%');
    cleanup();
    render(<HouseStatusCards snapshot={snap({ charge_cap_enabled: false, charge_cap_soc: 90 })} />);
    expect(screen.queryByTestId('house-battery-cap')).toBeNull();
  });

  it('stays compact so the taller Today bars fit the phone screen', () => {
    // Issue #429 round 2: the two cards shrank to pay for the bars.
    render(<HouseStatusCards snapshot={snap({ battery_capacity_kwh: 9.5 })} />);
    const charge = tid('house-charge-card');
    expect(charge.className).toContain('p-2');
    expect(charge.className).toContain('gap-1');
    // The glyph scaled down with the card.
    expect(tid('house-charge-icon').getAttribute('width')).toBe('26');
    expect(tid('house-charge-window').className).toContain('text-base');
    expect(tid('house-battery-stored').className).toContain('text-xl');
    expect(screen.getByTestId('house-battery-card').className).toContain('p-2');
    expect(screen.getAllByTestId('house-battery-segment')[0].className).toContain('h-2');
  });
});


describe('<HouseStatusCards/> car charging line (issue #474)', () => {
  // 18:00 UTC on 1 November; the car line is in browser-local time.
  const NOW = new Date('2026-11-01T18:00:00Z');
  const at = (iso: string) => LOCAL_FORMAT.time(new Date(iso));
  const dispatches = (list: Array<{ start: string; end: string; smart?: boolean; kwh?: number }>) => ({
    ok: true,
    enabled: false,
    configured: true,
    checked_at: '2026-11-01T17:58:00Z',
    error: null,
    devices: 1,
    holding: false,
    hold_failing: false,
    stale_after_minutes: 15,
    dispatches: list.map((d) => ({
      start: d.start,
      end: d.end,
      type: d.smart === false ? 'BOOST' : 'SMART',
      energy_added_kwh: d.kwh ?? null,
      smart: d.smart !== false,
    })),
  });

  beforeEach(() => {
    timedExport.value = null;
    timedExport.loaded = true;
    carSlots.value = null;
    carSlots.loaded = true;
    runtime.value = null;
    useInverterStore.setState({ visualNoiseThreshold: 20 });
  });
  afterEach(cleanup);

  it('shows no line when there is nothing to show', () => {
    render(<HouseStatusCards snapshot={snap()} now={NOW} />);
    expect(screen.queryByTestId('house-car-charge')).toBeNull();
  });

  it('shows the next car charge under the battery window, which is unchanged', () => {
    carSlots.value = dispatches([{ start: '2026-11-01T23:30:00Z', end: '2026-11-02T05:30:00Z', kwh: 12.34 }]);
    render(<HouseStatusCards snapshot={snap()} now={NOW} />);
    const line = tid('house-car-charge');
    expect(line.getAttribute('data-state')).toBe('next');
    expect(line.textContent).toBe(`Car ${at('2026-11-01T23:30:00Z')}–${at('2026-11-02T05:30:00Z')}`);
    expect(line.getAttribute('title')).toBe('12.3 kWh planned');
    // The battery's own event is untouched.
    expect(screen.getByText('Next Charge')).toBeDefined();
    expect(tid('house-charge-window').textContent).toBe('23:30–05:30');
    expect(tid('house-charge-card').contains(line)).toBe(true);
  });

  it("says until when a running car charge goes, without changing the battery's label", () => {
    carSlots.value = dispatches([{ start: '2026-11-01T17:30:00Z', end: '2026-11-01T19:00:00Z' }]);
    render(<HouseStatusCards snapshot={snap()} now={NOW} />);
    const line = tid('house-car-charge');
    expect(line.getAttribute('data-state')).toBe('active');
    expect(line.textContent).toBe(`Car charging until ${at('2026-11-01T19:00:00Z')}`);
    expect(screen.getByText('Next Charge')).toBeDefined();
    expect(tid('house-charge-icon').getAttribute('data-pulsing')).toBe('false');
  });

  it('marks a boost', () => {
    carSlots.value = dispatches([{ start: '2026-11-01T19:00:00Z', end: '2026-11-01T20:00:00Z', smart: false }]);
    render(<HouseStatusCards snapshot={snap()} now={NOW} />);
    expect(tid('house-car-charge').textContent).toMatch(/ · Boost$/);
  });

  it('hides the line for a stale or failed check', () => {
    carSlots.value = { ...dispatches([{ start: '2026-11-01T23:30:00Z', end: '2026-11-02T05:30:00Z' }]), checked_at: '2026-11-01T17:00:00Z' };
    const { unmount } = render(<HouseStatusCards snapshot={snap()} now={NOW} />);
    expect(screen.queryByTestId('house-car-charge')).toBeNull();
    unmount();
    carSlots.value = { ...dispatches([{ start: '2026-11-01T23:30:00Z', end: '2026-11-02T05:30:00Z' }]), error: 'Octopus is down' };
    render(<HouseStatusCards snapshot={snap()} now={NOW} />);
    expect(screen.queryByTestId('house-car-charge')).toBeNull();
  });

  it('still shows the car on a Gateway, where the schedule is unavailable', () => {
    carSlots.value = dispatches([{ start: '2026-11-01T23:30:00Z', end: '2026-11-02T05:30:00Z' }]);
    render(<HouseStatusCards snapshot={snap({ device_type_code: '7001' })} now={NOW} />);
    expect(tid('house-charge-card').getAttribute('data-state')).toBe('unavailable');
    expect(tid('house-charge-card').contains(tid('house-car-charge'))).toBe(true);
  });

  it('waits for the car check as well as the schedule, so the card does not jump', () => {
    carSlots.loaded = false;
    render(<HouseStatusCards snapshot={snap()} now={NOW} />);
    expect(tid('house-charge-card').getAttribute('data-state')).toBe('loading');
  });
});

describe('<HouseStatusCards/> battery runtime line (issue #472)', () => {
  const NOW = new Date('2026-11-01T18:00:00Z');
  const local = (iso: string) => {
    const d = new Date(iso);
    return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
  };
  beforeEach(() => {
    timedExport.value = null;
    timedExport.loaded = true;
    carSlots.value = null;
    runtime.value = null;
    useInverterStore.setState({ visualNoiseThreshold: 20 });
  });
  afterEach(cleanup);

  it('shows no line while off', () => {
    render(<HouseStatusCards snapshot={snap()} now={NOW} />);
    expect(screen.queryByTestId('house-battery-runtime')).toBeNull();
  });

  it('says when the battery reaches its reserve, with a confidence in dots', () => {
    const at = Date.parse('2026-11-01T18:30:00Z') / 1000;
    runtime.value = { outcome: 'empty_at', at, earliest: at - 1800, latest: at + 3600, confidence: 'medium', computed_at: at - 1800 };
    render(<HouseStatusCards snapshot={snap()} now={NOW} />);
    const line = tid('house-battery-runtime');
    expect(tid('house-battery-card').contains(line)).toBe(true);
    expect(tid('house-runtime-text').textContent).toMatch(new RegExp(`^Empty (at|tomorrow at) ${local('2026-11-01T18:30:00Z')}$`));
    const dots = [...line.querySelectorAll('[data-testid="house-runtime-dot"]')];
    expect(dots).toHaveLength(3);
    expect(dots.filter((d) => d.getAttribute('data-lit') === 'true')).toHaveLength(2);
    expect(line.getAttribute('title')).toMatch(/medium confidence$/);
    // The dots aren't the only signal, and an ⓘ says what they mean.
    expect(tid('house-runtime-dots').getAttribute('aria-label')).toBe('Medium confidence');
    const info = screen.getByRole('button', { name: 'More about Battery Runtime' });
    expect(line.contains(info)).toBe(true);
    // It stays out of the schedule card, which holds the car line (#474).
    expect(tid('house-charge-card').contains(line)).toBe(false);
  });

  it('says it cannot calculate while charging faster than forecast, with an ⓘ', () => {
    runtime.value = { outcome: 'charging', computed_at: Date.parse('2026-11-01T17:58:00Z') / 1000 };
    render(<HouseStatusCards snapshot={snap()} now={NOW} />);
    expect(tid('house-runtime-text').textContent).toBe('Unable to calculate: charging faster than forecast');
    expect(screen.queryByTestId('house-runtime-dots')).toBeNull();
    expect(tid('house-battery-runtime').contains(screen.getByRole('button', { name: 'More about Battery Runtime' }))).toBe(true);
  });

  it('shows a refill without dots', () => {
    const at = Date.parse('2026-11-01T23:30:00Z') / 1000;
    runtime.value = { outcome: 'recharges_first', at, by: 'charge', computed_at: Date.parse('2026-11-01T17:58:00Z') / 1000 };
    render(<HouseStatusCards snapshot={snap()} now={NOW} />);
    expect(tid('house-runtime-text').textContent).toMatch(/^Lasts until the .* charge$/);
    expect(screen.queryByTestId('house-runtime-dots')).toBeNull();
    expect(screen.queryByRole('button', { name: 'More about Battery Runtime' })).toBeNull();
  });
});
