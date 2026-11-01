import { describe, it, expect } from 'vitest';
import { LOGS_ITEM, NAV_ITEMS, visibleNavItems, type NavVisibility } from '../../src/lib/navItems';

/**
 * The page list shared by the bottom bar and the house view's drawer
 * (issue #422): both must follow the same Octopus, Logs, `?RO` and
 * hidden-panel rules.
 */

const base: NavVisibility = { readOnly: false, hiddenPanels: [], octopusConfigured: false, developerMode: false };
const labels = (v: Partial<NavVisibility>) => visibleNavItems({ ...base, ...v }).map((i) => i.label);

describe('visibleNavItems', () => {
  it('lists every page except Octopus and Logs by default, in order', () => {
    expect(labels({})).toEqual(['Status', 'Power', 'Battery', 'Solar', 'Inverter', 'Meters', 'History', 'Planner', 'Control', 'Settings']);
  });

  it('adds Octopus once it is configured', () => {
    expect(labels({ octopusConfigured: true })).toContain('Octopus');
  });

  it('adds Logs last in developer mode', () => {
    const items = visibleNavItems({ ...base, developerMode: true });
    expect(items[items.length - 1]).toBe(LOGS_ITEM);
  });

  it('hides Control and Settings in read-only mode', () => {
    const l = labels({ readOnly: true, octopusConfigured: true });
    expect(l).not.toContain('Control');
    expect(l).not.toContain('Settings');
    expect(l).toContain('Octopus');
  });

  it('hides panels hidden in Settings but never Status', () => {
    const l = labels({ hiddenPanels: ['power', 'history', 'octopus'], octopusConfigured: true });
    expect(l).not.toContain('Power');
    expect(l).not.toContain('History');
    expect(l).not.toContain('Octopus');
    expect(labels({ hiddenPanels: [''] })).toContain('Status');
  });

  it('keeps the master list free of Logs', () => {
    expect(NAV_ITEMS.some((i) => i.to === '/logs')).toBe(false);
  });
});
