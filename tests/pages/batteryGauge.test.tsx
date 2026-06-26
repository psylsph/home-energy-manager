import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import BatteryGauge from '../../src/components/BatteryGauge';

/**
 * The AA-cell battery gauge replaced the circular SOC ring in BatteryPanel.
 * These render tests pin the contract the panel and the flow-diagram node
 * both depend on: the accessible label carries the SOC, a fill is drawn for
 * any non-zero SOC, and the fill height scales with the charge level.
 *
 * The fill-height *math* itself is covered by the `batteryFillFraction`
 * unit tests in energyFlow.test.ts; here we assert the SVG reflects it.
 */
describe('BatteryGauge', () => {
  it('exposes the SOC in an accessible label', () => {
    render(<BatteryGauge soc={97} />);
    expect(screen.getByRole('img', { name: /97% charged/i })).toBeTruthy();
  });

  it('renders a fill rect when SOC > 0', () => {
    const { container } = render(<BatteryGauge soc={50} />);
    // The fill rect is the only element carrying the inline transition
    // style; the body/terminal outlines have no style attr.
    const fills = container.querySelectorAll('rect[style]');
    expect(fills.length).toBe(1);
  });

  it('renders no fill rect when SOC is 0', () => {
    const { container } = render(<BatteryGauge soc={0} />);
    expect(container.querySelectorAll('rect[style]')).toHaveLength(0);
  });

  it('a higher SOC produces a taller fill than a lower one', () => {
    const { container: low } = render(<BatteryGauge soc={25} />);
    const { container: high } = render(<BatteryGauge soc={90} />);
    const lowFill = low.querySelector('rect[style]') as SVGSVGElement;
    const highFill = high.querySelector('rect[style]') as SVGSVGElement;
    const lowH = Number(lowFill.getAttribute('height'));
    const highH = Number(highFill.getAttribute('height'));
    expect(highH).toBeGreaterThan(lowH);
  });

  it('supports a horizontal mobile orientation with left-to-right fill', () => {
    const { container: low } = render(<BatteryGauge soc={25} orientation="horizontal" width={128} />);
    const { container: high } = render(<BatteryGauge soc={90} orientation="horizontal" width={128} />);
    expect(low.querySelector('svg')?.getAttribute('data-orientation')).toBe('horizontal');
    expect(low.querySelector('svg')?.getAttribute('viewBox')).toBe('0 0 80 40');
    const lowFill = low.querySelector('rect[style]') as SVGSVGElement;
    const highFill = high.querySelector('rect[style]') as SVGSVGElement;
    expect(Number(highFill.getAttribute('width'))).toBeGreaterThan(Number(lowFill.getAttribute('width')));
  });

  it('hides the numeric label below the label-width threshold', () => {
    const { container } = render(<BatteryGauge soc={50} width={60} />);
    // showLabel defaults true but gates on width >= 72.
    expect(container.querySelector('text')).toBeNull();
  });

  it('always uses the theme text colour (no dark-ink flip as the fill rises)', () => {
    // Regression guard for the transition-period bug: the label used to flip
    // to dark ink once the fill covered ~45% of the body, leaving the upper
    // half of the glyph unreadable against the dark background mid-rise.
    // Now it must follow --app-text-primary at every SOC.
    for (const soc of [5, 25, 45, 55, 75, 100]) {
      const { container } = render(<BatteryGauge soc={soc} />);
      const text = container.querySelector('text')!;
      expect(text.getAttribute('fill')).toBe('var(--app-text-primary, #E6EDF3)');
    }
  });

  it('scales the font down for the 4-char "100%" label and keeps it inside the body', () => {
    // "100%" overflows the body at the default font size; the 100% case
    // must use the smaller size and the glyph must not exceed the body width.
    const { container } = render(<BatteryGauge soc={100} />);
    const text = container.querySelector('text')!;
    expect(Number(text.getAttribute('font-size'))).toBe(10);
    // Body spans BODY_X(5)..(5+30)=35 in the 40-wide viewBox. A centred
    // "100%" at size 10 (≈0.6em advance × 4 chars = 24 units) fits within
    // the ~26-unit inner width with margin.
    const body = container.querySelectorAll('rect')[1];
    const bx = Number(body.getAttribute('x'));
    const bw = Number(body.getAttribute('width'));
    expect(bx).toBe(5);
    expect(bw).toBe(30);
  });

  it('uses the full font size for 2/3-char labels', () => {
    const { container } = render(<BatteryGauge soc={50} />);
    expect(Number(container.querySelector('text')!.getAttribute('font-size'))).toBe(12);
  });
});
