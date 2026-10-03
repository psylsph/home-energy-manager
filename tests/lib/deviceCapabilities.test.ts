import { describe, it, expect } from 'vitest';
import fixture from '../fixtures/device-limit-matrix.json';
import {
  deviceSupportsEps,
  deviceSupportsExportLimit,
  deviceSupportsTimedDischarge,
  deviceUsesHr50ActivePowerRate,
  isAcCoupledDevice,
  isThreePhaseLimitModel,
  usesAcLimitRegisters,
  usesDirectChargeLimit,
} from '../../src/lib/deviceCapabilities';

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
      ['4101', 'unvalidated device family'],
      ['5001', 'EMS'],
      ['5101', 'unvalidated device family'],
      ['7001', 'Gateway'],
      ['8101', 'Hybrid HV Gen3 6kW (HR1105 write unverified)'],
      ['8102', 'Hybrid HV Gen3 8kW (HR1105 write unverified)'],
      ['8103', 'Hybrid HV Gen3 10kW (HR1105 write unverified)'],
      ['81FF', 'Hybrid HV Gen3 family (HR1105 write unverified)'],
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
 * AC-coupled classification. The backend (`model.rs` `DeviceType::from_register`)
 * maps 0x3001 -> ACCoupled, 0x3002 -> ACCoupledMk2, and falls back to
 * ACCoupled for any other 0x30xx code — so the whole 0x30xx family is
 * AC-coupled, not just the two listed codes.
 */
describe('isAcCoupledDevice', () => {
  it.each([
    ['3001', 'AC-coupled (legacy)'],
    ['3002', 'AC-coupled Mk2'],
    ['3050', 'unlisted 30xx falls back to ACCoupled in the backend'],
  ])('returns true for %s (%s)', (code) => {
    expect(isAcCoupledDevice(code)).toBe(true);
  });

  it.each([
    ['2001', 'Gen hybrid'],
    ['4001', 'three-phase'],
    ['6001', 'AC three-phase'],
    ['8001', 'AIO'],
  ])('returns false for %s (%s)', (code) => {
    expect(isAcCoupledDevice(code)).toBe(false);
  });

  it('returns false when the code is missing', () => {
    expect(isAcCoupledDevice(undefined)).toBe(false);
    expect(isAcCoupledDevice(null)).toBe(false);
    expect(isAcCoupledDevice('')).toBe(false);
  });
});

/**
 * Three-phase-bank charge/discharge limit register models: 0x40/60/81/82.
 *
 * The Gateway (0x70xx) is NOT one of them: it keeps its limits in the
 * single-phase AC-limit bank (HR 313/314), like GivTCP's
 * `set_battery_charge_limit_ac` routing.
 */
describe('isThreePhaseLimitModel', () => {
  it.each([
    ['4001', 'ThreePhase'],
    ['6001', 'AC three-phase'],
    ['8101', 'Hybrid HV Gen3'],
    ['8201', 'AIO Hybrid'],
  ])('returns true for %s (%s)', (code) => {
    expect(isThreePhaseLimitModel(code)).toBe(true);
  });

  it.each([
    ['7001', 'Gateway uses the AC-limit bank, not the three-phase bank'],
    ['3001', 'AC-coupled uses AC-config block, not three-phase bank'],
    ['2001', 'Gen hybrid'],
    ['4101', 'unvalidated device family'],
    ['8001', 'AIO'],
  ])('returns false for %s (%s)', (code) => {
    expect(isThreePhaseLimitModel(code)).toBe(false);
  });

  it('returns false when the code is missing', () => {
    expect(isThreePhaseLimitModel(undefined)).toBe(false);
    expect(isThreePhaseLimitModel(null)).toBe(false);
  });
});

/**
 * usesDirectChargeLimit = AC-coupled OR three-phase-limit families: the
 * charge/discharge power limit registers are already 1-100% for these.
 */
describe('usesDirectChargeLimit', () => {
  it.each([
    ['3001', 'AC-coupled (legacy)'],
    ['3002', 'AC-coupled Mk2'],
    ['3050', 'unlisted 30xx AC-coupled'],
    ['4001', 'ThreePhase'],
    ['6001', 'AC three-phase'],
    ['7001', 'Gateway (HR 313/314)'],
    ['8101', 'Hybrid HV Gen3'],
    ['8201', 'AIO Hybrid'],
  ])('returns true for %s (%s)', (code) => {
    expect(usesDirectChargeLimit(code)).toBe(true);
  });

  it.each([
    ['1001', 'Gen1 hybrid'],
    ['2001', 'Gen hybrid (HR111/112 are 0-50)'],
    ['4101', 'unvalidated device family'],
    ['8001', 'AIO'],
    ['8301', 'Gen4 hybrid'],
  ])('returns false for %s (%s)', (code) => {
    expect(usesDirectChargeLimit(code)).toBe(false);
  });

  it('returns false when the code is missing', () => {
    expect(usesDirectChargeLimit(undefined)).toBe(false);
    expect(usesDirectChargeLimit(null)).toBe(false);
  });
});

/**
 * Issue #346: the Inverter Active Power Limit is only offered where HEM both
 * writes and reads back HR 50. `decode_holding_1000_1079` overwrites
 * `active_power_rate` with HR 1002 on every 1000-range-layout device, and the
 * write path has no matching branch (HR 1002 is not in `SAFE_WRITE_REGS` and
 * neither reference library ships a setter for it), so on those families the
 * slider wrote a register the layout ignores and then read the old value back.
 */
describe('deviceUsesHr50ActivePowerRate', () => {
  it.each([
    ['2001', 'Gen hybrid'],
    ['3001', 'AC-coupled'],
    ['8001', 'AIO 6kW'],
    ['8301', 'Gen4 hybrid'],
    ['7001', 'Gateway keeps the single-phase HR50 layout'],
    ['5001', 'EMS'],
    ['2301', 'PV inverter'],
  ])('returns true for %s (%s)', (code) => {
    expect(deviceUsesHr50ActivePowerRate(code)).toBe(true);
  });

  it.each([
    ['4001', 'ThreePhase reads HR1002'],
    ['6001', 'AC three-phase reads HR1002'],
    ['8101', 'Hybrid HV Gen3 reads HR1002'],
    ['8201', 'AIO Hybrid reads HR1002'],
    ['4101', 'unsupported commercial AIO'],
    ['5101', 'unsupported commercial EMS'],
    ['9999', 'unknown family'],
  ])('returns false for %s (%s)', (code) => {
    expect(deviceUsesHr50ActivePowerRate(code)).toBe(false);
  });

  it('returns false when the code is missing', () => {
    // No code means no confirmed register layout, so the control stays hidden
    // rather than flashing a slider the backend would reject.
    expect(deviceUsesHr50ActivePowerRate(undefined)).toBe(false);
    expect(deviceUsesHr50ActivePowerRate(null)).toBe(false);
  });
});

describe('deviceSupportsExportLimit', () => {
  it.each(['4101', '41FF', '5101', '51FF'])(
    'does not expose controls for unvalidated device code %s',
    (code) => {
      expect(
        deviceSupportsExportLimit({ device_type_code: code } as never),
      ).toBe(false);
    },
  );
});

/**
 * The Timed-Discharge-supporting set mirrors the backend's
 * `DeviceType::supports_timed_discharge`. AC-coupled models expose the
 * AC-config block for EPS, but field logs show their HR319/320 Timed
 * Discharge slot writes are rejected, so this predicate intentionally
 * diverges from `deviceSupportsEps` for 3001/3002.
 */
describe('deviceSupportsTimedDischarge', () => {
  describe('supported device families', () => {
    it.each([
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
      ['3001', 'AC-coupled (HR319/320 rejected in field logs)'],
      ['3002', 'AC-coupled Mk2 (HR319/320 gated until confirmed)'],
      ['2101', 'Polar hybrid'],
      ['2201', 'Gen3+ hybrid'],
      ['4001', 'Three-phase'],
      ['4101', 'unvalidated device family'],
      ['5001', 'EMS'],
      ['5101', 'unvalidated device family'],
      ['7001', 'Gateway'],
      ['8101', 'Hybrid HV Gen3 6kW'],
      ['8102', 'Hybrid HV Gen3 8kW'],
      ['8103', 'Hybrid HV Gen3 10kW'],
      ['81FF', 'Hybrid HV Gen3 family'],
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

  it('diverges from EPS for legacy AC-coupled models whose HR319/320 writes fail', () => {
    for (const code of ['3001', '3002']) {
      const snap = { device_type_code: code } as never;
      expect(deviceSupportsEps(snap)).toBe(true);
      expect(deviceSupportsTimedDischarge(snap)).toBe(false);
    }
  });
});

/**
 * Issue #346: the three predicates above must agree with the backend for every
 * device family, because together they decide which register the slider writes
 * and what scale it displays. The table mirrors `DeviceType` in
 * `src-tauri/src/inverter/model.rs`; the Rust
 * `every_device_type_decodes_the_charge_limit_from_the_register_it_writes` and
 * `every_device_type_reads_active_power_rate_from_the_register_it_writes`
 * tests assert the other half of the contract against the actual poll blocks.
 *
 * `direct` -> the limit register is already 1-100 (HR 313/314, HR 1108/1110).
 * `hr50`   -> the Inverter Active Power Limit is valid (HEM reads and writes
 *             HR 50); false where the decoder overwrites it with HR 1002.
 */
describe('device classification matches the shared backend fixture', () => {
  // tests/fixtures/device-limit-matrix.json is also asserted by the Rust test
  // `device_limit_matrix_fixture_matches_the_backend_classifier`, so a change
  // to either classifier fails one side until the fixture is updated.
  const BANK_FLAGS = {
    half: { direct: false, ac: false, threePhase: false },
    ac: { direct: true, ac: true, threePhase: false },
    threephase: { direct: true, ac: false, threePhase: true },
  } as const;

  it('covers every family', () => {
    expect(fixture.devices.length).toBeGreaterThanOrEqual(20);
  });

  it.each(fixture.devices)('$code ($family)', ({ code, bank, hr50 }) => {
    const want = BANK_FLAGS[bank as keyof typeof BANK_FLAGS];
    expect(usesDirectChargeLimit(code)).toBe(want.direct);
    expect(usesAcLimitRegisters(code)).toBe(want.ac);
    expect(isThreePhaseLimitModel(code)).toBe(want.threePhase);
    expect(deviceUsesHr50ActivePowerRate(code)).toBe(hr50);
  });
});

describe('usesAcLimitRegisters', () => {
  it.each([
    ['3001', 'AC-coupled'],
    ['3002', 'AC-coupled Mk2'],
    ['7001', 'Gateway'],
  ])('returns true for %s (%s)', (code) => {
    expect(usesAcLimitRegisters(code)).toBe(true);
  });

  it.each([
    ['2001', 'Gen hybrid (HR 111/112)'],
    ['4001', 'Three-phase (HR 1108/1110)'],
    ['8001', 'AIO (HR 111/112)'],
    ['4101', 'unsupported commercial AIO'],
  ])('returns false for %s (%s)', (code) => {
    expect(usesAcLimitRegisters(code)).toBe(false);
  });

  it('returns false when the code is missing', () => {
    expect(usesAcLimitRegisters(undefined)).toBe(false);
    expect(usesAcLimitRegisters(null)).toBe(false);
  });
});
