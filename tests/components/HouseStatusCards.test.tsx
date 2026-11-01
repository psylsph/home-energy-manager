import { describe, it, expect, afterEach, beforeEach, vi } from 'vitest';
import { render, screen, cleanup, act } from '@testing-library/react';

const timedExport = vi.hoisted(() => ({
  value: null as null | { schedule_enabled: boolean; slots: unknown[] },
  loaded: true,
}));
vi.mock('../../src/hooks/useTimedExportSchedule', () => ({
  useTimedExportSchedule: () => ({ schedule: timedExport.value, loaded: timedExport.loaded }),
}));

import HouseStatusCards from '../../src/components/house/HouseStatusCards';
import { socColor } from '../../src/lib/energyFlow';
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

  it('says Charge Window, without pulsing, inside a slot when the battery is not charging', () => {
    render(<HouseStatusCards snapshot={snap({ inverter_time: '2026-11-01 01:00:00', battery_state: 'idle' })} />);
    expect(screen.getByText('Charge Window')).toBeDefined();
    expect(tid('house-charge-icon').getAttribute('data-pulsing')).toBe('false');
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

  it('says All day when the merged slots cover the whole day', () => {
    render(<HouseStatusCards snapshot={snap({ charge_slots: [slot(0, 0, 12, 0), slot(12, 0, 0, 0)] })} />);
    expect(tid('house-charge-card').getAttribute('data-state')).toBe('active');
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

  it('says Charge Window for a few watts of noise inside the window', () => {
    render(<HouseStatusCards snapshot={snap({ inverter_time: '2026-11-01 01:00:00', battery_state: 'charging', battery_power: -5 })} />);
    expect(screen.getByText('Charge Window')).toBeDefined();
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
    expect(segments).toHaveLength(10);
    const lit = segments.filter((s) => s.getAttribute('data-lit') === 'true');
    expect(lit).toHaveLength(3);
    for (const s of lit) expect(s.style.backgroundColor).toBe(css(socColor(33.5)));
    expect(segments[9].style.backgroundColor).toBe('var(--color-bg-elevated)');
  });

  it('shows a dash when the capacity is unknown', () => {
    render(<HouseStatusCards snapshot={snap({ battery_capacity_kwh: 0, soc: 0 })} />);
    expect(tid('house-battery-stored').textContent).toBe('—');
    expect(screen.getAllByTestId('house-battery-segment').filter((s) => s.getAttribute('data-lit') === 'true')).toHaveLength(0);
  });
});
