import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import SummaryTiles from '../../src/components/SummaryTiles';
import type { InverterSnapshot } from '../../src/lib/types';

function snapshot(): InverterSnapshot {
  return {
    today_solar_kwh: 12.3,
    home_energy_today_kwh: 4.5,
    today_charge_kwh: 2.1,
    today_import_kwh: 1.2,
    today_export_kwh: 6.7,
    today_discharge_kwh: 0.8,
  } as InverterSnapshot;
}

describe('<SummaryTiles/> mobile layout', () => {
  it('hides the Today heading on mobile and restores it on md+', () => {
    render(<SummaryTiles snapshot={snapshot()} />);

    const heading = screen.getByText('Today');
    expect(heading.className).toContain('hidden');
    expect(heading.className).toContain('md:block');
  });

  it('uses a 3 × 2 mobile grid while keeping the desktop 3-column layout', () => {
    const { container } = render(<SummaryTiles snapshot={snapshot()} />);

    const grid = container.querySelector('[data-testid="summary-tiles-grid"]') as HTMLElement | null;
    expect(grid).not.toBeNull();
    expect(grid!.style.display).toBe('grid');
    expect(grid!.style.gridTemplateColumns).toBe('repeat(3, minmax(0, 1fr))');
    expect(grid!.style.gridAutoFlow).toBe('row');
    expect(grid!.className).not.toContain('grid-cols-2');
  });

  it('uses smaller tile icons on mobile and the original size on md+', () => {
    const { container } = render(<SummaryTiles snapshot={snapshot()} />);

    const solarIcon = Array.from(container.querySelectorAll('span')).find(
      (el) => el.textContent === '☀️',
    );
    expect(solarIcon).toBeDefined();
    expect(solarIcon!.className).toContain('w-5');
    expect(solarIcon!.className).toContain('h-5');
    expect(solarIcon!.className).toContain('text-[10px]');
    expect(solarIcon!.className).toContain('md:w-8');
    expect(solarIcon!.className).toContain('md:h-8');
    expect(solarIcon!.className).toContain('md:text-sm');
  });
});

describe('<SummaryTiles/> Battery tile values', () => {
  // These tests pin the displayed values to the snapshot fields, so a future
  // refactor that accidentally re-routes or renames the Battery Charged /
  // Discharged tiles (e.g. issue #164 — a decoder regression that drove both
  // to 0.0 kWh) is caught at the component level. The frontend simply
  // formats whatever the backend sends; these tests guard the wiring.
  function tileValue(container: HTMLElement, label: string): string | null {
    // Walk up from the label span to the tile container (the div with the
    // title tooltip), then pick the font-mono value span inside it.
    const labelEl = Array.from(container.querySelectorAll('span')).find(
      (el) => el.textContent === label,
    );
    if (!labelEl) return null;
    const tile = labelEl.closest('[title]');
    if (!tile) return null;
    const valueEl = tile.querySelector('span.font-mono');
    return valueEl?.textContent ?? null;
  }

  it('Battery Charged tile shows today_charge_kwh value', () => {
    const snap = {
      today_charge_kwh: 7.7,
      today_discharge_kwh: 9.9,
    } as InverterSnapshot;
    const { container } = render(<SummaryTiles snapshot={snap} />);
    expect(tileValue(container, 'Battery Charged')).toBe('7.7kWh');
  });

  it('Battery Discharged tile shows today_discharge_kwh value', () => {
    const snap = {
      today_charge_kwh: 7.7,
      today_discharge_kwh: 9.9,
    } as InverterSnapshot;
    const { container } = render(<SummaryTiles snapshot={snap} />);
    expect(tileValue(container, 'Battery Discharged')).toBe('9.9kWh');
  });

  it('Battery tiles display 0.0kWh when snapshot fields are zero', () => {
    // Pins the user-visible behaviour for the "battery hasn't accumulated
    // today yet" case so we don't accidentally hide or replace the zero
    // display with a placeholder.
    const snap = {
      today_charge_kwh: 0,
      today_discharge_kwh: 0,
    } as InverterSnapshot;
    const { container } = render(<SummaryTiles snapshot={snap} />);
    expect(tileValue(container, 'Battery Charged')).toBe('0.0kWh');
    expect(tileValue(container, 'Battery Discharged')).toBe('0.0kWh');
  });
});
