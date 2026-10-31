import { useCallback, useState } from 'react';
import { apiPost } from '../../lib/api';
import { clampInterval, VALID_INTERVALS } from '../../lib/settingsForm';
import Toggle from '../Toggle';
import { AdvancedSettings, SettingsRow, SettingsSection } from './SettingsSection';
import { useSettingsSave, useSettingToggle, type Flash } from './useSettingsSave';

const TRAY_TEXT = {
  on: 'Closing the window will now hide it to the tray',
  off: 'Closing the window will now quit the app',
  fail: 'Failed to update minimise-to-tray',
};
const START_MINIMISED_TEXT = {
  on: 'Will start hidden in the tray on next launch',
  off: 'Will show the window on next launch',
  fail: 'Failed to update start-minimised',
};

const ROW_LABEL = 'text-text-primary text-sm font-sans font-medium';
const HINT = 'text-text-secondary text-xs font-sans';

/** A labelled switch row that names the switch after its label. */
function SettingsToggle({
  label,
  hint,
  checked,
  onChange,
}: {
  label: string;
  hint: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <SettingsRow label={label} hint={hint} strong>
      <Toggle checked={checked} ariaLabel={label} onChange={onChange} />
    </SettingsRow>
  );
}

/**
 * Settings → App: how the app itself runs — poll interval, HTTP port, and
 * (desktop only) start on login and the tray behaviour.
 */
export default function AppSection({
  id,
  flash,
  initial,
}: {
  id?: string;
  flash: Flash;
  initial: {
    intervalSecs: number;
    httpPort: number;
    autostartEnabled: boolean;
    minimiseToTray: boolean;
    startMinimised: boolean;
  };
}) {
  const [intervalSecs, setIntervalSecs] = useState(() => clampInterval(initial.intervalSecs));
  const [httpPort, setHttpPort] = useState<number | ''>(initial.httpPort);
  const [autostartEnabled, setAutostartEnabled] = useState(initial.autostartEnabled);
  const { run } = useSettingsSave(flash);
  // The autostart and tray controls need the Tauri shell; headless / Pi
  // users manage the systemd service instead (see INSTALL.md).
  const desktop = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

  // Issue #217: minimise to tray applies on the next close (the Rust close
  // handler reads it live); start minimised applies on the next launch.
  const persistTray = useCallback((next: boolean) => apiPost('/api/settings', { minimise_to_tray: next }), []);
  const persistStartMinimised = useCallback((next: boolean) => apiPost('/api/settings', { start_minimised: next }), []);
  const [minimiseToTray, setMinimiseToTray] = useSettingToggle(initial.minimiseToTray, persistTray, flash, TRAY_TEXT);
  const [startMinimised, setStartMinimised] = useSettingToggle(initial.startMinimised, persistStartMinimised, flash, START_MINIMISED_TEXT);

  const handleIntervalChange = (val: number) => {
    setIntervalSecs(val);
    return run(async () => {
      await apiPost('/api/settings', { interval_secs: val });
      return `Refresh interval set to ${val}s`;
    }, 'Failed to update interval');
  };

  const handleHttpPortSave = async () => {
    if (httpPort === '') {
      flash('HTTP port cannot be blank', false);
      return;
    }
    await run(async () => {
      await apiPost('/api/settings', { http_port: httpPort });
      return `HTTP port set to ${httpPort}. Restart required to take effect.`;
    }, 'Failed to update HTTP port');
  };

  // Issue #117. Apply the OS-level entry first so the saved preference
  // matches the platform; if the plugin fails (e.g. a registry ACL error on
  // Windows) fall back to a Startup-folder shortcut. If that fails too, the
  // switch moves back and the Rust self-heal retries on the next launch.
  const handleAutostartToggle = async (next: boolean) => {
    if (!desktop) return;
    const previous = autostartEnabled;
    setAutostartEnabled(next);
    try {
      const { enable, disable } = await import('@tauri-apps/plugin-autostart');
      if (next) await enable();
      else await disable();
      await apiPost('/api/settings', { autostart_enabled: next });
      flash(
        next
          ? 'Will start automatically when you log in'
          : 'Will no longer start automatically when you log in',
        true,
      );
    } catch (e) {
      try {
        const { invoke } = await import('@tauri-apps/api/core');
        await invoke('autostart_fallback', { enable: next });
        await apiPost('/api/settings', { autostart_enabled: next });
        flash(
          next
            ? 'Will start automatically when you log in (Startup folder)'
            : 'Will no longer start automatically when you log in',
          true,
        );
      } catch (e2) {
        setAutostartEnabled(previous);
        const msg = e instanceof Error ? e.message : String(e);
        const msg2 = e2 instanceof Error ? e2.message : String(e2);
        flash(`Failed to ${next ? 'enable' : 'disable'} auto-start: ${msg} / ${msg2}`, false);
      }
    }
  };

  return (
    <SettingsSection id={id} title="App">
      {desktop && (
        <>
          <SettingsToggle
            label="Start on Login"
            hint="Launch the app automatically when you log in. On Windows this adds an entry to the Startup apps list; on macOS a LaunchAgent; on Linux a desktop autostart file."
            checked={autostartEnabled}
            onChange={(v) => void handleAutostartToggle(v)}
          />
          <SettingsToggle
            label="Minimise to Tray"
            hint="Keep the app running in the background. Closing the window hides it to the system tray instead of quitting — reopen it from the tray icon."
            checked={minimiseToTray}
            onChange={(v) => void setMinimiseToTray(v)}
          />
          <SettingsToggle
            label="Start Hidden in Tray"
            hint="Launch with the window already hidden in the tray. Handy alongside Start on Login so the app boots quietly in the background."
            checked={startMinimised}
            onChange={(v) => void setStartMinimised(v)}
          />
        </>
      )}

      {/* Rarely changed: most installs never touch these. */}
      <AdvancedSettings>
          <div className="flex flex-col gap-2">
            <span className={ROW_LABEL}>Refresh Interval</span>
            <p className={HINT}>How often the app polls the inverter for fresh data.</p>
            <p className={HINT}>
              Each poll also reaches the GivEnergy cloud through the WiFi dongle, which stores it as a
              data point, so a short interval makes your cloud history much larger.
            </p>
            <div className="flex gap-2">
              {VALID_INTERVALS.map((s) => (
                <button
                  key={s}
                  onClick={() => void handleIntervalChange(s)}
                  className={`flex-1 py-2 rounded-lg text-sm font-mono transition ${
                    intervalSecs === s
                      ? 'bg-accent text-on-accent font-semibold'
                      : 'bg-bg-elevated text-text-primary hover:bg-bg-elevated/80'
                  }`}
                >
                  {s}s
                </button>
              ))}
            </div>
          </div>

          <div className="flex flex-col gap-2">
            <span className={ROW_LABEL}>HTTP Port</span>
            <p className={HINT}>Change to run multiple instances on the same machine. Requires restart.</p>
            <div className="flex items-center gap-3">
              <input
                type="number"
                min={1024}
                max={65535}
                value={httpPort}
                onChange={(e) => setHttpPort(e.target.value === '' ? '' : Number(e.target.value))}
                className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-28 border border-transparent focus:outline-none focus:border-accent"
              />
              <button
                onClick={() => void handleHttpPortSave()}
                className="bg-accent text-on-accent font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 transition-opacity"
              >
                Save
              </button>
            </div>
          </div>
      </AdvancedSettings>
    </SettingsSection>
  );
}
