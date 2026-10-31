import { useState } from 'react';
import { apiPost } from '../../lib/api';
import { positiveOrZero } from '../../lib/settingsForm';
import { defaultTariffConfig, flatTariffConfig, isTariffConfigValid } from '../../lib/tariff';
import type { PollSettings, TariffConfig } from '../../lib/types';
import { SettingsSection } from './SettingsSection';
import TariffSlotEditor from './TariffSlotEditor';
import { useSettingsSave, type Flash } from './useSettingsSave';

/** The saved tariff, falling back to a flat rate from the legacy single-rate field. */
function tariffFrom(cfg: TariffConfig | null | undefined, legacy: number | null | undefined, fallback: () => TariffConfig) {
  if (cfg) return cfg;
  if (legacy != null) return flatTariffConfig(legacy);
  return fallback();
}

/** Settings → Energy Tariffs: import/export time windows and the Standing Charge. */
export default function TariffsSection({
  id,
  flash,
  initial,
}: {
  id?: string;
  flash: Flash;
  initial: Partial<PollSettings>;
}) {
  const [importCfg, setImportCfg] = useState<TariffConfig>(() =>
    tariffFrom(initial.import_tariff_config, initial.import_tariff, defaultTariffConfig),
  );
  const [exportCfg, setExportCfg] = useState<TariffConfig>(() =>
    tariffFrom(initial.export_tariff_config, initial.export_tariff, () => flatTariffConfig(0.15)),
  );
  // Empty string = no Standing Charge (saved as 0).
  const [standingCharge, setStandingCharge] = useState(() => {
    const v = initial.import_standing_charge_p_per_day;
    return v != null && v > 0 ? String(v) : '';
  });
  const { busy: saving, run } = useSettingsSave(flash);
  const valid = isTariffConfigValid(importCfg) && isTariffConfigValid(exportCfg);

  const handleSave = async () => {
    // Defence-in-depth: Save is disabled while invalid, but a programmatic
    // call must still be rejected.
    if (!valid) {
      flash('Tariff configuration is invalid', false);
      return;
    }
    // The server's validation message says why a save that looks valid
    // failed (e.g. a hand-edited settings file).
    await run(async () => {
      await apiPost('/api/settings', {
        import_tariff_config: importCfg,
        export_tariff_config: exportCfg,
        // Blank or junk means no Standing Charge rather than a stuck save (issue #131).
        import_standing_charge_p_per_day: positiveOrZero(standingCharge),
      });
      return 'Tariff rates saved';
    }, 'Failed to save tariffs');
  };

  return (
    <SettingsSection
      id={id}
      title="Energy Tariffs"
      description="Configure your electricity tariff time windows and rates. Add multiple windows for time-of-use tariffs like Octopus Flux, Cosy, or Eco7. Used for cost calculations on the History page."
    >
      <TariffSlotEditor label="Import" config={importCfg} onChange={setImportCfg} />
      <TariffSlotEditor label="Export" config={exportCfg} onChange={setExportCfg} />

      {/* Issue #131: UK-style tariffs charge a flat daily fee on top of the
          per-kWh rate; without it the History cost graph reads low. */}
      <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-2">
        <label className="flex flex-col gap-1 max-w-xs">
          <span className="text-text-primary text-sm font-sans font-medium">
            Standing Charge (p/day)
          </span>
          <input
            type="number"
            step="0.001"
            min="0"
            inputMode="decimal"
            value={standingCharge}
            placeholder="e.g. 54.86"
            onChange={(e) => setStandingCharge(e.target.value)}
            aria-label="Import Standing Charge in Pence Per Day"
            className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-accent outline-none transition-colors"
          />
          <span className="text-text-secondary text-xs font-sans">
            Daily fixed import cost added to every History cost total. UK Octopus Flux ≈ 54.86. Leave blank for no Standing Charge.
          </span>
        </label>
      </div>

      <button
        onClick={() => void handleSave()}
        disabled={saving || !valid}
        title={valid ? undefined : 'Tariff configuration is invalid — see errors above.'}
        className="bg-accent text-on-accent font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 disabled:cursor-not-allowed transition-opacity self-start"
      >
        {saving ? 'Saving…' : 'Save Tariffs'}
      </button>
    </SettingsSection>
  );
}
