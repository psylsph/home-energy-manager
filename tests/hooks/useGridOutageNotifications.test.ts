import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, cleanup, act } from '@testing-library/react';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

// ---------------------------------------------------------------------------
// useGridOutageNotifications fires OS-level Notification popups when the
// snapshot transitions into / out of a faulted state (grid loss, inverter
// trip, battery over-temp). We mock the Notification API and drive the
// Zustand store through fault onset → ongoing → recovery to exercise the
// classifyFault state machine and the one-shot / recovery notification logic.
// ---------------------------------------------------------------------------

const notificationInstances: { title: string; body: string }[] = [];

// Plain mock constructor + a configurable static `.permission` so the
// `canNotify()` guard (`Notification.permission === 'granted'`) can be
// flipped per test. Using defineProperty with configurable:true lets us
// redefine it in beforeEach without colliding with a class field.
function MockNotification(this: { close: () => void }, title: string, options?: NotificationOptions) {
  notificationInstances.push({ title, body: options?.body ?? '' });
  this.close = () => {};
}
MockNotification.requestPermission = vi.fn().mockResolvedValue('granted');

function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    timestamp: 0,
    solar_power: 0, pv1_power: 0, pv2_power: 0,
    pv1_voltage: 0, pv2_voltage: 0, pv1_current: 0, pv2_current: 0,
    battery_power: 0, soc: 50, battery_voltage: 50, battery_current: 0,
    battery_state: 'idle', battery_temperature: 20, battery_capacity_kwh: 9.5,
    eps_power_w: 0, grid_power: 0, grid_voltage: 230, grid_frequency: 50,
    grid_online: true, grid_loss: false, inverter_trip: false,
    battery_over_temp: false, home_power: 0, inverter_temperature: 30,
    inverter_time: '',
    today_solar_kwh: 0, today_pv1_kwh: 0, today_pv2_kwh: 0,
    today_import_kwh: 0, today_export_kwh: 0, today_charge_kwh: 0,
    total_import_kwh: 0, total_export_kwh: 0, total_solar_kwh: 0,
    total_charge_kwh: 0, total_discharge_kwh: 0, total_throughput_kwh: 0,
    operating_hours: 0, today_discharge_kwh: 0, today_consumption_kwh: 0,
    home_energy_today_kwh: 0, battery_modules: [], battery_mode: 'eco',
    battery_reserve: 4, charge_rate: 0, discharge_rate: 0, active_power_rate: 0,
    max_battery_power_w: 0, max_ac_power_w: 0, export_limit_w: 0, target_soc: 100,
    enable_charge_target: false, enable_charge: false, enable_discharge: false,
    auto_winter_active: false, load_limiter_active: false, cosy_active: false,
    cosy_enabled: false, agile_active: false, agile_state: 'idle', agile_enabled: false,
    max_charge_slots: 0, max_discharge_slots: 0, charge_slots: [], discharge_slots: [],
    meters: [], inverter_serial: '', firmware_version: '', dsp_firmware_version: '',
    dc_dsp_firmware_version: '', device_type: '', device_type_display: 'Gen 3 Hybrid',
    device_type_code: '2201', battery_calibration_stage: 0, enable_ammeter: false,
    enable_reversed_ct_clamp: false, meter_type: 0, supports_battery_calibration: false,
    ac_eps_enabled: false, ac_export_priority: 0,
    ...overrides,
  };
}

const { useGridOutageNotifications } = await import('../../src/hooks/useGridOutageNotifications');

function setSnapshot(snapshot: InverterSnapshot | null) {
  act(() => {
    useInverterStore.setState({ snapshot });
  });
}

describe('useGridOutageNotifications', () => {
  beforeEach(() => {
    cleanup();
    notificationInstances.length = 0;
    // Install the mock as the global Notification and grant permission.
    (globalThis as unknown as { Notification: typeof MockNotification }).Notification =
      MockNotification;
    Object.defineProperty(MockNotification, 'permission', {
      value: 'granted',
      configurable: true,
      writable: true,
    });
    useInverterStore.setState({ snapshot: null });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it('does nothing when there is no snapshot', () => {
    renderHook(() => useGridOutageNotifications());
    expect(notificationInstances).toHaveLength(0);
  });

  it('fires a notification on grid fault onset', () => {
      renderHook(() => useGridOutageNotifications());
    setSnapshot(makeSnapshot({ grid_loss: true, grid_online: false, soc: 40 }));
    expect(notificationInstances).toHaveLength(1);
    expect(notificationInstances[0].title).toBe('Grid power lost');
    expect(notificationInstances[0].body).toContain('No Utility');
    expect(notificationInstances[0].body).toContain('40%');
  });

  it('includes battery discharge rate in the body when discharging', () => {
    renderHook(() => useGridOutageNotifications());
    setSnapshot(
      makeSnapshot({ grid_loss: true, grid_online: false, battery_power: 1500 }),
    );
    expect(notificationInstances).toHaveLength(1);
    expect(notificationInstances[0].body).toContain('discharging');
    expect(notificationInstances[0].body).toContain('1.5kW');
  });

  it('fires a recovery notification when the grid comes back', () => {
      renderHook(() => useGridOutageNotifications());
    // Onset
    setSnapshot(makeSnapshot({ grid_loss: true, grid_online: false }));
    expect(notificationInstances).toHaveLength(1);
    // Recovery
    setSnapshot(makeSnapshot({ grid_loss: false, grid_online: true, grid_voltage: 241.3 }));
    expect(notificationInstances).toHaveLength(2);
    expect(notificationInstances[1].title).toBe('Grid power restored');
    expect(notificationInstances[1].body).toContain('241.3V');
  });

  it('does not re-notify while a fault is ongoing', () => {
    renderHook(() => useGridOutageNotifications());
    // First poll: fault onset → one notification.
    setSnapshot(makeSnapshot({ grid_loss: true, grid_online: false }));
    expect(notificationInstances).toHaveLength(1);
    // Second poll: still faulted (e.g. SOC changed) → no new notification.
    setSnapshot(makeSnapshot({ grid_loss: true, grid_online: false, soc: 30 }));
    expect(notificationInstances).toHaveLength(1);
    // Third poll: still faulted → still no new notification.
    setSnapshot(makeSnapshot({ grid_loss: true, grid_online: false, soc: 25 }));
    expect(notificationInstances).toHaveLength(1);
  });

  it('re-arms after recovery so a second outage notifies again', () => {
    renderHook(() => useGridOutageNotifications());
    // First outage
    setSnapshot(makeSnapshot({ grid_loss: true, grid_online: false }));
    expect(notificationInstances).toHaveLength(1);
    // Recover
    setSnapshot(makeSnapshot({ grid_loss: false, grid_online: true }));
    expect(notificationInstances).toHaveLength(2);
    // Second outage
    setSnapshot(makeSnapshot({ grid_loss: true, grid_online: false }));
    expect(notificationInstances).toHaveLength(3);
    expect(notificationInstances[2].title).toBe('Grid power lost');
  });

  it('fires a battery over-temp notification and recovery', () => {
    renderHook(() => useGridOutageNotifications());
    setSnapshot(makeSnapshot({ battery_over_temp: true }));
    expect(notificationInstances).toHaveLength(1);
    expect(notificationInstances[0].title).toBe('Battery over temperature');
    // Recover — battery temp path has its own recovery message.
    setSnapshot(makeSnapshot({ battery_over_temp: false }));
    expect(notificationInstances).toHaveLength(2);
    expect(notificationInstances[1].title).toBe('Battery temperature normal');
  });

  it('fires an inverter-trip notification and recovery', () => {
    renderHook(() => useGridOutageNotifications());
    setSnapshot(makeSnapshot({ inverter_trip: true }));
    expect(notificationInstances).toHaveLength(1);
    expect(notificationInstances[0].title).toBe('Inverter trip detected');
    // Recover — inverter trip reuses the grid/inverter recovery branch.
    setSnapshot(makeSnapshot({ inverter_trip: false }));
    expect(notificationInstances).toHaveLength(2);
    expect(notificationInstances[1].title).toBe('Grid power restored');
  });

  it('does not fire when permission is not granted', () => {
    Object.defineProperty(MockNotification, 'permission', {
      value: 'denied',
      configurable: true,
      writable: true,
    });
    renderHook(() => useGridOutageNotifications());
    setSnapshot(makeSnapshot({ grid_loss: true, grid_online: false }));
    expect(notificationInstances).toHaveLength(0);
  });
});
