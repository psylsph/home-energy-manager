import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/react';
import {
  CAR_CHARGING_COLOR,
  CAR_CHARGING_NAME,
  solidLegendLabel,
} from '../../src/lib/carChargingLegend';

describe('solidLegendLabel', () => {
  it('draws the Car Charging label in the solid colour', () => {
    const { container } = render(<>{solidLegendLabel(CAR_CHARGING_NAME)}</>);
    const span = container.querySelector('span')!;
    expect(span.textContent).toBe('Car Charging');
    expect(span.style.color).toBe('rgb(192, 132, 252)');
    expect(CAR_CHARGING_COLOR).toBe('#c084fc');
  });

  it('leaves the other labels to the legend', () => {
    expect(solidLegendLabel('Weekday Median')).toBe('Weekday Median');
  });
});
