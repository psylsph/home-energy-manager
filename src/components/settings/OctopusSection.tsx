import { useState } from 'react';
import { apiPost } from '../../lib/api';
import { openExternal } from '../../lib/openExternal';
import type { PollSettings } from '../../lib/types';
import Toggle from '../Toggle';
import { SettingsSection } from './SettingsSection';
import { useSettingsSave, type Flash } from './useSettingsSave';
import HelpTip from '../HelpTip';

type GasUnit = 'unknown' | 'kwh' | 'm3';

const INPUT =
  'w-full rounded-lg border border-white/10 bg-bg-elevated px-3 py-2 text-sm text-text-primary outline-none focus:border-accent';
const FIELD_LABEL = 'mb-1 block text-xs font-medium text-text-secondary';

/**
 * Settings → Octopus Energy Data (issue #212). The API key is write-only:
 * GET /api/settings says only whether one is saved.
 */
export default function OctopusSection({
  id,
  flash,
  initial,
  onConfiguredChange,
}: {
  id?: string;
  flash: Flash;
  initial: Partial<PollSettings>;
  /** Told after each save whether the integration is now on. */
  onConfiguredChange: (configured: boolean) => void;
}) {
  const [account, setAccount] = useState(initial.octopus_account_number ?? '');
  const [apiKey, setApiKey] = useState('');
  const [keyConfigured, setKeyConfigured] = useState(Boolean(initial.octopus_api_key_configured));
  const [gasUnit, setGasUnit] = useState<GasUnit>(initial.octopus_gas_unit ?? 'unknown');
  const [gasEnabled, setGasEnabled] = useState(initial.octopus_gas_enabled ?? true);
  const [e7Start, setE7Start] = useState(initial.octopus_economy7_start ?? '00:30');
  const [e7End, setE7End] = useState(initial.octopus_economy7_end ?? '07:30');
  const { busy: saving, run } = useSettingsSave(flash);

  const handleSave = () =>
    run(async () => {
      const trimmedAccount = account.trim();
      const newKey = apiKey.trim();
      // Saved credentials are the only switch: with both present the
      // integration is on, and clearing the account number turns it off.
      const configured = trimmedAccount.length > 0 && (newKey.length > 0 || keyConfigured);
      const body: Record<string, unknown> = {
        octopus_account_number: trimmedAccount,
        octopus_gas_unit: gasUnit,
        octopus_gas_enabled: gasEnabled,
        octopus_economy7_start: e7Start,
        octopus_economy7_end: e7End,
      };
      if (newKey) body.octopus_api_key = newKey;
      await apiPost('/api/settings', body);
      if (newKey) setKeyConfigured(true);
      setApiKey('');
      onConfiguredChange(configured);
      window.dispatchEvent(new Event('octopus-settings-changed'));
      if (!configured) {
        return 'Octopus settings saved. Add both the account number and API key to switch the integration on.';
      }
      await apiPost('/api/octopus/sync');
      return 'Octopus settings saved — consumption sync started';
    }, 'Failed to save Octopus settings');

  return (
    <SettingsSection
      id={id}
      title="Octopus Energy Data"
      description="Adds an Octopus tab with your smart-meter readings, costs and Saving Sessions."
      help="Save your Octopus account number and API key to add an Octopus tab with your smart-meter readings, costs and Saving Sessions alerts. Clear the account number and save to switch it off."
    >
      <div className="grid gap-4 sm:grid-cols-2">
        <label className="block">
          <span className={FIELD_LABEL}>Account Number</span>
          <input value={account} onChange={(e) => setAccount(e.target.value)} placeholder="A-1234ABCD" autoComplete="off" className={INPUT} />
        </label>
        <label className="block">
          <span className={FIELD_LABEL}>API Key</span>
          <input type="password" value={apiKey} onChange={(e) => setApiKey(e.target.value)} placeholder={keyConfigured ? 'Saved — enter a new key to replace' : 'sk_live_…'} autoComplete="new-password" className={INPUT} />
        </label>
      </div>
      <button
        type="button"
        onClick={() => openExternal('https://octopus.energy/dashboard/new/accounts/personal-details/api-access')}
        className="self-start text-sm text-accent underline hover:opacity-80"
      >
        Get Your Octopus API Key
      </button>
      <div className="flex items-center justify-between gap-4 rounded-lg bg-bg-elevated px-4 py-3">
        <span>
          <span className="block text-sm font-medium text-text-primary">
            My Home Has Gas{' '}
            <HelpTip label="My Home Has Gas">
              Switch off if you are electricity only. Gas readings are not fetched and gas is hidden from the Octopus
              page and its exports.
            </HelpTip>
          </span>
          <span className="block text-xs text-text-secondary">Switch off if you are electricity only.</span>
        </span>
        <Toggle checked={gasEnabled} onChange={setGasEnabled} ariaLabel="My Home Has Gas" />
      </div>
      {gasEnabled && (
        <div className="max-w-sm">
          <label className="block">
            <span className={FIELD_LABEL}>Gas API Unit</span>
            <select value={gasUnit} onChange={(e) => setGasUnit(e.target.value as GasUnit)} className={INPUT}>
              <option value="unknown">Unknown — Do Not Calculate Gas Cost</option>
              <option value="kwh">kWh — Calculate Gas Cost</option>
              <option value="m3">m³ — Usage Only</option>
            </select>
          </label>
          {/* Outside the <label>, so the ⓘ's name isn't read as part of the select's. */}
          <span className="mt-1 block text-xs text-text-secondary">
            Choose kWh only if your readings are already converted.{' '}
            <HelpTip label="Gas API Unit">
              Octopus does not label gas units in this API. Only choose kWh if your downloaded readings are already
              converted; m³ needs billing conversion data that the API does not provide.
            </HelpTip>
          </span>
        </div>
      )}
      <div className="max-w-sm">
        <span className={FIELD_LABEL}>Economy 7 Night-Rate Window</span>
        <div className="grid grid-cols-2 gap-3">
          <label className="block">
            <span className="sr-only">Economy 7 Night Rate Starts</span>
            <input type="time" value={e7Start} onChange={(e) => setE7Start(e.target.value)} aria-label="Economy 7 Night Rate Starts" className={INPUT} />
          </label>
          <label className="block">
            <span className="sr-only">Economy 7 Night Rate Ends</span>
            <input type="time" value={e7End} onChange={(e) => setE7End(e.target.value)} aria-label="Economy 7 Night Rate Ends" className={INPUT} />
          </label>
        </div>
        <span className="mt-1 block text-xs text-text-secondary">
          Only for historical E-2R tariffs, in UK time.{' '}
          <HelpTip label="Economy 7 Night-Rate Window">
            Used only for historical E-2R tariffs. Times follow the Europe/London clock; check your meter or bill
            because Economy 7 hours can vary.
          </HelpTip>
        </span>
      </div>
      <p className="text-xs text-text-secondary">
        Find these in your Octopus dashboard. The key is stored locally, never displayed again, and never written to logs.
      </p>
      <button type="button" onClick={() => void handleSave()} disabled={saving} className="self-start rounded-lg bg-accent px-4 py-2 text-sm font-medium text-on-accent disabled:opacity-50">
        {saving ? 'Saving…' : 'Save Octopus Settings'}
      </button>
    </SettingsSection>
  );
}
