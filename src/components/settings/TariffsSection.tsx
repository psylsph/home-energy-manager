import { useEffect, useState } from 'react';
import { apiGet, apiPost } from '../../lib/api';
import { positiveOrZero } from '../../lib/settingsForm';
import { defaultTariffConfig, flatTariffConfig, isTariffConfigValid } from '../../lib/tariff';
import {
  NO_APPLIED,
  isTariffPreview,
  lockedParts,
  partsReplaced,
  penceText,
  type AppliedParts,
  type PartName,
  type TariffPreview,
  type TariffPreviewResponse,
  type TariffSource,
} from '../../lib/tariffSource';
import type { PollSettings, TariffConfig } from '../../lib/types';
import HistoryRatesTools from './HistoryRatesTools';
import { SettingsGroup, SettingsSection } from './SettingsSection';
import TariffSlotEditor from './TariffSlotEditor';
import TariffSourceControl from './TariffSourceControl';
import { useSettingsSave, type Flash } from './useSettingsSave';

/** The saved tariff, falling back to a flat rate from the legacy single-rate field. */
function tariffFrom(cfg: TariffConfig | null | undefined, legacy: number | null | undefined, fallback: () => TariffConfig) {
  if (cfg) return cfg;
  if (legacy != null) return flatTariffConfig(legacy);
  return fallback();
}

const INPUT =
  'bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-accent outline-none transition-colors disabled:opacity-60';

/** Settings → Energy Tariffs: import/export time windows, the Standing Charge and gas rates. */
export default function TariffsSection({
  id,
  flash,
  initial,
}: {
  id?: string;
  flash: Flash;
  initial: Partial<PollSettings>;
}) {
  const octopusConfigured = initial.octopus_api_key_configured === true;
  const [importCfg, setImportCfg] = useState<TariffConfig>(() =>
    tariffFrom(initial.import_tariff_config, initial.import_tariff, defaultTariffConfig),
  );
  const [exportCfg, setExportCfg] = useState<TariffConfig>(() =>
    tariffFrom(initial.export_tariff_config, initial.export_tariff, () => flatTariffConfig(0.15)),
  );
  // Empty string = no Standing Charge (saved as 0).
  const [standingCharge, setStandingCharge] = useState(() =>
    penceText(initial.import_standing_charge_p_per_day),
  );
  const [gasRate, setGasRate] = useState(() => penceText(initial.gas_tariff_p_per_kwh));
  const [gasStanding, setGasStanding] = useState(() => penceText(initial.gas_standing_charge_p_per_day));
  // The gas rates the server holds, to tell whether the form changed them. Not
  // the `initial` prop, which is only what the page loaded with.
  const [savedGas, setSavedGas] = useState(() => ({
    rate: penceText(initial.gas_tariff_p_per_kwh),
    standing: penceText(initial.gas_standing_charge_p_per_day),
  }));

  // Following Octopus (issue #455).
  const [source, setSource] = useState<TariffSource>(initial.tariff_source === 'octopus' ? 'octopus' : 'manual');
  const [applied, setApplied] = useState<AppliedParts>(initial.octopus_tariff_applied ?? NO_APPLIED);
  const [preview, setPreview] = useState<TariffPreview | null>(null);
  const [switching, setSwitching] = useState(false);
  const [sourceError, setSourceError] = useState<string | null>(null);
  // Parts holding numbers the user typed, which switching to Octopus would replace.
  const [handEntered, setHandEntered] = useState<Record<PartName, boolean>>(() => ({
    import:
      initial.import_tariff_config != null || (initial.import_standing_charge_p_per_day ?? 0) > 0,
    export: initial.export_tariff_config != null,
    gas: (initial.gas_tariff_p_per_kwh ?? 0) > 0 || (initial.gas_standing_charge_p_per_day ?? 0) > 0,
  }));
  const typed = (part: PartName) => setHandEntered((h) => ({ ...h, [part]: true }));

  const { busy: saving, run } = useSettingsSave(flash);
  const valid = isTariffConfigValid(importCfg) && isTariffConfigValid(exportCfg);
  const locked = lockedParts(source, applied);
  // Octopus fills the gas unit rate only when the readings are in kWh, so with
  // anything else just the standing charge is locked.
  const gasRateFollowed = preview?.gas.ok
    ? preview.gas.unit_rate_used
    : initial.octopus_gas_enabled === true && initial.octopus_gas_unit === 'kwh';
  const gasRateLocked = locked.gas && gasRateFollowed;

  useEffect(() => {
    if (!octopusConfigured) return;
    let cancelled = false;
    void (async () => {
      try {
        const res = await apiGet<TariffPreviewResponse>('/api/octopus/tariff-preview');
        if (!cancelled && isTariffPreview(res.data)) setPreview(res.data);
      } catch {
        // The preview is optional; the editors work without it.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [octopusConfigured]);

  /**
   * Take the server's view of what follows Octopus: the source, which parts are
   * locked, and (for the parts it fills) the windows and rates it saved.
   */
  const adopt = (data: TariffPreview) => {
    setPreview(data);
    setSource(data.source);
    setApplied(data.applied);
    if (data.source !== 'octopus') return;
    if (data.applied.import && data.import.ok) {
      setImportCfg({ slots: data.import.slots });
      if (data.import.standing_charge_p_per_day != null) {
        setStandingCharge(penceText(data.import.standing_charge_p_per_day));
      }
    }
    if (data.applied.export && data.export.ok) setExportCfg({ slots: data.export.slots });
    if (data.applied.gas && data.gas.ok) {
      const filled: Partial<typeof savedGas> = {};
      if (data.gas.unit_rate_used) filled.rate = penceText(data.gas.unit_rate_p_per_kwh);
      if (data.gas.standing_charge_p_per_day != null) {
        filled.standing = penceText(data.gas.standing_charge_p_per_day);
      }
      if (filled.rate !== undefined) setGasRate(filled.rate);
      if (filled.standing !== undefined) setGasStanding(filled.standing);
      // What Octopus filled is now what the server holds.
      setSavedGas((saved) => ({ ...saved, ...filled }));
    }
  };

  const handleSwitch = async (next: TariffSource) => {
    setSwitching(true);
    setSourceError(null);
    try {
      const { data } = await apiPost<TariffPreviewResponse>('/api/octopus/tariff-source', { source: next });
      if (!isTariffPreview(data)) throw new Error('Unexpected reply from the server');
      adopt(data);
      if (data.source === 'octopus') {
        setHandEntered({ import: false, export: false, gas: false });
        flash('Energy Tariffs now follow Octopus', true);
      } else {
        flash('Energy Tariffs are manual again', true);
      }
    } catch (e) {
      setSourceError(e instanceof Error ? e.message : 'Could not change the tariff source');
    } finally {
      setSwitching(false);
    }
  };

  /**
   * After a refused save: a sync may have started (or stopped) following a
   * part since the page loaded, so re-read it. The editors then show the truth
   * that the error message describes.
   */
  const reread = async () => {
    if (!octopusConfigured) return;
    try {
      const res = await apiGet<TariffPreviewResponse>('/api/octopus/tariff-preview');
      if (isTariffPreview(res.data)) adopt(res.data);
    } catch {
      // Keep what is shown; the error toast already says what happened.
    }
  };

  const handleSave = async () => {
    // Defence-in-depth: Save is disabled while invalid, but a programmatic
    // call must still be rejected.
    if (!valid) {
      flash('Tariff configuration is invalid', false);
      return;
    }
    // Only the parts that don't follow Octopus: the server refuses the rest.
    const body: Record<string, unknown> = {};
    if (!locked.import) {
      body.import_tariff_config = importCfg;
      // Blank or junk means no Standing Charge rather than a stuck save (issue #131).
      body.import_standing_charge_p_per_day = positiveOrZero(standingCharge);
    }
    if (!locked.export) body.export_tariff_config = exportCfg;
    const gasChanged = gasRate !== savedGas.rate || gasStanding !== savedGas.standing;
    if (gasChanged) {
      if (!gasRateLocked) body.gas_tariff_p_per_kwh = positiveOrZero(gasRate);
      if (!locked.gas) body.gas_standing_charge_p_per_day = positiveOrZero(gasStanding);
    }
    if (Object.keys(body).length === 0) {
      flash('Every rate follows Octopus, so there is nothing to save', true);
      return;
    }
    // The server's validation message says why a save that looks valid
    // failed (e.g. a hand-edited settings file).
    await run(async () => {
      try {
        await apiPost('/api/settings', body);
      } catch (e) {
        await reread();
        throw e;
      }
      // Saved: the server now holds what the form shows.
      if ('gas_tariff_p_per_kwh' in body || 'gas_standing_charge_p_per_day' in body) {
        setSavedGas({ rate: gasRate, standing: gasStanding });
      }
      return 'Tariff rates saved';
    }, 'Failed to save tariffs');
  };

  return (
    <SettingsSection
      id={id}
      title="Energy Tariffs"
      description="Your tariff windows and rates, for the Planner and History costs."
      help="Configure your electricity tariff time windows and rates. Add multiple windows for time-of-use tariffs like Octopus Flux, Cosy, or Eco7. The Planner charges in the cheapest import window, and the History page uses the rates for costs. Each History row keeps the rates in force when it was recorded, so saving a change affects costs from now on and leaves earlier costs as they were."
    >
      {octopusConfigured && (
        <TariffSourceControl
          source={source}
          preview={preview}
          applied={applied}
          replaced={partsReplaced(preview, handEntered)}
          busy={switching}
          error={sourceError}
          onSwitch={handleSwitch}
        />
      )}

      <TariffSlotEditor
        label="Import"
        config={importCfg}
        readOnly={locked.import}
        onChange={(cfg) => {
          typed('import');
          setImportCfg(cfg);
        }}
      />
      <TariffSlotEditor
        label="Export"
        config={exportCfg}
        readOnly={locked.export}
        onChange={(cfg) => {
          typed('export');
          setExportCfg(cfg);
        }}
      />

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
            disabled={locked.import}
            onChange={(e) => {
              typed('import');
              setStandingCharge(e.target.value);
            }}
            aria-label="Import Standing Charge in Pence per Day"
            className={INPUT}
          />
          <span className="text-text-secondary text-xs font-sans">
            Daily fixed import cost added to every History cost total. UK Octopus Flux ≈ 54.86. Leave blank for no Standing Charge.
          </span>
        </label>
      </div>

      <SettingsGroup
        title="Gas"
        description="Your gas unit rate and standing charge, in pence."
        help="Octopus fills these in when your gas readings are in kWh. HEM doesn't use them for other costs yet; the Octopus page prices gas from the rates Octopus published."
      >
        <div className="flex flex-wrap gap-4">
          <label className="flex flex-col gap-1">
            <span className="text-text-primary text-sm font-sans">Gas Unit Rate (p/kWh)</span>
            <input
              type="number"
              step="0.001"
              min="0"
              inputMode="decimal"
              value={gasRate}
              placeholder="e.g. 6.1"
              disabled={gasRateLocked}
              onChange={(e) => {
                typed('gas');
                setGasRate(e.target.value);
              }}
              className={INPUT}
            />
          </label>
          <label className="flex flex-col gap-1">
            <span className="text-text-primary text-sm font-sans">Gas Standing Charge (p/day)</span>
            <input
              type="number"
              step="0.001"
              min="0"
              inputMode="decimal"
              value={gasStanding}
              placeholder="e.g. 29.4"
              disabled={locked.gas}
              onChange={(e) => {
                typed('gas');
                setGasStanding(e.target.value);
              }}
              className={INPUT}
            />
          </label>
        </div>
      </SettingsGroup>

      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <button
          onClick={() => void handleSave()}
          disabled={saving || !valid}
          title={valid ? undefined : 'Tariff configuration is invalid — see errors above.'}
          className="bg-accent text-on-accent font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 disabled:cursor-not-allowed transition-opacity"
        >
          {saving ? 'Saving…' : 'Save Tariffs'}
        </button>
        {/* Issue #455: rows keep the rates they were recorded with. */}
        <span className="text-text-secondary text-xs font-sans">
          Changes apply from now. Past costs keep the rates they were recorded with.
        </span>
      </div>

      <HistoryRatesTools octopusConfigured={octopusConfigured} />
    </SettingsSection>
  );
}
