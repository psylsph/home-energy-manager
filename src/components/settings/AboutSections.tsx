import { useCallback } from 'react';
import { apiPost } from '../../lib/api';
import { openExternal } from '../../lib/openExternal';
import { useInverterStore } from '../../store/useInverterStore';
import Toggle from '../Toggle';
import { SettingsSection } from './SettingsSection';
import { useSettingToggle, type Flash } from './useSettingsSave';

const UPDATE_TEXT = {
  on: 'Will check for new releases',
  off: 'Update checking is now off',
  fail: 'Failed to update update-checking',
};

/** Settings → Updates: current/latest version and the "new release" check. */
export function UpdatesSection({
  id,
  flash,
  initialCheckForUpdates,
}: {
  id?: string;
  flash: Flash;
  initialCheckForUpdates: boolean;
}) {
  const latestVersionInfo = useInverterStore((s) => s.latestVersionInfo);
  // The backend reads `check_for_updates` each cycle. Turning it off also
  // clears the cached info so the banner hides at once; a fresh check
  // repopulates it if it's turned back on.
  const persist = useCallback(async (next: boolean) => {
    await apiPost('/api/settings', { check_for_updates: next });
    if (!next) useInverterStore.getState().setLatestVersionInfo(null);
  }, []);
  const [checkForUpdates, setCheckForUpdates] = useSettingToggle(initialCheckForUpdates, persist, flash, UPDATE_TEXT);

  return (
    <SettingsSection
      id={id}
      title="Updates"
      description="The app checks GitHub for new releases and shows a dismissible banner when one is available. It doesn’t install anything automatically — clicking the banner opens the release page where you can download the update."
    >
      <div className="text-sm font-sans text-text-secondary flex flex-wrap items-center gap-x-2 gap-y-1">
        <span>
          Current version:{' '}
          <span className="font-mono text-text-primary">v{latestVersionInfo?.current_version ?? __APP_VERSION__}</span>
        </span>
        {latestVersionInfo && !latestVersionInfo.disabled && latestVersionInfo.latest_version && (
          <>
            <span aria-hidden="true" className="text-text-secondary/50">·</span>
            <span>
              Latest release:{' '}
              <span className="font-mono text-text-primary">v{latestVersionInfo.latest_version}</span>
              {latestVersionInfo.update_available && (
                <button
                  type="button"
                  onClick={() => void openExternal(latestVersionInfo.release_url ?? 'https://github.com/psylsph/home-energy-manager/releases/latest')}
                  className="ml-2 rounded-full bg-amber-500/30 hover:bg-amber-500/50 border border-amber-500/30 px-2.5 py-0.5 text-xs font-semibold text-amber-100 transition-colors"
                >
                  View Release
                </button>
              )}
            </span>
          </>
        )}
      </div>
      <div className="flex items-start justify-between gap-3">
        <div className="flex flex-col gap-0.5">
          <span className="text-text-primary text-sm font-sans font-medium">Check for New Releases</span>
          <span className="text-text-secondary text-xs font-sans">
            When on, the app contacts GitHub once every few hours to look for a newer version. The only information sent is your IP address — no usage data. Turn this off if you’d rather the app never reach out.
          </span>
        </div>
        <div className="shrink-0 mt-1">
          <Toggle
            checked={checkForUpdates}
            onChange={(v) => void setCheckForUpdates(v)}
            ariaLabel="Check for New Releases"
          />
        </div>
      </div>
    </SettingsSection>
  );
}

/** Settings → Developer: the developer-mode switch (stored in the browser). */
export function DeveloperSection({ id }: { id?: string }) {
  const developerMode = useInverterStore((s) => s.developerMode);
  const setDeveloperMode = useInverterStore((s) => s.setDeveloperMode);
  return (
    <SettingsSection id={id} title="Developer">
      <div className="flex items-center justify-between">
        <span className="text-text-primary text-sm font-sans">Developer Mode</span>
        <Toggle checked={developerMode} onChange={setDeveloperMode} ariaLabel="Developer Mode" />
      </div>
    </SettingsSection>
  );
}

export function AboutSection({ id }: { id?: string }) {
  return (
    <SettingsSection id={id} title="About">
      <button
        type="button"
        onClick={() => openExternal('https://psylsph.github.io/home-energy-manager/')}
        className="text-accent text-sm font-sans hover:underline text-left"
      >
        psylsph.github.io/home-energy-manager
      </button>
    </SettingsSection>
  );
}
