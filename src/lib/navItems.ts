/**
 * Which pages the app's navigation offers, shared by the bottom bar and the
 * burger-menu drawer used with the house view (issue #422), so the two can
 * never list different pages.
 */
import { FLOW_COLORS } from './energyFlow';

export interface NavItem {
  to: string;
  label: string;
  /** Colour of the active link; `undefined` uses the theme accent. */
  accent?: string;
}

export const NAV_ITEMS: readonly NavItem[] = [
  { to: '/', label: 'Status', accent: FLOW_COLORS.home },
  { to: '/power', label: 'Power', accent: FLOW_COLORS.home },
  { to: '/battery', label: 'Battery', accent: FLOW_COLORS.battery },
  { to: '/solar', label: 'Solar', accent: FLOW_COLORS.solar },
  { to: '/inverter', label: 'Inverter', accent: FLOW_COLORS.inverter },
  { to: '/meters', label: 'Meters', accent: FLOW_COLORS.grid },
  { to: '/history', label: 'History' },
  { to: '/forecast', label: 'Planner', accent: FLOW_COLORS.solar },
  { to: '/octopus', label: 'Octopus', accent: '#ec4899' },
  { to: '/control', label: 'Control' },
  { to: '/settings', label: 'Settings' },
];

export const LOGS_ITEM: NavItem = { to: '/logs', label: 'Logs' };

export interface NavVisibility {
  /** `?RO` read-only mode hides Control and Settings (issue #114). */
  readOnly: boolean;
  /** Panels hidden in Settings → Panel Controls. */
  hiddenPanels: readonly string[];
  /** The Octopus page exists only once an account number and key are saved. */
  octopusConfigured: boolean;
  /** Logs appears only in developer mode. */
  developerMode: boolean;
}

/** The pages to offer, in order. Status is never hidden. */
export function visibleNavItems(v: NavVisibility): NavItem[] {
  const items = NAV_ITEMS.filter((item) => {
    const key = item.to.replace(/^\//, '');
    if (v.readOnly && (key === 'control' || key === 'settings')) return false;
    if (key === 'octopus' && !v.octopusConfigured) return false;
    return !key || !v.hiddenPanels.includes(key);
  });
  return v.developerMode ? [...items, LOGS_ITEM] : items;
}
