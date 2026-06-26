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

  it('hides the numeric label below the label-width threshold', () => {
    const { container } = render(<BatteryGauge soc={50} width={60} />);
    // showLabel defaults true but gates on width >= 72.
    expect(container.querySelector('text')).toBeNull();
  });
});
