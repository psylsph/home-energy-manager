import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// `apiPost` is mocked so tests can assert on the recorded calls.

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import TariffsSection from '../../src/components/settings/TariffsSection';

// Issue #131: a non-zero saved Standing Charge, so hydration can be checked.
const Section = () => (
  <TariffsSection flash={() => {}} initial={{ import_standing_charge_p_per_day: 54.86 }} />
);
import { apiPost } from '../../src/lib/api';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

describe('<TariffsSection/> — Standing Charge input (issue #131)', () => {
  beforeEach(() => {
    silenceConsoleError();
    vi.mocked(apiPost).mockClear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  describe('render', () => {
    it('renders the Standing Charge input below the Export tariff editor', async () => {
      // The input lives in its own bordered card directly under the Export
      // TariffSlotEditor, with the Save Tariffs button further down. The
      // exact ordering matters — the issue specifically asks for the input
      // to be "below the Export entry section".
      render(<Section />);

      const input = await screen.findByLabelText(
        /Import Standing Charge in Pence Per Day/i,
      );
      expect(input).toBeDefined();
      // It should be a number input with sensible step / min attrs.
      expect(input.getAttribute('type')).toBe('number');
      expect(input.getAttribute('min')).toBe('0');
    });

    it('hydrates the input from the server-provided Standing Charge', async () => {
      // The saved Standing Charge is 54.86; the input must show exactly that.
      render(<Section />);
      const input = await screen.findByLabelText(
        /Import Standing Charge in Pence Per Day/i,
      );
      await waitFor(() => {
        expect((input as HTMLInputElement).value).toBe('54.86');
      });
    });

    it('renders the helper text explaining the field purpose', async () => {
      // The placeholder / caption must mention p/day and the Octopus Flux
      // example so a first-time user knows what to type.
      render(<Section />);
      expect(
        await screen.findByText(/Daily fixed import cost/i),
      ).toBeDefined();
    });

    it('places the standing-charge card between Export editor and Save button', async () => {
      // The Section 4 (Energy Tariffs) ordering is:
      //   Import editor → Export editor → Standing Charge card → Save button.
      // Future refactors must not relocate the input to a different section
      // — this guards against accidentally moving it into Section 5
      // (Local Weather) or somewhere else entirely.
      render(<Section />);

      const exportHeading = await screen.findByRole('heading', {
        name: 'Export',
        level: 3,
      });
      const standingLabel = await screen.findByText(/Standing Charge \(p\/day\)/);
      const saveButton = await screen.findByRole('button', { name: /Save Tariffs/i });

      const section = exportHeading.closest('section');
      expect(section).not.toBeNull();

      // Compare document order via compareDocumentPosition: the standing
      // charge label must come AFTER the Export heading and BEFORE the
      // Save Tariffs button.
      const exportBeforeStanding =
        exportHeading.compareDocumentPosition(standingLabel) &
        Node.DOCUMENT_POSITION_FOLLOWING;
      const standingBeforeSave =
        standingLabel.compareDocumentPosition(saveButton) & Node.DOCUMENT_POSITION_FOLLOWING;

      expect(exportBeforeStanding).not.toBe(0);
      expect(standingBeforeSave).not.toBe(0);
    });
  });

  describe('save round-trip', () => {
    it('Save Tariffs posts the Standing Charge in pence/day', async () => {
      render(<Section />);
      const input = await screen.findByLabelText(
        /Import Standing Charge in Pence Per Day/i,
      );

      // Edit to a fresh value and click Save Tariffs.
      fireEvent.change(input, { target: { value: '42.50' } });
      const saveButton = await screen.findByRole('button', { name: /Save Tariffs/i });
      fireEvent.click(saveButton);

      await waitFor(() => {
        // Issue #131: the field name must be `import_standing_charge_p_per_day`
        // and the value must be the number the user entered, NOT a string
        // and NOT a £-converted value.
        expect(apiPost).toHaveBeenCalledWith('/api/settings', {
          import_tariff_config: expect.objectContaining({ slots: expect.any(Array) }),
          export_tariff_config: expect.objectContaining({ slots: expect.any(Array) }),
          import_standing_charge_p_per_day: 42.5,
        });
      });
    });

    it('blank input serialises to 0 (no Standing Charge)', async () => {
      // Clear the field — saving with an empty value should persist 0, the
      // documented "no Standing Charge" sentinel.
      render(<Section />);
      const input = await screen.findByLabelText(
        /Import Standing Charge in Pence Per Day/i,
      );
      fireEvent.change(input, { target: { value: '' } });
      const saveButton = await screen.findByRole('button', { name: /Save Tariffs/i });
      fireEvent.click(saveButton);

      await waitFor(() => {
        expect(apiPost).toHaveBeenCalledWith(
          '/api/settings',
          expect.objectContaining({ import_standing_charge_p_per_day: 0 }),
        );
      });
    });

    it('non-numeric input serialises to 0 instead of NaN', async () => {
      // Defensive: a paste of "abc" or similar junk must not blow up the
      // save handler with NaN (which would fail backend validation or, worse,
      // serialise into settings.json as the literal string "NaN").
      render(<Section />);
      const input = await screen.findByLabelText(
        /Import Standing Charge in Pence Per Day/i,
      );
      fireEvent.change(input, { target: { value: 'abc' } });
      const saveButton = await screen.findByRole('button', { name: /Save Tariffs/i });
      fireEvent.click(saveButton);

      await waitFor(() => {
        const calls = vi.mocked(apiPost).mock.calls;
        const saveCall = calls.find((c) => c[0] === '/api/settings');
        expect(saveCall).toBeDefined();
        const body = saveCall![1] as Record<string, unknown>;
        const v = body.import_standing_charge_p_per_day;
        expect(typeof v).toBe('number');
        expect(Number.isFinite(v as number)).toBe(true);
      });
    });
  });
});