import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import TariffsSection from '../../src/components/settings/TariffsSection';

// Issue #455: history rows keep the rates they were recorded with, so saving
// a tariff only changes costs from now on. The section must say so beside Save.
describe('<TariffsSection/> — changes apply from now (issue #455)', () => {
  afterEach(() => cleanup());

  it('says beside the Save button that past costs keep their recorded rates', () => {
    render(<TariffsSection flash={() => {}} initial={{}} />);
    expect(
      screen.getByText('Changes apply from now. Past costs keep the rates they were recorded with.'),
    ).toBeTruthy();
  });

  it('keeps the note visible even when the tariff is invalid and Save is disabled', () => {
    render(
      <TariffsSection
        flash={() => {}}
        initial={{ import_tariff_config: { slots: [] } }}
      />,
    );
    expect(screen.getByText(/Past costs keep the rates/)).toBeTruthy();
  });
});
