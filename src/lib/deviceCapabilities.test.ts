import { describe, it, expect } from 'vitest';
import { deviceSupportsEps } from './deviceCapabilities';

/**
 * The EPS-supporting set mirrors the backend's `DeviceType::supports_eps`
 * (see `src-tauri/src/inverter/model.rs`) and the givenergy-modbus reference
 * library's `_AC_CONFIG_BLOCK_MODELS = {AC, AC_3PH, ALL_IN_ONE}`.
 */
describe('deviceSupportsEps', () => {
  describe('supported device families', () => {
    it.each([
      ['3001', 'AC-coupled (legacy)'],
      ['3002', 'AC-coupled Mk2'],
      ['6001', 'AC three-phase (low)'],
      ['60AB', 'AC three-phase (any 60xx)'],
      ['8001', 'AIO 6kW'],
      ['8002', 'AIO 3.6kW'],
      ['8003', 'AIO 5kW'],
      ['80FF', 'AIO family (any 80xx)'],
    ])('returns true for %s (%s)', (code) => {
      expect(
        deviceSupportsEps({ device_type_code: code } as never),
      ).toBe(true);
    });
  });

  describe('unsupported device families', () => {
    it.each([
      ['1001', 'Gen1 hybrid'],
      ['2001', 'Gen hybrid (pre-ARM-refined)'],
      ['2101', 'Polar hybrid'],
      ['2201', 'Gen3+ hybrid'],
      ['4001', 'Three-phase'],
      ['4101', 'AIO Commercial'],
      ['5001', 'EMS'],
      ['5101', 'EMS Commercial'],
      ['7001', 'Gateway'],
      ['8101', 'Hybrid HV Gen3'],
      ['8201', 'AIO Hybrid'],
      ['8301', 'Gen4 hybrid'],
      ['2301', 'PV inverter'],
    ])('returns false for %s (%s)', (code) => {
      expect(
        deviceSupportsEps({ device_type_code: code } as never),
      ).toBe(false);
    });
  });

  it('returns false when device_type_code is missing', () => {
    expect(deviceSupportsEps(null)).toBe(false);
    expect(deviceSupportsEps(undefined)).toBe(false);
    expect(deviceSupportsEps({} as never)).toBe(false);
  });
});