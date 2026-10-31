import { useCallback, useEffect, useRef, useState } from 'react';
import { apiGet } from '../lib/api';
import { octopusConfigured } from '../lib/settingsForm';
import type { PollSettings } from '../lib/types';
import HistoryBackupPanel from '../components/HistoryBackupPanel';
import { AboutSection, DeveloperSection, UpdatesSection } from '../components/settings/AboutSections';
import AppSection from '../components/settings/AppSection';
import ConnectionSection from '../components/settings/ConnectionSection';
import EvChargerSection from '../components/settings/EvChargerSection';
import NotificationsSection from '../components/settings/NotificationsSection';
import OctopusSection from '../components/settings/OctopusSection';
import PanelControlsSection from '../components/settings/PanelControlsSection';
import RemoteAccessSection from '../components/settings/RemoteAccessSection';
import SolarArraysSection from '../components/settings/SolarArraysSection';
import TariffsSection from '../components/settings/TariffsSection';
import WeatherSection from '../components/settings/WeatherSection';
import SettingsNav from '../components/settings/SettingsNav';
import { SETTINGS_NAV, SETTINGS_SECTION } from '../lib/settingsNav';
import type { Flash } from '../components/settings/useSettingsSave';

const FLASH_MS = 4000;

/**
 * The Settings page: loads the settings once and lays out the sections,
 * grouped by intent with a navigation rail (issue #402). Each section owns
 * its form and saves. Sections mount only after the load,
 * so each seeds its fields from `initial` and no Save can post defaults.
 */
export default function SettingsPage() {
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  // The settings as loaded. A failed load leaves this empty and every
  // section on its defaults.
  const [initial, setInitial] = useState<Partial<PollSettings>>({});
  // Whether the Octopus tab exists, for Panel Visibility; follows saves.
  const [octopusOn, setOctopusOn] = useState(false);
  // The inverter serial, for the default ntfy topic; follows Connect.
  const [serial, setSerial] = useState('');
  const [message, setMessage] = useState<{ text: string; ok: boolean } | null>(null);
  const flashTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => () => {
    if (flashTimer.current !== null) {
      clearTimeout(flashTimer.current);
      flashTimer.current = null;
    }
  }, []);

  const flash = useCallback<Flash>((text, ok, opts) => {
    if (flashTimer.current !== null) {
      clearTimeout(flashTimer.current);
      flashTimer.current = null;
    }
    setMessage({ text, ok });
    if (opts?.sticky) return;
    flashTimer.current = setTimeout(() => {
      flashTimer.current = null;
      setMessage(null);
    }, FLASH_MS);
  }, []);

  useEffect(() => {
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: PollSettings }>('/api/settings');
        const s = res.data ?? {};
        setInitial(s);
        setOctopusOn(octopusConfigured(s));
        setSerial(s.serial ?? '');
      } catch (e: unknown) {
        console.warn('Failed to load settings:', e);
      }
      setSettingsLoaded(true);
    })();
  }, []);

  if (!settingsLoaded) {
    return (
      <div className="flex flex-col items-center justify-center min-h-[60vh] gap-4">
        <div className="w-10 h-10 border-4 border-accent border-t-transparent rounded-full animate-spin" />
        <p className="text-text-secondary text-sm font-sans">Loading Settings…</p>
      </div>
    );
  }

  const S = SETTINGS_SECTION;
  return (
    <div className="max-w-5xl mx-auto px-4 py-6 md:grid md:grid-cols-[12rem_minmax(0,1fr)] md:gap-8">
      {message && (
        <div
          data-testid="settings-flash"
          className={`fixed top-4 right-4 z-50 px-4 py-2 rounded-lg text-sm font-sans shadow-lg ${
            message.ok ? 'bg-green-800/80 text-green-200' : 'bg-red-800/80 text-red-200'
          }`}
        >
          {message.text}
        </div>
      )}

      <SettingsNav groups={SETTINGS_NAV} />

      <div className="flex flex-col gap-8 min-w-0 max-w-2xl">
        {/* Connection */}
        <ConnectionSection id={S.connection} flash={flash} initial={initial} onSerialSaved={setSerial} />
        <EvChargerSection
          id={S.evCharger}
          flash={flash}
          initialHost={initial.evc_host ?? ''}
          initialPort={initial.evc_port ?? 502}
        />
        <RemoteAccessSection id={S.remoteAccess} flash={flash} initial={initial} />

        {/* Energy & cost */}
        <OctopusSection id={S.octopus} flash={flash} initial={initial} onConfiguredChange={setOctopusOn} />
        <TariffsSection id={S.tariffs} flash={flash} initial={initial} />
        <SolarArraysSection id={S.solarArrays} flash={flash} initial={initial} />
        <WeatherSection id={S.weather} flash={flash} />

        {/* Alerts */}
        <NotificationsSection id={S.notifications} flash={flash} serial={serial} />

        {/* Display */}
        <PanelControlsSection
          id={S.panelControls}
          flash={flash}
          initialHiddenPanels={initial.hidden_panels ?? []}
          octopusConfigured={octopusOn}
        />

        {/* App */}
        <AppSection
          id={S.app}
          flash={flash}
          initial={{
            intervalSecs: initial.interval_secs ?? 20,
            httpPort: initial.http_port ?? 7337,
            autostartEnabled: initial.autostart_enabled ?? false,
            minimiseToTray: initial.minimise_to_tray ?? false,
            startMinimised: initial.start_minimised ?? false,
          }}
        />
        <UpdatesSection id={S.updates} flash={flash} initialCheckForUpdates={initial.check_for_updates ?? true} />
        <HistoryBackupPanel id={S.backups} />
        <DeveloperSection id={S.developer} />
        <AboutSection id={S.about} />
      </div>
    </div>
  );
}
