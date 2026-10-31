// The Settings page's navigation: sections grouped by what the user came to do.

export interface SettingsNavGroup {
  label: string;
  sections: { id: string; label: string }[];
}

/** Section ids, in page order. */
export const SETTINGS_SECTION = {
  connection: 'settings-connection',
  evCharger: 'settings-ev-charger',
  remoteAccess: 'settings-remote-access',
  octopus: 'settings-octopus',
  tariffs: 'settings-tariffs',
  solarArrays: 'settings-solar-arrays',
  weather: 'settings-weather',
  notifications: 'settings-notifications',
  panelControls: 'settings-panel-controls',
  app: 'settings-app',
  updates: 'settings-updates',
  backups: 'settings-backups',
  developer: 'settings-developer',
  about: 'settings-about',
} as const;

const S = SETTINGS_SECTION;

export const SETTINGS_NAV: SettingsNavGroup[] = [
  {
    label: 'Connection',
    sections: [
      { id: S.connection, label: 'Inverter' },
      { id: S.evCharger, label: 'EV Charger' },
      { id: S.remoteAccess, label: 'Remote & API Access' },
    ],
  },
  {
    label: 'Energy & Cost',
    sections: [
      { id: S.octopus, label: 'Octopus' },
      { id: S.tariffs, label: 'Tariffs' },
      { id: S.solarArrays, label: 'Solar Arrays' },
      { id: S.weather, label: 'Weather' },
    ],
  },
  { label: 'Alerts', sections: [{ id: S.notifications, label: 'Notifications' }] },
  { label: 'Display', sections: [{ id: S.panelControls, label: 'Panel Controls' }] },
  {
    label: 'App',
    sections: [
      { id: S.app, label: 'App Behaviour' },
      { id: S.updates, label: 'Updates' },
      { id: S.backups, label: 'History Backup' },
      { id: S.developer, label: 'Developer' },
      { id: S.about, label: 'About' },
    ],
  },
];

/**
 * The section the reader is in: the last one whose top has passed `line`
 * (pixels from the viewport top), or the last section outright once the page
 * is scrolled to the bottom, where short final sections can never reach the
 * line. `tops` are the sections' viewport tops, in the order of `ids`.
 */
export function activeSectionAt(ids: string[], tops: number[], line: number, atBottom: boolean): string {
  if (ids.length === 0) return '';
  if (atBottom) return ids[ids.length - 1]!;
  let active = ids[0]!;
  ids.forEach((id, i) => {
    if ((tops[i] ?? Infinity) <= line) active = id;
  });
  return active;
}

/** The group holding section `id`, or the first group. */
export function groupOf(groups: SettingsNavGroup[], id: string): SettingsNavGroup | undefined {
  return groups.find((g) => g.sections.some((s) => s.id === id)) ?? groups[0];
}
