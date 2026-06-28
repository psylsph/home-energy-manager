import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/react';
import BatteryModeSummary from '../../src/components/BatteryModeSummary';
import type { InverterSnapshot, ScheduleSlot } from '../../src/lib/types';

function slot(
  startHour: number,
  startMinute: number,
  endHour: number,
  endMinute: number,
  enabled = true,
): ScheduleSlot {
  return {
    enabled,
    start_hour: startHour,
    start_minute: startMinute,
    end_hour: endHour,
    end_minute: endMinute,
    target_soc: 100,
  };
}

function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    battery_state: 'idle',
    battery_power_mode: 0,
    enable_charge: false,
    enable_discharge: false,
    charge_slots: [],
    discharge_slots: [],
    ...overrides,
  } as InverterSnapshot;
}

describe('<BatteryModeSummary/>', () => {
  it('renders all four mechanism rows', () => {
    const { container } = render(
      <BatteryModeSummary snapshot={makeSnapshot()} now={new Date('2026-06-28T12:00:00')} />,
    );
    expect(container.textContent).toContain('Eco');
    expect(container.textContent).toContain('Timed Charge');
    expect(container.textContent).toContain('Timed Export');
    expect(container.textContent).toContain('Timed Discharge');
  });

  it('marks every mechanism as off when all registers are clear', () => {
    const { container } = render(
      <BatteryModeSummary snapshot={makeSnapshot()} now={new Date('2026-06-28T12:00:00')} />,
    );
    for (const key of ['eco', 'timed_charge', 'timed_export', 'timed_discharge']) {
      const row = container.querySelector(`[data-testid="battery-mode-${key}"]`);
      expect(row).not.toBeNull();
      expect(row!.getAttribute('data-state')).toBe('off');
    }
  });

  it('renders Eco as active when self-consumption is enabled', () => {
    const { container } = render(
      <BatteryModeSummary
        snapshot={makeSnapshot({ battery_power_mode: 1 })}
        now={new Date('2026-06-28T12:00:00')}
      />,
    );
    const row = container.querySelector('[data-testid="battery-mode-eco"]');
    expect(row?.getAttribute('data-state')).toBe('active');
    expect(row?.textContent).toContain('on');
  });

  it('renders an active Timed Export row with the exporting-now suffix', () => {
    const { container } = render(
      <BatteryModeSummary
        snapshot={makeSnapshot({
          enable_discharge: true,
          discharge_slots: [slot(16, 0, 19, 0)],
          battery_state: 'discharging',
        })}
        now={new Date('2026-06-28T17:00:00')}
      />,
    );
    const row = container.querySelector('[data-testid="battery-mode-timed_export"]');
    expect(row?.getAttribute('data-state')).toBe('active');
    expect(row?.textContent).toContain('armed · exporting now');
  });

  it('renders an active Timed Discharge row with the covering-demand suffix', () => {
    const { container } = render(
      <BatteryModeSummary
        snapshot={makeSnapshot({
          battery_pause_mode: 2,
          battery_pause_slot: slot(3, 0, 4, 0),
          battery_state: 'discharging',
        })}
        now={new Date('2026-06-28T03:30:00')}
      />,
    );
    const row = container.querySelector('[data-testid="battery-mode-timed_discharge"]');
    expect(row?.getAttribute('data-state')).toBe('active');
    expect(row?.textContent).toContain('armed · covering demand now');
  });

  it('keeps armed-but-inactive mechanisms in the armed state', () => {
    const { container } = render(
      <BatteryModeSummary
        snapshot={makeSnapshot({
          enable_charge: true,
          charge_slots: [slot(2, 0, 4, 0)],
        })}
        now={new Date('2026-06-28T12:00:00')}
      />,
    );
    const row = container.querySelector('[data-testid="battery-mode-timed_charge"]');
    expect(row?.getAttribute('data-state')).toBe('armed');
    expect(row?.textContent).toContain('armed');
    expect(row?.textContent).not.toContain('charging now');
  });

  it('uses the injected `now` for deterministic window tests', () => {
    const snapshot = makeSnapshot({
      enable_discharge: true,
      discharge_slots: [slot(16, 0, 19, 0)],
      battery_state: 'discharging',
    });
    const { container: inWindow } = render(
      <BatteryModeSummary snapshot={snapshot} now={new Date('2026-06-28T17:00:00')} />,
    );
    expect(
      inWindow.querySelector('[data-testid="battery-mode-timed_export"]')?.getAttribute('data-state'),
    ).toBe('active');

    const { container: outWindow } = render(
      <BatteryModeSummary snapshot={snapshot} now={new Date('2026-06-28T12:00:00')} />,
    );
    expect(
      outWindow
        .querySelector('[data-testid="battery-mode-timed_export"]')
        ?.getAttribute('data-state'),
    ).toBe('armed');
  });
});
