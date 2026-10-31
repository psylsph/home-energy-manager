import { useState } from 'react';
import { apiGet, apiPost } from '../../lib/api';
import type { DiscoveredInverter, PollSettings } from '../../lib/types';
import { scannedHost } from '../../lib/settingsForm';
import { isValidIpv4Host } from '../../lib/validators';
import { useInverterStore } from '../../store/useInverterStore';
import Toggle from '../Toggle';
import { DiscoveredList, HostField } from './HostField';
import { SettingsSection } from './SettingsSection';
import { TEXT_INPUT } from './styles';
import { errorText, useSettingsSave, type Flash } from './useSettingsSave';

function RestartModal({ host, onClose }: { host: string; onClose: () => void }) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div className="bg-bg-surface rounded-2xl p-6 max-w-sm mx-4 shadow-2xl">
        <h3 className="text-text-primary text-lg font-semibold mb-2">Restart Required</h3>
        <p className="text-text-secondary text-sm mb-6">
          The connection to <strong className="text-text-primary">{host}</strong> has been saved.
          Please restart the app for the changes to take full effect.
        </p>
        <button
          onClick={onClose}
          className="w-full py-2.5 bg-accent/20 text-accent rounded-lg text-sm font-medium hover:bg-accent/30 transition"
        >
          Got It
        </button>
      </div>
    </div>
  );
}

/** Settings → Inverter Connection: address, serial, network scan, auto-discovery. */
export default function ConnectionSection({
  id,
  flash,
  initial,
  onSerialSaved,
}: {
  id?: string;
  flash: Flash;
  initial: Partial<PollSettings>;
  /** Told the serial each time Connect saves one. */
  onSerialSaved?: (serial: string) => void;
}) {
  const connectionState = useInverterStore((s) => s.connectionState);
  const connectedHost = useInverterStore((s) => s.connectedHost);
  const developerMode = useInverterStore((s) => s.developerMode);
  const [host, setHost] = useState(initial.host ?? '');
  const [port, setPort] = useState(initial.port ?? 8899);
  const [serial, setSerial] = useState(initial.serial ?? '');
  const [disableAutoDiscovery, setDisableAutoDiscovery] = useState(initial.disable_auto_discovery ?? true);
  const [discovering, setDiscovering] = useState(false);
  const [discoverResults, setDiscoverResults] = useState<DiscoveredInverter[]>([]);
  const [discoverError, setDiscoverError] = useState('');
  const [showRestartModal, setShowRestartModal] = useState(false);
  const { busy: saving, run } = useSettingsSave(flash);
  // Must be a valid IPv4 dotted-quad (issue #153). Empty is allowed only so
  // the field can be cleared; Connect stays disabled until it's valid.
  const hostInvalid = host !== '' && !isValidIpv4Host(host);

  const handleConnect = async () => {
    // Defence-in-depth (issue #153): Connect is disabled on invalid input,
    // but a stale tab or programmatic caller must still be rejected.
    if (!isValidIpv4Host(host)) {
      flash('Inverter address must be a valid IPv4 address (e.g. 192.168.1.50)', false);
      return;
    }
    const savedHost = localStorage.getItem('saved_host');
    const hostChanged = savedHost && savedHost !== host;
    await run(async () => {
      await apiPost('/api/settings', { host, port, serial });
      localStorage.setItem('saved_host', host);
      onSerialSaved?.(serial);
      if (hostChanged) {
        setShowRestartModal(true);
        return;
      }
      return 'Settings saved — reconnecting…';
    }, 'Failed to save settings');
  };

  const handleDiscover = async () => {
    setDiscovering(true);
    setDiscoverError('');
    setDiscoverResults([]);
    try {
      const res = await apiGet<{ ok: boolean; subnets?: string[]; inverters: DiscoveredInverter[] }>('/api/discover');
      const results: DiscoveredInverter[] = (res.inverters || []).map((inv) => ({
        host: scannedHost(inv),
        port: inv.port,
        serial: inv.serial ?? null,
        generation: inv.generation ?? null,
      }));
      setDiscoverResults(results);
      if (results.length === 0) {
        const scanned = res.subnets?.length ? ` Scanned: ${res.subnets.join('.x, ')}.x` : '';
        setDiscoverError(`No inverters found on the network.${scanned}`);
      }
    } catch (error) {
      setDiscoverError(errorText(error, 'Discovery failed — is the backend running?'));
    }
    setDiscovering(false);
  };

  return (
    <SettingsSection id={id} title="Inverter Connection">
      {showRestartModal && (
        <RestartModal
          host={host}
          onClose={() => {
            setShowRestartModal(false);
            flash('Connection saved. Restart required.', true);
          }}
        />
      )}

      <div className="flex items-center gap-2 text-sm font-sans">
        <span
          className={`w-2.5 h-2.5 rounded-full ${
            connectionState === 'connected'
              ? 'bg-green-500'
              : connectionState === 'reconnecting'
                ? 'bg-yellow-500 animate-pulse'
                : 'bg-red-500'
          }`}
        />
        <span className="text-text-secondary capitalize">{connectionState}</span>
        {connectedHost && <span className="text-text-secondary">— {connectedHost}</span>}
      </div>

      <HostField
        label="Inverter Address"
        host={host}
        onHost={setHost}
        invalid={hostInvalid}
        placeholder="e.g. 192.168.1.50"
        port={developerMode ? port : undefined}
        onPort={setPort}
        portTitle="Inverter Modbus port"
      />

      <label className="flex flex-col gap-1">
        <span className="text-text-secondary text-xs font-sans">Serial Number <span className="opacity-50">(auto-detected)</span></span>
        <input
          type="text"
          value={serial}
          onChange={(e) => setSerial(e.target.value)}
          placeholder="Leave blank to auto-detect"
          className={TEXT_INPUT}
        />
      </label>

      <div className="flex gap-3 pt-1">
        <button
          onClick={() => void handleConnect()}
          disabled={saving || !host || hostInvalid}
          className="bg-accent text-on-accent font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 disabled:cursor-not-allowed transition-opacity"
        >
          {saving ? 'Saving…' : 'Connect'}
        </button>
        <button
          onClick={() => void handleDiscover()}
          disabled={discovering}
          className="bg-bg-elevated text-text-primary font-sans text-sm px-5 py-2 rounded-lg hover:bg-bg-base transition-colors disabled:opacity-40"
        >
          {discovering ? 'Scanning…' : 'Scan Network'}
        </button>
      </div>

      <DiscoveredList
        title="Discovered Inverters"
        error={discoverError}
        items={discoverResults.map((inv) => ({
          address: `${inv.host}:${inv.port}`,
          detail: `${inv.serial ?? 'Unknown Serial'}${inv.generation ? ` · ${inv.generation}` : ''}`,
          use: () => {
            setHost(inv.host);
            setPort(inv.port);
            if (inv.serial) setSerial(inv.serial);
          },
        }))}
      />

      <div className="flex items-center justify-between bg-bg-elevated rounded-xl px-4 py-3 border border-white/5">
        <div className="flex flex-col gap-0.5">
          <span className="text-text-primary text-sm font-sans font-medium">Enable Auto-Discovery</span>
          <span className="text-text-secondary text-xs font-sans">
            Scan the LAN for a new dongle IP after repeated connection failures. Disable if you have multiple inverters on the same network.
          </span>
        </div>
        <Toggle
          checked={!disableAutoDiscovery}
          ariaLabel="Enable Auto-Discovery"
          onChange={(v) => {
            const previous = disableAutoDiscovery;
            setDisableAutoDiscovery(!v);
            apiPost('/api/settings', { disable_auto_discovery: !v })
              .then(() => flash('Auto-Discovery setting saved', true))
              .catch((e) => {
                // Move back, so the switch never shows a setting that didn't save.
                setDisableAutoDiscovery(previous);
                flash(errorText(e, 'Failed to save'), false);
              });
          }}
        />
      </div>
    </SettingsSection>
  );
}
