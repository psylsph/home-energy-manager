import { useState } from 'react';
import { apiPost } from '../../lib/api';
import { meterAddressText, meterName } from '../../lib/meters';
import type { PollSettings } from '../../lib/types';
import { useInverterStore } from '../../store/useInverterStore';
import { SettingsSection } from './SettingsSection';
import { useSettingsSave, type Flash } from './useSettingsSave';

/** Same limit as the backend (`settings::METER_NAME_MAX_CHARS`). */
const MAX_NAME_CHARS = 32;

/** Names worth saving: trimmed, blanks dropped (blank means the address title). */
function cleanNames(fields: Record<string, string>): Record<string, string> {
  const names: Record<string, string> = {};
  for (const [address, name] of Object.entries(fields)) {
    const trimmed = name.trim();
    if (trimmed) names[address] = trimmed;
  }
  return names;
}

/**
 * Settings → Meters (issue #473): a name for each external CT clamp, shown on
 * the Meters page and in the meter pickers instead of `Meter 0x01`. Display
 * only — which meter is the grid CT or a solar array doesn't change. Lists the
 * meters on the bus now plus any that already have a name, so an offline
 * meter's name can still be changed or cleared. With none, it says so rather
 * than disappearing, so the Settings nav's Meters link always lands.
 */
export default function MetersSection({
  id,
  flash,
  initial,
}: {
  id?: string;
  flash: Flash;
  initial: Partial<PollSettings>;
}) {
  const [fields, setFields] = useState<Record<string, string>>(() => ({ ...(initial.meter_names ?? {}) }));
  const { busy: saving, run } = useSettingsSave(flash);
  const setMeterLabels = useInverterStore((s) => s.setMeterLabels);
  // The store, which App loads with the settings and Solar Arrays updates on
  // save, so a deleted array's name goes at once.
  const solarArrays = useInverterStore((s) => s.meterLabels.solarArrays);
  // Only the addresses, as a stable string, so a poll doesn't re-render this.
  const liveAddresses = useInverterStore((s) =>
    (s.snapshot?.meters ?? [])
      .map((m) => m.address)
      .filter((a) => a >= 1 && a <= 8)
      .join(','),
  );

  const addresses = [
    ...new Set([
      ...(liveAddresses ? liveAddresses.split(',').map(Number) : []),
      ...Object.keys(fields).map(Number),
    ]),
  ]
    .filter((a) => Number.isInteger(a) && a >= 1 && a <= 8)
    .sort((a, b) => a - b);

  const handleSave = () =>
    run(async () => {
      const names = cleanNames(fields);
      await apiPost('/api/settings', { meter_names: names });
      setMeterLabels({ names });
      return 'Meter names saved';
    }, 'Failed to save meter names');

  return (
    <SettingsSection
      id={id}
      title="Meters"
      description="Names for your CT meters, shown on the Meters page and in meter pickers."
      help="Each external CT clamp is titled by its Modbus address (Meter 0x01) unless you name it here, for example Mains In or Solar Main Roof. Names are display only: they don't change which meter HEM uses for the grid or a solar array. Leave a name blank to use the solar array's name, if the meter measures one, or the address."
    >
      {addresses.length === 0 ? (
        <p className="text-text-secondary/70 text-xs font-sans italic" data-testid="meter-names-empty">
          No external CT meters detected.
        </p>
      ) : (
        <>
          <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
            {addresses.map((address) => {
              const key = String(address);
              const inputId = `meter-name-${address}`;
              return (
                <div key={address} className="flex flex-col gap-1.5">
                  <label htmlFor={inputId} className="text-text-primary text-sm font-sans font-medium">
                    Meter {meterAddressText(address)}
                  </label>
                  <input
                    id={inputId}
                    type="text"
                    maxLength={MAX_NAME_CHARS}
                    value={fields[key] ?? ''}
                    onChange={(e) => {
                      const value = e.target.value;
                      setFields((f) => ({ ...f, [key]: value }));
                    }}
                    placeholder={meterName(address, {}, solarArrays) ?? 'e.g. Mains In'}
                    className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-sans border border-transparent focus:outline-none focus:border-accent"
                    data-testid="meter-name-input"
                  />
                </div>
              );
            })}
          </div>

          <button
            type="button"
            onClick={() => void handleSave()}
            className="bg-accent text-on-accent font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 transition-opacity disabled:opacity-30 self-start"
            disabled={saving}
            data-testid="meter-names-save"
          >
            {saving ? 'Saving…' : 'Save Meter Names'}
          </button>
        </>
      )}
    </SettingsSection>
  );
}
