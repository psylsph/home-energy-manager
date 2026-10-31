import { useState } from 'react';
import { apiGet, apiPost } from '../../lib/api';
import type { DiscoveredEvc } from '../../lib/types';
import { scannedHost } from '../../lib/settingsForm';
import { isValidIpv4Host } from '../../lib/validators';
import { useInverterStore } from '../../store/useInverterStore';
import { DiscoveredList, HostField } from './HostField';
import { SettingsSection } from './SettingsSection';
import { errorText, useSettingsSave, type Flash } from './useSettingsSave';

/** Settings → EV Charger: optional read-only monitoring over standard Modbus TCP. */
export default function EvChargerSection({
  id,
  flash,
  initialHost,
  initialPort,
}: {
  id?: string;
  flash: Flash;
  initialHost: string;
  initialPort: number;
}) {
  const developerMode = useInverterStore((s) => s.developerMode);
  const [evcHost, setEvcHost] = useState(initialHost);
  const [evcPort, setEvcPort] = useState(initialPort);
  const [discoverResults, setDiscoverResults] = useState<DiscoveredEvc[]>([]);
  const [discoverError, setDiscoverError] = useState('');
  const [discovering, setDiscovering] = useState(false);
  const { busy: saving, run } = useSettingsSave(flash);
  // Empty host = EVC disabled (intentional, see handleSave). Anything
  // non-empty must be a valid IPv4 dotted-quad (issue #138).
  const hostInvalid = evcHost !== '' && !isValidIpv4Host(evcHost);

  const handleSave = async () => {
    // Defence-in-depth (issue #138): the Save button is also disabled on
    // invalid input, but a stale tab or programmatic caller must still be
    // rejected.
    if (hostInvalid) {
      flash('EV Charger address must be a valid IPv4 address (e.g. 192.168.1.50)', false);
      return;
    }
    await run(async () => {
      await apiPost('/api/settings', { evc_host: evcHost, evc_port: evcPort });
      useInverterStore.getState().setEvcHost(evcHost);
      // Clear cached EVC state so the energy-flow node resets for the new
      // host rather than carrying over the previous host's power values
      // (issue #138). The next EVC poll frame repopulates it.
      useInverterStore.getState().resetEvc();
      return evcHost ? 'EV Charger settings saved' : 'EV Charger disabled';
    }, 'Failed to save EV charger settings');
  };

  const handleDiscover = async () => {
    setDiscovering(true);
    setDiscoverError('');
    setDiscoverResults([]);
    try {
      const res = await apiGet<{ ok: boolean; subnets?: string[]; chargers: DiscoveredEvc[] }>('/api/evc/discover');
      const results: DiscoveredEvc[] = (res.chargers || []).map((charger) => ({
        host: scannedHost(charger),
        port: charger.port,
        serial: charger.serial ?? null,
      }));
      setDiscoverResults(results);
      if (results.length === 0) {
        const scanned = res.subnets?.length ? ` Scanned: ${res.subnets.join('.x, ')}.x` : '';
        setDiscoverError(`No EV chargers found on the network.${scanned}`);
      }
    } catch (error) {
      setDiscoverError(errorText(error, 'EV charger discovery failed — is the backend running?'));
    }
    setDiscovering(false);
  };

  return (
    <SettingsSection
      id={id}
      title="EV Charger"
      description="Optional. Connect a GivEnergy EV Charger on your local network for read-only monitoring. Uses standard Modbus TCP (port 502), not the proprietary inverter protocol."
    >
      <HostField
        label="Charger Address"
        host={evcHost}
        onHost={setEvcHost}
        invalid={hostInvalid}
        placeholder="e.g. 192.168.1.50 (leave blank to disable)"
        port={developerMode ? evcPort : undefined}
        onPort={setEvcPort}
        portTitle="EV Charger Modbus port"
      />

      <div className="flex gap-3 pt-1">
        <button
          onClick={() => void handleSave()}
          disabled={saving || hostInvalid}
          className="bg-accent text-on-accent font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 transition-opacity disabled:opacity-40 disabled:cursor-not-allowed"
        >
          Save
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
        title="Discovered EV Chargers"
        error={discoverError}
        items={discoverResults.map((c) => ({
          address: `${c.host}:${c.port}`,
          detail: c.serial ?? 'Standard Modbus TCP device',
          use: () => {
            setEvcHost(c.host);
            setEvcPort(c.port);
          },
        }))}
      />
    </SettingsSection>
  );
}
