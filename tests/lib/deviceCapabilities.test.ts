import { describe, it, expect } from 'vitest';
import { deviceSupportsEps, deviceSupportsTimedDischarge } from '../../src/lib/deviceCapabilities';

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

/**
 * The Timed-Discharge-supporting set mirrors the backend's
 * `DeviceType::supports_timed_discharge` and the givenergy-modbus reference
 * library's `_AC_CONFIG_BLOCK_MODELS = {AC, AC_3PH, ALL_IN_ONE}` — the pause
 * registers (HR318-320) live in the same HR 300-359 block as EPS (HR317).
 */
describe('deviceSupportsTimedDischarge', () => {
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
        deviceSupportsTimedDischarge({ device_type_code: code } as never),
      ).toBe(true);
    });
  });

  describe('unsupported device families', () => {
    it.each([
      ['1001', 'Gen1 hybrid (reported case)'],
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
        deviceSupportsTimedDischarge({ device_type_code: code } as never),
      ).toBe(false);
    });
  });

  describe('Gen3 Hybrid firmware-gated targeted probe', () => {
    // Gen3 Hybrid (device code 0x20xx, ARM fw century 3) reaches the pause
    // registers via a targeted 3-register probe, enabled only at ARM fw >= 312.
    it.each([
      ['2001', '312'],
      ['2001', '318'],
      ['2001', '399'],
      ['2003', '350'],
    ])(
      'returns true for Gen3 code %s at ARM fw %s',
      (code, fw) => {
        expect(
          deviceSupportsTimedDischarge({
            device_type_code: code,
            firmware_version: fw,
          } as never),
        ).toBe(true);
      },
    );

    it.each([
      ['2001', '300', 'below threshold'],
      ['2001', '311', 'just below threshold'],
      ['2001', '', 'no firmware reported'],
      ['2001', 'garbage', 'unparseable firmware'],
      // Gen2 shares the 0x20xx prefix (ARM fw century 8/9); must NOT qualify
      // even at high firmware, since it's a different generation.
      ['2001', '812', 'Gen2 firmware century'],
      ['2001', '449', 'Gen1 firmware century'],
    ])('returns false for code %s at ARM fw %s (%s)', (code, fw) => {
      expect(
        deviceSupportsTimedDischarge({
          device_type_code: code,
          firmware_version: fw,
        } as never),
      ).toBe(false);
    });
  });

  it('returns false when device_type_code is missing', () => {
    expect(deviceSupportsTimedDischarge(null)).toBe(false);
    expect(deviceSupportsTimedDischarge(undefined)).toBe(false);
    expect(deviceSupportsTimedDischarge({} as never)).toBe(false);
  });

  it('matches deviceSupportsEps on every supported and unsupported code', () => {
    // HR317 (EPS) and HR318-320 (pause / Timed Discharge) share the same
    // HR 300-359 block, so the two predicates must agree on every device
    // code. Locking that here means a future divergence is an intentional,
    // reviewed change rather than a drift.
    const codes = [
      '1001', '2001', '2101', '2201', '2301',
      '3001', '3002', '4001', '4101', '5001',
      '5101', '6001', '7001', '8001', '8002',
      '8003', '8101', '8201', '8301',
    ];
    for (const code of codes) {
      const snap = { device_type_code: code } as never;
      expect(deviceSupportsTimedDischarge(snap)).toBe(deviceSupportsEps(snap));
    }
  });
});
