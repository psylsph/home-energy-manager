import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import SolarArraysSection from '../../src/components/settings/SolarArraysSection';
import { apiPost } from '../../src/lib/api';
import type { PollSettings } from '../../src/lib/types';

/** Two DC strings and two CT arrays, one of them unnamed. */
const SAVED: Partial<PollSettings> = {
  pv1_rated_kw: 3.8,
  pv2_rated_kw: 1.3,
  solar_arrays: [
    { meter_address: 1, name: 'East roof', rated_kw: 6.0 },
    { meter_address: 2, name: '', rated_kw: 4.2 },
  ],
};

const Section = ({ initial = SAVED }: { initial?: Partial<PollSettings> }) => (
  <SolarArraysSection flash={() => {}} initial={initial} />
);

function silenceConsoleError() {
  vi.spyOn(console, 'error').mockImplementation(() => {});
}

describe('<SolarArraysSection/> — Solar Arrays (issue #110)', () => {
  beforeEach(() => {
    silenceConsoleError();
    vi.mocked(apiPost).mockClear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  // -------------------------------------------------------------------------
  // Render
  // -------------------------------------------------------------------------

  describe('render', () => {
    it('renders the Solar Arrays section heading', async () => {
      render(<Section />);
      await screen.findByRole('heading', { name: 'Solar Arrays', level: 2 });
    });

    it('renders PV1 and PV2 kWp number inputs', async () => {
      render(<Section />);
      await screen.findByTestId('pv1-rated-kw-input');
      await screen.findByTestId('pv2-rated-kw-input');
    });

    it('caps PV1 / PV2 kWp inputs at max=100', async () => {
      render(<Section />);
      const pv1 = await screen.findByTestId('pv1-rated-kw-input');
      const pv2 = await screen.findByTestId('pv2-rated-kw-input');
      expect(pv1.getAttribute('max')).toBe('100');
      expect(pv2.getAttribute('max')).toBe('100');
    });

    it('negative PV1 input is accepted but clamped to 0 on save', async () => {
      render(<Section />);
      const pv1 = await screen.findByTestId('pv1-rated-kw-input');
      fireEvent.change(pv1, { target: { value: '-2' } });
      fireEvent.click(await screen.findByTestId('solar-arrays-save'));
      await waitFor(() => {
        expect(apiPost).toHaveBeenCalledWith(
          '/api/settings',
          expect.objectContaining({ pv1_rated_kw: 0 }),
        );
      });
    });

    it('PV2 kWp can be edited and posted independently of PV1', async () => {
      render(<Section />);
      const pv2 = await screen.findByTestId('pv2-rated-kw-input');
      fireEvent.change(pv2, { target: { value: '8.5' } });
      fireEvent.click(await screen.findByTestId('solar-arrays-save'));
      await waitFor(() => {
        expect(apiPost).toHaveBeenCalledWith(
          '/api/settings',
          expect.objectContaining({ pv1_rated_kw: 3.8, pv2_rated_kw: 8.5 }),
        );
      });
    });

    it('renders the "+ Add array" button', async () => {
      render(<Section />);
      await screen.findByTestId('solar-array-add');
    });

    it('renders the Save Solar Arrays button', async () => {
      render(<Section />);
      await screen.findByTestId('solar-arrays-save');
    });

    it('renders two CT array rows from settings', async () => {
      render(<Section />);
      const rows = await screen.findAllByTestId('solar-array-row');
      expect(rows).toHaveLength(2);
    });

    it('hydrates PV1/PV2 kWp inputs from settings (3.8 / 1.3)', async () => {
      render(<Section />);
      const pv1 = await screen.findByTestId('pv1-rated-kw-input');
      const pv2 = await screen.findByTestId('pv2-rated-kw-input');
      expect((pv1 as HTMLInputElement).value).toBe('3.8');
      expect((pv2 as HTMLInputElement).value).toBe('1.3');
    });

    it('hydrates CT array row names and kWp from settings', async () => {
      render(<Section />);
      const names = await screen.findAllByTestId('solar-array-name');
      const kwpInputs = await screen.findAllByTestId('solar-array-kwp');
      // Row 0: East roof, 6.0 kWp → value "6" (String(6.0) === "6")
      expect((names[0] as HTMLInputElement).value).toBe('East roof');
      expect((kwpInputs[0] as HTMLInputElement).value).toBe('6');
      // Row 1: unnamed, 4.2 kWp
      expect((names[1] as HTMLInputElement).value).toBe('');
      expect((kwpInputs[1] as HTMLInputElement).value).toBe('4.2');
    });

    it('shows placeholder when solar_arrays is empty', async () => {
      render(<Section initial={{ solar_arrays: [] }} />);
      await screen.findByText(/No external CT meter arrays configured/i);
    });
  });

  // -------------------------------------------------------------------------
  // CT array list interactions
  // -------------------------------------------------------------------------

  describe('CT array list', () => {
    it('"+ Add array" appends a new row', async () => {
      render(<Section />);
      const before = (await screen.findAllByTestId('solar-array-row')).length;
      fireEvent.click(await screen.findByTestId('solar-array-add'));
      const after = (await screen.findAllByTestId('solar-array-row')).length;
      expect(after).toBe(before + 1);
    });

    it('a new row defaults to the lowest unused meter address', async () => {
      render(<Section />);
      fireEvent.click(await screen.findByTestId('solar-array-add'));
      const selects = await screen.findAllByTestId('solar-array-address');
      // Mock returns arrays at 1 and 2, so next unused is 3.
      expect((selects[selects.length - 1] as HTMLSelectElement).value).toBe('3');
    });

    it('✕ removes its row', async () => {
      render(<Section />);
      const before = (await screen.findAllByTestId('solar-array-row')).length;
      const removes = await screen.findAllByTestId('solar-array-remove');
      fireEvent.click(removes[removes.length - 1]);
      const after = (await screen.findAllByTestId('solar-array-row')).length;
      expect(after).toBe(before - 1);
    });

    it('editing a CT row name updates the POST payload', async () => {
      render(<Section />);
      const names = await screen.findAllByTestId('solar-array-name');
      // Change "East roof" to "West array".
      fireEvent.change(names[0], { target: { value: 'West array' } });
      fireEvent.click(await screen.findByTestId('solar-arrays-save'));
      await waitFor(() => {
        expect(apiPost).toHaveBeenCalledWith(
          '/api/settings',
          expect.objectContaining({
            solar_arrays: expect.arrayContaining([
              expect.objectContaining({ meter_address: 1, name: 'West array' }),
            ]),
          }),
        );
      });
    });

    it('adding then removing a new row leaves the payload unchanged', async () => {
      render(<Section />);
      // Add a row then immediately remove it.
      fireEvent.click(await screen.findByTestId('solar-array-add'));
      const removes = await screen.findAllByTestId('solar-array-remove');
      fireEvent.click(removes[removes.length - 1]);
      fireEvent.click(await screen.findByTestId('solar-arrays-save'));
      await waitFor(() => {
        expect(apiPost).toHaveBeenCalledWith(
          '/api/settings',
          expect.objectContaining({
            solar_arrays: expect.arrayContaining([
              expect.objectContaining({ meter_address: 1 }),
              expect.objectContaining({ meter_address: 2 }),
            ]),
          }),
        );
      });
    });

    it('a new CT array kWp input has max=100', async () => {
      render(<Section />);
      fireEvent.click(await screen.findByTestId('solar-array-add'));
      const kwpInputs = await screen.findAllByTestId('solar-array-kwp');
      expect(
        kwpInputs[kwpInputs.length - 1].getAttribute('max'),
      ).toBe('100');
    });
  });

  // -------------------------------------------------------------------------
  // Save round-trip
  // -------------------------------------------------------------------------

  describe('save round-trip', () => {
    it('posts pv1_rated_kw, pv2_rated_kw, and solar_arrays', async () => {
      render(<Section />);
      fireEvent.click(await screen.findByTestId('solar-arrays-save'));
      await waitFor(() => {
        expect(apiPost).toHaveBeenCalledWith(
          '/api/settings',
          expect.objectContaining({
            pv1_rated_kw: 3.8,
            pv2_rated_kw: 1.3,
            solar_arrays: expect.arrayContaining([
              expect.objectContaining({
                meter_address: 1, name: 'East roof', rated_kw: 6.0,
              }),
            ]),
          }),
        );
      });
    });

    it('editing PV1 kWp to 5 posts the new value', async () => {
      render(<Section />);
      const pv1 = await screen.findByTestId('pv1-rated-kw-input');
      fireEvent.change(pv1, { target: { value: '5' } });
      fireEvent.click(await screen.findByTestId('solar-arrays-save'));
      await waitFor(() => {
        expect(apiPost).toHaveBeenCalledWith(
          '/api/settings',
          expect.objectContaining({ pv1_rated_kw: 5, pv2_rated_kw: 1.3 }),
        );
      });
    });

    it('blank PV1 serialises to 0', async () => {
      render(<Section />);
      const pv1 = await screen.findByTestId('pv1-rated-kw-input');
      fireEvent.change(pv1, { target: { value: '' } });
      fireEvent.click(await screen.findByTestId('solar-arrays-save'));
      await waitFor(() => {
        expect(apiPost).toHaveBeenCalledWith(
          '/api/settings',
          expect.objectContaining({ pv1_rated_kw: 0 }),
        );
      });
    });

    it('a new CT row appears in the save payload', async () => {
      render(<Section />);
      fireEvent.click(await screen.findByTestId('solar-array-add'));
      const names = await screen.findAllByTestId('solar-array-name');
      fireEvent.change(names[names.length - 1], { target: { value: 'Garage' } });
      const kwpInputs = await screen.findAllByTestId('solar-array-kwp');
      fireEvent.change(kwpInputs[kwpInputs.length - 1], { target: { value: '3.68' } });
      fireEvent.click(await screen.findByTestId('solar-arrays-save'));
      await waitFor(() => {
        expect(apiPost).toHaveBeenCalledWith(
          '/api/settings',
          expect.objectContaining({
            solar_arrays: expect.arrayContaining([
              expect.objectContaining({
                meter_address: 3, name: 'Garage', rated_kw: 3.68,
              }),
            ]),
          }),
        );
      });
    });

    it('only meter addresses 1-8 appear in the POST payload', async () => {
      render(<Section />);
      fireEvent.click(await screen.findByTestId('solar-arrays-save'));
      await waitFor(() => {
        const calls = vi.mocked(apiPost).mock.calls;
        const saveCall = calls.find((c) => c[0] === '/api/settings');
        expect(saveCall).toBeDefined();
        const body = saveCall![1] as { solar_arrays?: { meter_address: number }[] };
        if (body.solar_arrays) {
          for (const arr of body.solar_arrays) {
            expect(arr.meter_address).toBeGreaterThanOrEqual(1);
            expect(arr.meter_address).toBeLessThanOrEqual(8);
          }
        }
      });
    });

    it('clearing PV1 and PV2 serialises both to 0', async () => {
      render(<Section />);
      const pv1 = await screen.findByTestId('pv1-rated-kw-input');
      const pv2 = await screen.findByTestId('pv2-rated-kw-input');
      fireEvent.change(pv1, { target: { value: '' } });
      fireEvent.change(pv2, { target: { value: '' } });
      fireEvent.click(await screen.findByTestId('solar-arrays-save'));
      await waitFor(() => {
        expect(apiPost).toHaveBeenCalledWith(
          '/api/settings',
          expect.objectContaining({ pv1_rated_kw: 0, pv2_rated_kw: 0 }),
        );
      });
    });
  });
});
