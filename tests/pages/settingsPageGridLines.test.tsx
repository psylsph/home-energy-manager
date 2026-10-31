import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, within } from '@testing-library/react';
import { openAdvanced } from './settingsTestUtils';

// The Chart Grid Lines group in Settings → Panel Controls. The real
// `useInverterStore` is the subject under test, so it's used as-is and
// reset between tests; only the API is stubbed.

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import PanelControlsSection from '../../src/components/settings/PanelControlsSection';

const Section = () => (
  <PanelControlsSection flash={() => {}} initialHiddenPanels={[]} octopusConfigured={false} />
);
import { useInverterStore } from '../../src/store/useInverterStore';

/** Silence noisy React act() warnings from async setState in useEffects. */
function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

describe('<PanelControlsSection/> — Chart Grid Lines sub-section (issue #111)', () => {
  beforeEach(() => {
    silenceConsoleError();
    // Reset between tests so the latch / state from one test doesn't leak.
    localStorage.removeItem('gridLineWeight');
    useInverterStore.setState({ gridLineWeight: 'standard' });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.removeItem('gridLineWeight');
  });

  describe('render', () => {
    it('renders the sub-section heading inside Panel Controls', async () => {
      render(<Section />);
      await openAdvanced();
      expect(
        await screen.findByRole('heading', { name: 'Chart Grid Lines', level: 3 }),
      ).toBeDefined();
    });

    it('renders both Standard and Subtle buttons with the swatch SVGs', async () => {
      render(<Section />);
      await openAdvanced();
      const standardBtn = await screen.findByRole('button', { name: /Standard/i });
      const subtleBtn = await screen.findByRole('button', { name: /Subtle/i });

      // Each button must carry a visual swatch so the user previews the
      // grid style before committing. We don't assert rendered colour
      // (that needs CSS evaluation), just that the <svg> lives inside
      // each button. The swatches use `aria-hidden="true"` so they're
      // skipped by the accessibility tree — query the container for an
      // <svg> instead.
      expect(standardBtn.querySelector('svg')).not.toBeNull();
      expect(subtleBtn.querySelector('svg')).not.toBeNull();
    });

    it('describes the Forecast chart as using the setting', async () => {
      render(<Section />);
      await openAdvanced();
      await screen.findByRole('heading', { name: 'Chart Grid Lines', level: 3 });
      expect(screen.getByText(/Power, History, Battery, Solar, and Forecast charts/)).toBeDefined();
    });

    it('marks Standard as the default selected preset', async () => {
      render(<Section />);
      await openAdvanced();
      const standardBtn = await screen.findByRole('button', { name: /Standard/i });
      const subtleBtn = await screen.findByRole('button', { name: /Subtle/i });
      // aria-pressed is the canonical a11y signal for a toggle button.
      // Standard is the default per issue #111 (existing users see no
      // visual change).
      expect(standardBtn.getAttribute('aria-pressed')).toBe('true');
      expect(subtleBtn.getAttribute('aria-pressed')).toBe('false');
    });
  });

  describe('interaction', () => {
    it('clicking Subtle switches the store to "subtle" and toggles aria-pressed', async () => {
      render(<Section />);
      await openAdvanced();
      const subtleBtn = await screen.findByRole('button', { name: /Subtle/i });

      fireEvent.click(subtleBtn);

      await waitFor(() => {
        expect(useInverterStore.getState().gridLineWeight).toBe('subtle');
      });
      expect(subtleBtn.getAttribute('aria-pressed')).toBe('true');
      expect(
        screen.getByRole('button', { name: /Standard/i }).getAttribute('aria-pressed'),
      ).toBe('false');
    });

    it('clicking Subtle persists the choice to localStorage', async () => {
      render(<Section />);
      await openAdvanced();
      const subtleBtn = await screen.findByRole('button', { name: /Subtle/i });
      fireEvent.click(subtleBtn);
      await waitFor(() => {
        expect(localStorage.getItem('gridLineWeight')).toBe('subtle');
      });
    });

    it('clicking Standard after Subtle restores the baseline state and storage', async () => {
      // Start in Subtle so we can prove the click goes back to Standard
      // rather than no-op'ing on the already-selected button.
      useInverterStore.getState().setGridLineWeight('subtle');
      localStorage.setItem('gridLineWeight', 'subtle');

      render(<Section />);
      await openAdvanced();
      const standardBtn = await screen.findByRole('button', { name: /Standard/i });
      fireEvent.click(standardBtn);

      await waitFor(() => {
        expect(useInverterStore.getState().gridLineWeight).toBe('standard');
        expect(localStorage.getItem('gridLineWeight')).toBe('standard');
      });
    });

    it('reflects an externally-set store value on next render', async () => {
      // The user can also flip the preference from devtools / future
      // settings sync. The next render must reflect the change without
      // needing a re-mount.
      const { rerender } = render(<Section />);
      await openAdvanced();
      // Wait for first paint of the buttons before mutating store state.
      await screen.findByRole('button', { name: /Standard/i });
      useInverterStore.getState().setGridLineWeight('subtle');
      rerender(<Section />);

      const subtleBtn = await screen.findByRole('button', { name: /Subtle/i });
      const standardBtn = screen.getByRole('button', { name: /Standard/i });
      expect(subtleBtn.getAttribute('aria-pressed')).toBe('true');
      expect(standardBtn.getAttribute('aria-pressed')).toBe('false');
    });
  });

  describe('position in Panel Controls', () => {
    it('sits under Panel Controls → Advanced, folded away until opened', async () => {
      // Issue #111 asks for the control inside Panel Controls; issue #402
      // moved it under Advanced, since it's rarely changed.
      render(<Section />);
      const advanced = await screen.findByRole('button', { name: 'Advanced' });
      expect(advanced.getAttribute('aria-expanded')).toBe('false');
      expect(screen.queryByRole('heading', { name: 'Chart Grid Lines' })).toBeNull();

      fireEvent.click(advanced);
      expect(advanced.getAttribute('aria-expanded')).toBe('true');
      const panel = document.getElementById(advanced.getAttribute('aria-controls')!)!;
      expect(within(panel).getByRole('heading', { name: 'Chart Grid Lines', level: 3 })).toBeDefined();
      const section = screen.getByRole('heading', { name: 'Panel Controls', level: 2 }).closest('section')!;
      expect(section.contains(panel)).toBe(true);

      fireEvent.click(advanced);
      expect(screen.queryByRole('heading', { name: 'Chart Grid Lines' })).toBeNull();
    });
  });
});
