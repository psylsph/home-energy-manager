import { useCallback, useEffect, useState } from 'react';
import { apiGet, apiPost } from '../../lib/api';
import { openExternal } from '../../lib/openExternal';
import { useInverterStore } from '../../store/useInverterStore';
import Toggle from '../Toggle';
import AlertThresholds from './notifications/AlertThresholds';
import AlertTriggers from './notifications/AlertTriggers';
import NtfySettings from './notifications/NtfySettings';
import PushoverSettings from './notifications/PushoverSettings';
import TelegramSettings from './notifications/TelegramSettings';
import {
  DEFAULT_ALERTS_CONFIG,
  hasChannel,
  ntfyTopic,
  type AlertsConfig,
} from './notifications/alertsConfig';
import { SettingsSection } from './SettingsSection';
import { errorText, useSettingsSave, type Flash } from './useSettingsSave';

/**
 * Settings → Notifications: the alert providers (Telegram, ntfy, Pushover),
 * thresholds and triggers. The config has its own endpoint, so it loads
 * here rather than with the page's settings.
 */
export default function NotificationsSection({
  id,
  flash,
  serial,
}: {
  id?: string;
  flash: Flash;
  /** The saved inverter serial, for the default ntfy topic. */
  serial: string;
}) {
  // Only these two snapshot fields matter here, so the form doesn't
  // re-render on every poll.
  const inverterSerial = useInverterStore((s) => s.snapshot?.inverter_serial ?? '');
  const isGateway = useInverterStore((s) => s.snapshot?.device_type_code?.startsWith('70') ?? false);
  const [config, setConfig] = useState<AlertsConfig>(DEFAULT_ALERTS_CONFIG);
  // The form renders before the saved config arrives, so Save waits for it:
  // saving the defaults would blank the saved tokens. A failed load leaves
  // the form locked for the same reason.
  const [loaded, setLoaded] = useState(false);
  const [loadFailed, setLoadFailed] = useState(false);
  const save = useSettingsSave(flash);
  const test = useSettingsSave(flash);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: { config: Partial<AlertsConfig> } }>('/api/alerts');
        if (cancelled) return;
        if (res.ok && res.data?.config) {
          setConfig((current) => ({ ...current, ...res.data.config }));
          setLoaded(true);
        } else {
          setLoadFailed(true);
        }
      } catch (e: unknown) {
        console.warn('Failed to load alerts config:', e);
        if (!cancelled) setLoadFailed(true);
      }
    })();
    return () => { cancelled = true; };
  }, []);

  const update = useCallback((patch: Partial<AlertsConfig>) => {
    setConfig((p) => ({ ...p, ...patch }));
  }, []);

  const { generated, effective } = ntfyTopic(config, serial || inverterSerial);

  const handleSave = () =>
    save.run(async () => {
      // Persist the effective topic (the user's, or the serial default when
      // left blank) so ntfy works without the user typing one.
      const saveConfig = { ...config, ntfy_topic: effective };
      const res = await apiPost('/api/alerts', saveConfig) as { message: string; ok: boolean };
      flash(res.message, res.ok);
      // Mirror the saved inverter-temperature thresholds into the store so
      // the SystemAlertBanners re-render with them at once (issue #183).
      if (res.ok) {
        useInverterStore.getState().setInverterTempConfig({
          inverter_temp_min: Number(saveConfig.inverter_temp_min),
          inverter_temp_max: Number(saveConfig.inverter_temp_max),
        });
      }
    }, () => 'Failed to save alert settings');

  const handleTest = () =>
    test.run(async () => {
      const res = await apiPost('/api/alerts/test', {}) as { ok: boolean; message: string };
      flash(res.message, res.ok);
    }, () => 'Failed to Send Message — check API key and settings');

  return (
    // Inert until the saved config has loaded: anything typed earlier would
    // be replaced by the load, and a Save would post the defaults.
    <SettingsSection
      id={id}
      title="Notifications"
      inert={!loaded}
      action={
        <button
          onClick={() => openExternal('https://github.com/psylsph/home-energy-manager/blob/master/NOTIFICATIONS.md')}
          className="text-accent text-xs font-sans underline hover:opacity-80 transition-opacity"
        >
          Setup Guide ↗
        </button>
      }
    >
      <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
        <div className="flex items-center justify-between">
          <span className="text-text-primary text-sm font-sans">Enable Alerts</span>
          <Toggle
            checked={config.enabled}
            ariaLabel="Enable Alerts"
            onChange={(v) => {
              update({ enabled: v });
              apiPost('/api/alerts', { enabled: v })
                .then(() => flash(v ? 'Alerts enabled' : 'Alerts disabled', true))
                .catch((e) => {
                  update({ enabled: !v });
                  flash(errorText(e, 'Failed to save'), false);
                });
            }}
          />
        </div>
      </div>

      {config.enabled && (
        <div className="flex flex-col gap-4">
          <TelegramSettings config={config} update={update} />
          <NtfySettings
            config={config}
            update={update}
            generatedTopic={generated}
            effectiveTopic={effective}
            onCopyTopic={() => { void navigator.clipboard.writeText(effective); flash('Topic copied!', true); }}
          />
          <PushoverSettings config={config} update={update} />
          <AlertThresholds config={config} update={update} isGateway={isGateway} />
          <AlertTriggers config={config} update={update} />
        </div>
      )}

      {loadFailed && (
        <p className="text-amber-300 text-xs font-sans">
          The saved notification settings could not be loaded, so this section is turned off to avoid overwriting them. Reload the page to try again.
        </p>
      )}
      <div className="flex flex-col sm:flex-row gap-2">
        <button
          onClick={() => void handleSave()}
          disabled={save.busy || !loaded}
          className="bg-accent text-on-accent font-sans font-semibold text-sm px-4 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 transition-opacity sm:w-auto"
        >
          {save.busy ? 'Saving…' : 'Save Notification Settings'}
        </button>
        <button
          onClick={() => void handleTest()}
          disabled={test.busy || !hasChannel(config, effective)}
          className="bg-bg-elevated text-text-primary font-sans font-semibold text-sm px-4 py-2 rounded-lg hover:opacity-80 disabled:opacity-40 transition-opacity border border-white/5 sm:w-auto"
        >
          {test.busy ? 'Sending…' : 'Send Test Notification'}
        </button>
      </div>
    </SettingsSection>
  );
}
