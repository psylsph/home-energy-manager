/**
 * Precision coverage for the tariff Rate entry field in Settings.
 *
 * The rate input renders the stored £/kWh value as pence to 3 decimal places
 * (matching the most precise field in the app) and uses step="0.001" so a
 * sub-penny value like 12.345p passes HTML number-input validation. This
 * hydrates Settings with a 0.12345 £/kWh rate and asserts the rendered input
 * shows "12.345" at step 0.001.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, waitFor } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import TariffsSection from '../../src/components/settings/TariffsSection';

const Section = () => (
  <TariffsSection
    flash={() => {}}
    initial={{
      // 0.12345 £/kWh = 12.345p — needs 3dp.
      import_tariff_config: { slots: [{ start: '00:00', end: '23:59', rate: 0.12345 }] },
      export_tariff_config: { slots: [{ start: '00:00', end: '23:59', rate: 0.15 }] },
      import_standing_charge_p_per_day: 0,
    }}
  />
);

describe('<TariffsSection/> — tariff Rate field is 3dp', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it('renders the import rate to 3dp (12.345p) with a 0.001 step', async () => {
    render(<Section />);
    // Import editor is rendered before Export, so its Rate input is first.
    const rateInputs = await screen.findAllByLabelText(/Rate \(p\/kWh\)/i);
    const importRate = rateInputs[0] as HTMLInputElement;
    await waitFor(() => {
      expect(importRate.value).toBe('12.345');
    });
    expect(importRate.getAttribute('step')).toBe('0.001');
  });
});
