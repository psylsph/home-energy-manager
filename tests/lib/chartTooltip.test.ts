import { describe, it, expect } from 'vitest';
import { tooltipStyleProps } from '../../src/lib/chartTooltip';

// ---------------------------------------------------------------------------
// Shared Recharts tooltip styling (issue #283 feedback): the chart pages
// were switched to a theme-aware tooltip in #232; the Forecast charts
// must not fall back to Recharts' default white box (light text on white
// in dark mode). Pins the shared constant so a future "simplification"
// can't silently reintroduce the unreadable tooltip.
// ---------------------------------------------------------------------------

describe('tooltipStyleProps', () => {
  it('uses the theme-aware elevated surface, not the default white box', () => {
    expect(tooltipStyleProps.contentStyle.backgroundColor).toBe('var(--app-bg-elevated)');
  });

  it('renders the label (time) in the theme text colour', () => {
    // The time label inherits the page text colour by default — light in
    // dark mode — which is what made it unreadable on the white box.
    expect(tooltipStyleProps.labelStyle.color).toBe('var(--app-text-primary)');
  });
});
