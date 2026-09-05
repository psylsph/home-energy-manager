import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { LatestVersionInfo } from '../../src/lib/types';

/**
 * Coverage for src/store/useInverterStore.ts actions that the existing
 * useInverterStore.test.ts / *.evc.test.ts / *.gridLineWeight.test.ts /
 * *.readOnly.test.ts files leave untested. Each describe block targets a
 * setter group and verifies both state mutation and localStorage persistence.
 */

describe('useInverterStore — under-tested actions', () => {
  beforeEach(() => {
    useInverterStore.setState({
      themeMode: 'dark',
      readOnly: false,
      hiddenPanels: [],
      inverterTempConfig: { inverter_temp_min: 8, inverter_temp_max: 60 },
      panelGraphsEnabled: true,
      panelGraphsScale: 'today',
      panelGraphsYLock: true,
      panelGraphsYLockMax: 0,
      visualNoiseThreshold: 20,
      gridMeterAddress: 0,
      latestVersionInfo: null,
      dismissedUpdateVersion: null,
      reconnectRequestedAt: null,
      evcHost: '',
      pendingDischargeSlots: {},
      connectedSince: null,
      lastConnectedDurationSec: null,
      connectFailures: 0,
    });
    localStorage.clear();
  });

  afterEach(() => {
    localStorage.clear();
  });

  describe('markReconnectRequested', () => {
    it('stamps the provided timestamp into reconnectRequestedAt', () => {
      useInverterStore.getState().markReconnectRequested(123456789);
      expect(useInverterStore.getState().reconnectRequestedAt).toBe(123456789);
    });

    it('overwrites a previous timestamp on a subsequent call', () => {
      useInverterStore.getState().markReconnectRequested(1);
      useInverterStore.getState().markReconnectRequested(2);
      expect(useInverterStore.getState().reconnectRequestedAt).toBe(2);
    });
  });

  describe('setThemeMode', () => {
    it('updates themeMode and persists to localStorage', () => {
      useInverterStore.getState().setThemeMode('light');
      expect(useInverterStore.getState().themeMode).toBe('light');
      expect(localStorage.getItem('themeMode')).toBe('light');
    });

    it('switches back to dark and persists', () => {
      useInverterStore.getState().setThemeMode('light');
      useInverterStore.getState().setThemeMode('dark');
      expect(useInverterStore.getState().themeMode).toBe('dark');
      expect(localStorage.getItem('themeMode')).toBe('dark');
    });
  });

  describe('setReadOnly', () => {
    it('updates readOnly and persists to sessionStorage', () => {
      useInverterStore.getState().setReadOnly(true);
      expect(useInverterStore.getState().readOnly).toBe(true);
      expect(sessionStorage.getItem('readOnly')).toBe('true');
    });

    it('removes the obsolete localStorage readOnly key when toggled', () => {
      localStorage.setItem('readOnly', 'true');
      useInverterStore.getState().setReadOnly(false);
      expect(localStorage.getItem('readOnly')).toBeNull();
    });
  });

  describe('setHiddenPanels', () => {
    it('replaces the hidden panels list', () => {
      useInverterStore.getState().setHiddenPanels(['solar', 'history']);
      expect(useInverterStore.getState().hiddenPanels).toEqual(['solar', 'history']);
    });

    it('clears the list when passed an empty array', () => {
      useInverterStore.getState().setHiddenPanels(['solar']);
      useInverterStore.getState().setHiddenPanels([]);
      expect(useInverterStore.getState().hiddenPanels).toEqual([]);
    });
  });

  describe('setInverterTempConfig', () => {
    it('replaces the temperature alert config', () => {
      useInverterStore.getState().setInverterTempConfig({ inverter_temp_min: 5, inverter_temp_max: 55 });
      expect(useInverterStore.getState().inverterTempConfig).toEqual({
        inverter_temp_min: 5,
        inverter_temp_max: 55,
      });
    });
  });

  describe('setEvcHost', () => {
    it('stores the EVC host string', () => {
      useInverterStore.getState().setEvcHost('192.168.1.50');
      expect(useInverterStore.getState().evcHost).toBe('192.168.1.50');
    });

    it('accepts an empty string', () => {
      useInverterStore.getState().setEvcHost('');
      expect(useInverterStore.getState().evcHost).toBe('');
    });
  });

  describe('setPanelGraphsEnabled', () => {
    it('toggles panelGraphsEnabled and persists', () => {
      useInverterStore.getState().setPanelGraphsEnabled(false);
      expect(useInverterStore.getState().panelGraphsEnabled).toBe(false);
      expect(localStorage.getItem('panelGraphsEnabled')).toBe('false');
    });

    it('toggles back to true and persists', () => {
      useInverterStore.getState().setPanelGraphsEnabled(false);
      useInverterStore.getState().setPanelGraphsEnabled(true);
      expect(useInverterStore.getState().panelGraphsEnabled).toBe(true);
      expect(localStorage.getItem('panelGraphsEnabled')).toBe('true');
    });
  });

  describe('setPanelGraphsScale', () => {
    it('switches to 24h and persists', () => {
      useInverterStore.getState().setPanelGraphsScale('24h');
      expect(useInverterStore.getState().panelGraphsScale).toBe('24h');
      expect(localStorage.getItem('panelGraphsScale')).toBe('24h');
    });

    it('switches back to today and persists', () => {
      useInverterStore.getState().setPanelGraphsScale('24h');
      useInverterStore.getState().setPanelGraphsScale('today');
      expect(useInverterStore.getState().panelGraphsScale).toBe('today');
      expect(localStorage.getItem('panelGraphsScale')).toBe('today');
    });
  });

  describe('setPanelGraphsYLock', () => {
    it('updates panelGraphsYLock and resets the Y-lock max to 0', () => {
      useInverterStore.setState({ panelGraphsYLockMax: 5000 });
      useInverterStore.getState().setPanelGraphsYLock(false);
      expect(useInverterStore.getState().panelGraphsYLock).toBe(false);
      expect(useInverterStore.getState().panelGraphsYLockMax).toBe(0);
    });

    it('persists to localStorage', () => {
      useInverterStore.getState().setPanelGraphsYLock(false);
      expect(localStorage.getItem('panelGraphsYLock')).toBe('false');
    });
  });

  describe('setPanelGraphsYLockMax', () => {
    it('stores the new max', () => {
      useInverterStore.getState().setPanelGraphsYLockMax(4200);
      expect(useInverterStore.getState().panelGraphsYLockMax).toBe(4200);
    });

    it('does not persist to localStorage (session-only tracking)', () => {
      useInverterStore.getState().setPanelGraphsYLockMax(4200);
      expect(localStorage.getItem('panelGraphsYLockMax')).toBeNull();
    });
  });

  describe('setVisualNoiseThreshold', () => {
    it('updates the threshold and persists', () => {
      useInverterStore.getState().setVisualNoiseThreshold(35);
      expect(useInverterStore.getState().visualNoiseThreshold).toBe(35);
      expect(localStorage.getItem('visualNoiseThreshold')).toBe('35');
    });
  });

  describe('setGridMeterAddress', () => {
    it('updates the address and persists', () => {
      useInverterStore.getState().setGridMeterAddress(3);
      expect(useInverterStore.getState().gridMeterAddress).toBe(3);
      expect(localStorage.getItem('gridMeterAddress')).toBe('3');
    });
  });

  describe('setLatestVersionInfo', () => {
    it('stores the version info object', () => {
      const info: LatestVersionInfo = {
        current_version: '1.2.2',
        latest_version: '1.2.3',
        release_url: 'https://example.com/release',
        update_available: true,
      };
      useInverterStore.getState().setLatestVersionInfo(info);
      expect(useInverterStore.getState().latestVersionInfo).toEqual(info);
    });

    it('clears the info when passed null', () => {
      useInverterStore.getState().setLatestVersionInfo({
        current_version: '1.2.2',
        latest_version: '1.2.3',
        release_url: 'https://example.com/release',
        update_available: true,
      });
      useInverterStore.getState().setLatestVersionInfo(null);
      expect(useInverterStore.getState().latestVersionInfo).toBeNull();
    });
  });

  describe('dismissUpdateVersion', () => {
    it('stores the dismissed version and persists', () => {
      useInverterStore.getState().dismissUpdateVersion('1.2.3');
      expect(useInverterStore.getState().dismissedUpdateVersion).toBe('1.2.3');
      expect(localStorage.getItem('hem_dismissed_update_version')).toBe('1.2.3');
    });
  });

  describe('setPendingDischargeSlots / clearPendingDischargeSlots', () => {
    it('stores slots and persists to localStorage', () => {
      const slots = {
        0: { enabled: true, start_hour: 0, start_minute: 0, end_hour: 4, end_minute: 0, target_soc: 100 },
      };
      useInverterStore.getState().setPendingDischargeSlots(slots);
      expect(useInverterStore.getState().pendingDischargeSlots).toEqual(slots);
      expect(localStorage.getItem('pendingDischargeSlots')).not.toBeNull();
    });

    it('clearPendingDischargeSlots empties state and storage', () => {
      useInverterStore.getState().setPendingDischargeSlots({
        1: { enabled: true, start_hour: 5, start_minute: 0, end_hour: 6, end_minute: 0, target_soc: 80 },
      });
      useInverterStore.getState().clearPendingDischargeSlots();
      expect(useInverterStore.getState().pendingDischargeSlots).toEqual({});
      expect(localStorage.getItem('pendingDischargeSlots')).toBe('{}');
    });
  });
});

describe('useInverterStore — setConnection edge cases', () => {
  beforeEach(() => {
    useInverterStore.setState({
      connectionState: 'disconnected',
      connectedHost: null,
      connectedSince: null,
      lastConnectedDurationSec: null,
      connectFailures: 0,
    });
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2025-01-01T12:00:00Z').getTime());
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('sets connectedSince to Date.now() when connected without explicit timestamp', () => {
    const before = Date.now();
    useInverterStore.getState().setConnection('connected', '10.0.0.1');
    const after = Date.now();
    const since = useInverterStore.getState().connectedSince;
    expect(since).toBeGreaterThanOrEqual(before);
    expect(since).toBeLessThanOrEqual(after);
  });

  it('uses the explicit connectedSince when provided', () => {
    useInverterStore.getState().setConnection('connected', '10.0.0.1', 999);
    expect(useInverterStore.getState().connectedSince).toBe(999);
  });

  it('nulls connectedSince on reconnecting', () => {
    useInverterStore.getState().setConnection('connected', '10.0.0.1', 1000);
    useInverterStore.getState().setConnection('reconnecting');
    expect(useInverterStore.getState().connectedSince).toBeNull();
  });

  it('nulls connectedSince on disconnected', () => {
    useInverterStore.getState().setConnection('connected', '10.0.0.1', 1000);
    useInverterStore.getState().setConnection('disconnected');
    expect(useInverterStore.getState().connectedSince).toBeNull();
  });

  it('nulls connectedHost when host arg is omitted', () => {
    useInverterStore.getState().setConnection('connected', '10.0.0.1');
    useInverterStore.getState().setConnection('disconnected');
    expect(useInverterStore.getState().connectedHost).toBeNull();
  });

  it('preserves connectFailures on reconnecting (only resets on connected)', () => {
    useInverterStore.setState({ connectFailures: 7 });
    useInverterStore.getState().setConnection('reconnecting');
    expect(useInverterStore.getState().connectFailures).toBe(7);
  });
});

describe('useInverterStore — localStorage persistence helpers', () => {
  afterEach(() => {
    localStorage.clear();
    vi.restoreAllMocks();
  });

  it('setDeveloperMode persists under the devMode key', () => {
    useInverterStore.getState().setDeveloperMode(true);
    expect(localStorage.getItem('devMode')).toBe('true');
  });

  it('setChartRange persists the range under chartRange', () => {
    useInverterStore.getState().setChartRange('30d');
    expect(localStorage.getItem('chartRange')).toBe('30d');
  });

  it('persisting a value survives a store reload via vi.resetModules', async () => {
    // Write a value through the public setter, then re-import the module to
    // confirm the load* helpers pick it up from localStorage.
    useInverterStore.getState().setVisualNoiseThreshold(42);
    expect(localStorage.getItem('visualNoiseThreshold')).toBe('42');

    vi.resetModules();
    const { useInverterStore: freshStore } = await import('../../src/store/useInverterStore');
    expect(freshStore.getState().visualNoiseThreshold).toBe(42);
  });

  it('an out-of-range visualNoiseThreshold in storage falls back to 20', async () => {
    localStorage.setItem('visualNoiseThreshold', 'NaN-ish');
    vi.resetModules();
    const { useInverterStore: freshStore } = await import('../../src/store/useInverterStore');
    expect(freshStore.getState().visualNoiseThreshold).toBe(20);
  });

  it('a negative gridMeterAddress in storage falls back to 0', async () => {
    localStorage.setItem('gridMeterAddress', '-5');
    vi.resetModules();
    const { useInverterStore: freshStore } = await import('../../src/store/useInverterStore');
    expect(freshStore.getState().gridMeterAddress).toBe(0);
  });

  it('a valid gridMeterAddress in storage is loaded', async () => {
    localStorage.setItem('gridMeterAddress', '7');
    vi.resetModules();
    const { useInverterStore: freshStore } = await import('../../src/store/useInverterStore');
    expect(freshStore.getState().gridMeterAddress).toBe(7);
  });

  it('an unrecognised chartRange in storage falls back to 24h', async () => {
    localStorage.setItem('chartRange', '3h');
    vi.resetModules();
    const { useInverterStore: freshStore } = await import('../../src/store/useInverterStore');
    expect(freshStore.getState().chartRange).toBe('24h');
  });

  it('a valid chartRange in storage is loaded', async () => {
    localStorage.setItem('chartRange', '7d');
    vi.resetModules();
    const { useInverterStore: freshStore } = await import('../../src/store/useInverterStore');
    expect(freshStore.getState().chartRange).toBe('7d');
  });
});
