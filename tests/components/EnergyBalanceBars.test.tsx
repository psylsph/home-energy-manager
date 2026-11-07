import { describe, it, expect, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import EnergyBalanceBars from '../../src/components/EnergyBalanceBars';
import type { InverterSnapshot } from '../../src/lib/types';

/**
 * The Today balance bars (issue #429): every fill carries the same
 * label + value pair, the two bars share their scale, and the AC-coupled
 * case says so instead of showing a quietly short bar.
 */

function snapshot(over: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    today_solar_kwh: 12,
    today_import_kwh: 2,
    today_discharge_kwh: 1,
    home_energy_today_kwh: 6,
    today_export_kwh: 4,
    today_charge_kwh: 2.5,
    device_type_code: '2201',
    ...over,
  } as InverterSnapshot;
}

const tid = (id: string) => screen.getByTestId(id);

describe('<EnergyBalanceBars/>', () => {
  afterEach(() => {
    cleanup();
  });

  function renderBalance(over: InverterSnapshot) {
    return render(<EnergyBalanceBars snapshot={over} />);
  }

  /** Each fill's stacked label and value, in render order. */
  function fillPairs(container: HTMLElement) {
    return [...container.querySelectorAll<HTMLElement>('[data-testid="balance-fill"]')]
      .map((f) => [...f.querySelectorAll('span')].map((s) => s.textContent));
  }

  it('keeps every label and value in the card text', () => {
    const { container } = renderBalance(snapshot());
    const card = container.querySelector('[data-testid="today-balance"]')!;
    expect(card.textContent).toContain('Solar');
    expect(card.textContent).toContain('Grid Import');
    expect(card.textContent).toContain('Battery Out');
    expect(card.textContent).toContain('Home Use');
    expect(card.textContent).toContain('Grid Export');
    expect(card.textContent).toContain('Battery In');
    expect(card.textContent).toContain('12.0kWh');
    expect(card.textContent).toContain('2.0kWh');
    expect(card.textContent).toContain('1.0kWh');
    expect(card.textContent).toContain('6.0kWh');
    expect(card.textContent).toContain('4.0kWh');
    expect(card.textContent).toContain('2.5kWh');
  });

  it('shows each bar total at the bar', () => {
    const { container } = renderBalance(snapshot());
    const card = container.querySelector('[data-testid="today-balance"]')!;
    expect(card.textContent).toContain('15.0kWh');
    expect(card.textContent).toContain('12.5kWh');
  });

  it('puts the same label + value pair inside every fill, with no legend', () => {
    const { container } = renderBalance(snapshot());
    // One rule for every segment: the pill carries the pair, in the same
    // place and form at every width. There is no legend to disagree with it.
    expect(container.querySelector('[data-testid="balance-legend"]')).toBeNull();
    expect(fillPairs(container)).toEqual([
      ['Solar', '12.0kWh'],
      ['Grid Import', '2.0kWh'],
      ['Battery Out', '1.0kWh'],
      ['Home Use', '6.0kWh'],
      ['Grid Export', '4.0kWh'],
      ['Battery In', '2.5kWh'],
    ]);
  });

  it('keeps the live evening reading in the pills (issue #429 review)', () => {
    // From real hardware: In 22.3 kWh (Solar 8.0, Grid Import 9.6, Battery
    // Out 4.7), Out 22.3 kWh (Home Use 7.7, Grid Export 3.2, Battery In
    // 11.4). Every fill keeps both parts, even the narrow ones: the label
    // wraps and the value stays on its own line.
    const { container } = renderBalance(snapshot({
      today_solar_kwh: 8.0,
      today_import_kwh: 9.6,
      today_discharge_kwh: 4.7,
      home_energy_today_kwh: 7.7,
      today_export_kwh: 3.2,
      today_charge_kwh: 11.4,
    }));
    expect(container.querySelector('[data-testid="balance-legend"]')).toBeNull();
    expect(fillPairs(container)).toEqual([
      ['Solar', '8.0kWh'],
      ['Grid Import', '9.6kWh'],
      ['Battery Out', '4.7kWh'],
      ['Home Use', '7.7kWh'],
      ['Grid Export', '3.2kWh'],
      ['Battery In', '11.4kWh'],
    ]);
  });

  it('does not draw zero segments, but keeps every value for screen readers', () => {
    const { container } = renderBalance(snapshot({
      today_solar_kwh: 8,
      today_import_kwh: 0,
      today_discharge_kwh: 0,
      home_energy_today_kwh: 0,
      today_export_kwh: 0,
      today_charge_kwh: 0,
    }));
    expect(container.querySelector('[data-testid="balance-legend"]')).toBeNull();
    // Energy In = 8, so the zero segments never draw a fill, and the empty
    // Energy Out bar draws nothing at all.
    const fills = [...container.querySelectorAll<HTMLElement>('[data-testid="balance-fill"]')];
    expect(fills.length).toBe(1);
    expect(fills[0].textContent).toBe('Solar8.0kWh');
    const list = container.querySelector('ul.sr-only')!;
    for (const entry of [
      'Grid Import 0.0kWh', 'Battery Out 0.0kWh',
      'Home Use 0.0kWh', 'Grid Export 0.0kWh', 'Battery In 0.0kWh',
    ]) {
      expect(list.textContent).toContain(entry);
    }
  });

  it('shows an empty state when nothing has been recorded', () => {
    const { container } = renderBalance(snapshot({
      today_solar_kwh: 0, today_import_kwh: 0, today_discharge_kwh: 0,
      home_energy_today_kwh: 0, today_export_kwh: 0, today_charge_kwh: 0,
    }));
    expect(tid('today-balance').textContent).toContain('No energy recorded yet today.');
    expect(container.querySelector('[data-testid="balance-fill"]')).toBeNull();
    // With no summary to ride, the ⓘ still sits on the footer line.
    expect(screen.getByRole('button', { name: "More about Today's Energy Balance" })).toBeDefined();
  });

  it('renders the derived percentages line from the same totals', () => {
    // Solar 10 in / 5.4 exported → 46% kept; 20.4 of 40 in imported → 51%.
    const { container } = renderBalance(snapshot({
      today_solar_kwh: 10,
      today_import_kwh: 20.4,
      today_discharge_kwh: 9.6,
      today_export_kwh: 5.4,
      home_energy_today_kwh: 30,
      today_charge_kwh: 12,
    }));
    const card = container.querySelector('[data-testid="today-balance"]')!;
    expect(card.textContent)
      .toContain('At least 46% of solar used or stored at home · 51% of energy in came from the grid');
  });

  it('rides the ⓘ at the end of the summary line', () => {
    // Solar 10 in / 5.4 exported → 46% kept; 20.4 of 40 in imported → 51%.
    renderBalance(snapshot({
      today_solar_kwh: 10,
      today_import_kwh: 20.4,
      today_discharge_kwh: 9.6,
      today_export_kwh: 5.4,
      home_energy_today_kwh: 30,
      today_charge_kwh: 12,
    }));
    const tip = screen.getByRole('button', { name: "More about Today's Energy Balance" });
    const line = tip.closest('p')!;
    // Same paragraph as the summary sentence, not the heading: the sentence
    // and the icon share the one line of text.
    expect(line.textContent)
      .toContain('At least 46% of solar used or stored at home · 51% of energy in came from the grid');
    // The full explanation lives behind it.
    expect(line.textContent).toContain('Home Use is worked out from the other five');
  });

  it('explains when the bars match and when losses show (code review #4)', () => {
    renderBalance(snapshot());
    const line = screen.getByRole('button', { name: "More about Today's Energy Balance" }).closest('p')!;
    // Single-phase, All-in-One and AC-coupled inverters derive Home Use as
    // In − export − charging, so the bars match by construction; only the
    // models with their own load counter can show a loss gap.
    expect(line.textContent).toContain('so the two bars match');
    expect(line.textContent).toContain('Three-phase inverters and the Gateway');
    expect(line.textContent).not.toContain('every day');
    // Why the solar share can read "At least" (second code review #6).
    expect(line.textContent).toContain('the solar share is a minimum');
  });

  it('says Energy In is what came in, not what went out (third code review #10)', () => {
    renderBalance(snapshot());
    const line = screen.getByRole('button', { name: "More about Today's Energy Balance" }).closest('p')!;
    expect(line.textContent).not.toContain('sent out');
    expect(line.textContent).toContain('Energy In is everything that came in');
  });

  it('keeps the ⓘ available where there is no summary (AC-coupled, all of In unmeasured)', () => {
    // An AC-coupled night: nothing In today, Home Use still counted Out.
    // The solar share is unmeasured and the grid share needs In — no summary,
    // but the ⓘ must not vanish with it.
    const { container } = renderBalance(snapshot({
      device_type_code: '3001',
      today_solar_kwh: 0, today_import_kwh: 0, today_discharge_kwh: 0,
      home_energy_today_kwh: 8, today_export_kwh: 0, today_charge_kwh: 0,
    }));
    const card = container.querySelector('[data-testid="today-balance"]')!;
    expect(card.textContent).not.toContain('came from the grid');
    expect(screen.getByRole('button', { name: "More about Today's Energy Balance" })).toBeDefined();
  });

  it('draws the bars tall enough for the stacked label and value', () => {
    const { container } = renderBalance(snapshot());
    const firstFill = container.querySelector<HTMLElement>('[data-testid="balance-fill"]')!;
    // The track is the fill's parent; three wrapped lines (Grid Export on a
    // phone) need the room, so it grows past its minimum instead of clipping.
    const track = firstFill.parentElement!.className.split(/\s+/);
    expect(track).toContain('min-h-9');
    expect(track).not.toContain('h-9');
    // A fixed-height fill would clip the text again; it stretches with the track.
    expect(firstFill.className.split(/\s+/)).not.toContain('h-full');
  });

  it('gives every fill its own legible text colour (issue #429 review)', () => {
    const { container } = renderBalance(snapshot());
    const fills = [...container.querySelectorAll<HTMLElement>('[data-testid="balance-fill"]')];
    expect(fills.length).toBeGreaterThan(0);
    for (const fill of fills) {
      // The literal ratios live in index.css; the component must pick the
      // token that matches the fill rather than inheriting the page colour.
      expect(fill.style.color).toMatch(/^var\(--app-on-/);
    }
  });

  it('keeps room for the text of a tiny segment (second code review #7)', () => {
    // 0.4 kWh of Battery Out on a 30 kWh day is about 1% of the bar: sized by
    // width alone it was ~4 px on a phone and both lines were clipped away.
    const { container } = renderBalance(snapshot({
      today_solar_kwh: 20,
      today_import_kwh: 9.6,
      today_discharge_kwh: 0.4,
      home_energy_today_kwh: 20,
      today_export_kwh: 5,
      today_charge_kwh: 5,
    }));
    const fills = [...container.querySelectorAll<HTMLElement>('[data-testid="balance-fill"]')];
    const tiny = fills.find((f) => f.textContent === 'Battery Out0.4kWh')!;
    expect(tiny.style.minWidth).toBe('3rem');
    for (const fill of fills) {
      // Grown by its share from a zero basis, never a fixed width, so the
      // others give up the room and a bar whose shares sum below 1 still
      // stops short (CSS fills only that fraction of the free space).
      expect(fill.style.width).toBe('');
      expect(fill.style.flexBasis).toBe('0px');
      expect(Number(fill.style.flexGrow)).toBeGreaterThan(0);
      expect(Number(fill.style.flexGrow)).toBeLessThanOrEqual(1);
    }
    expect(Number(tiny.style.flexGrow)).toBeCloseTo(0.4 / 30, 4);
  });

  it('animates the size only when motion is allowed (issue #429 review)', () => {
    const { container } = renderBalance(snapshot());
    const fill = container.querySelector<HTMLElement>('[data-testid="balance-fill"]')!;
    expect(fill.className).toContain('motion-safe:transition-[flex-grow]');
    expect(fill.className).toContain('motion-safe:duration-[400ms]');
    // An inline transition would beat the reduced-motion media query.
    expect(fill.getAttribute('style')).not.toContain('transition');
  });

  it('explains unmeasured PV in the ⓘ, not as a visible note, on AC-coupled systems', () => {
    // Issue #429 follow-up: the visible note is gone; the explanation sits
    // behind the ⓘ so the card stays to the numbers it can stand behind.
    for (const code of ['3001', '3002', '8001']) {
      cleanup();
      const { container } = renderBalance(snapshot({ device_type_code: code }));
      const card = container.querySelector('[data-testid="today-balance"]')!;
      const t = screen.getByRole('button', { name: "More about Today's Energy Balance" });
      const text = document.getElementById(t.getAttribute('aria-describedby')!)!.textContent!;
      expect(text).toContain("Solar from a separate PV system isn't measured by the inverter");
      expect(text).toContain('Home Use leaves out what that solar powers directly');
      // The tip rides the summary line instead of taking a line of its own.
      expect(t.closest('p')!.textContent).toContain(' of energy in came from the grid');
      expect(card.querySelector('[data-testid="balance-ac-note"]')).toBeNull();
      expect(screen.queryByRole('button', { name: 'More about unmeasured solar' })).toBeNull();
      // The solar share would still flatter an unmeasured PV: it stays out of
      // the summary line, the grid share still applies.
      expect(card.textContent).toContain(' of energy in came from the grid');
      expect(card.textContent).not.toContain('% of solar used or stored at home');
    }
  });

  // Issue #429 follow-up (BrianUK6): the page says nothing else about the bars
  // being for today, and rounded ends clipped the text in the end segments.
  it("titles the bars Today's Energy In and Today's Energy Out", () => {
    const { container } = renderBalance(snapshot());
    const bars = container.querySelectorAll('[data-testid="balance-bar"]');
    expect(bars).toHaveLength(2);
    expect(bars[0].textContent).toContain("Today's Energy In");
    expect(bars[1].textContent).toContain("Today's Energy Out");
  });

  it('draws the bars with a small corner radius so the end segments are not clipped', () => {
    const { container } = renderBalance(snapshot());
    const fills = container.querySelectorAll('[data-testid="balance-fill"]');
    expect(fills.length).toBeGreaterThan(0);
    for (const track of new Set([...fills].map((fill) => fill.parentElement!))) {
      // Softened corners, but never a large radius that would clip the end text.
      expect(track.className).toMatch(/(^|\s)rounded(\s|$)/);
      expect(track.className).not.toMatch(/rounded-(md|lg|xl|2xl|3xl|full)/);
    }
  });

  it('summarises every value and both totals for screen readers', () => {
    const { container } = renderBalance(snapshot());
    const card = container.querySelector('[data-testid="today-balance"]')!;
    const list = card.querySelector('ul.sr-only')!;
    expect(list.textContent).toContain("Today's Energy In 15.0kWh");
    expect(list.textContent).toContain("Today's Energy Out 12.5kWh");
    for (const label of ['Solar', 'Grid Import', 'Battery Out', 'Home Use', 'Grid Export', 'Battery In']) {
      expect(list.textContent).toContain(label);
    }
  });

  it('keeps the Today heading off phones, with no help on it', () => {
    const { container } = renderBalance(snapshot());
    const heading = container.querySelector('h2')!;
    expect(heading.textContent).toBe('Today');
    expect(heading.className).toContain('hidden');
    expect(heading.className).toContain('md:block');
    // The ⓘ moved to the summary line (issue #429 round 2): nothing is
    // attached to the heading any more.
    expect(heading.getAttribute('aria-describedby')).toBeNull();
    expect(heading.querySelector('button')).toBeNull();
  });

  it('colours fills with theme variables, never literals', () => {
    const { container } = renderBalance(snapshot());
    const coloured = [
      ...container.querySelectorAll<HTMLElement>('[data-testid="balance-fill"]'),
    ];
    const colours = coloured.map((el) => el.style.backgroundColor || el.getAttribute('style')!);
    expect(colours.length).toBeGreaterThan(0);
    for (const c of colours) {
      expect(c.startsWith('var(--app-fill-')).toBe(true);
    }
    const unique = new Set(colours);
    expect(unique.has('var(--app-fill-solar)')).toBe(true);
    expect(unique.has('var(--app-fill-grid)')).toBe(true);
    expect(unique.has('var(--app-fill-battery)')).toBe(true);
    expect(unique.has('var(--app-fill-home)')).toBe(true);
    expect(unique.has('var(--app-fill-export)')).toBe(true);
  });

  it.each(['2201', '7001', '8201'])('keeps unmeasured solar out of the tip on %s, which measures its own', (code) => {
    renderBalance(snapshot({ device_type_code: code }));
    const t = screen.getByRole('button', { name: "More about Today's Energy Balance" });
    expect(document.getElementById(t.getAttribute('aria-describedby')!)!.textContent).not.toContain("isn't measured");
  });
});
