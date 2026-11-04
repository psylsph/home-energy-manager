import { describe, it, expect } from 'vitest';
import contract from '../fixtures/givtcp-model-contract.json';
import { deviceSupportsTimedDischarge, isThreePhaseLimitModel } from '../../src/lib/deviceCapabilities';

// tests/fixtures/givtcp-model-contract.json records what GivTCP does per
// model and where HEM knowingly differs. The backend checks itself against it
// (src-tauri/src/inverter/givtcp_contract.rs); this keeps the front end's
// own copies of those rules in step, so the Control page never offers a
// control the backend would refuse, or hides one it would take.

interface Deviation {
  value: string | boolean;
  issue: number;
  why: string;
}

interface Row {
  code: string;
  arm_fw: number;
  hem: string;
  givtcp: { pause: string; three_phase: boolean };
  hem_deviations?: Record<string, Deviation>;
}

const rows = (contract as { rows: Row[] }).rows;

/** What HEM does for a field: GivTCP's value unless the contract records a deviation. */
function hem<K extends 'pause' | 'three_phase'>(row: Row, field: K): Row['givtcp'][K] {
  const deviation = row.hem_deviations?.[field];
  return (deviation ? deviation.value : row.givtcp[field]) as Row['givtcp'][K];
}

describe('front end against the GivTCP model contract', () => {
  it('has a row for every model family', () => {
    expect(rows.length).toBeGreaterThanOrEqual(19);
  });

  for (const row of rows) {
    const label = `0x${row.code} ARM ${row.arm_fw} (${row.hem})`;

    it(`${label}: offers Timed Discharge only where HEM pauses with a window`, () => {
      const snapshot = { device_type_code: row.code, firmware_version: String(row.arm_fw) };
      expect(deviceSupportsTimedDischarge(snapshot)).toBe(hem(row, 'pause') === 'mode_and_window');
    });

    it(`${label}: uses the three-phase rate bank only on the three-phase layout`, () => {
      expect(isThreePhaseLimitModel(row.code)).toBe(hem(row, 'three_phase'));
    });
  }
});
