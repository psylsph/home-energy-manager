import { useState, useEffect, useCallback } from 'react';
import { apiGet, apiPost, getApiBase, getServerPort } from '../lib/api';
import { openExternal } from '../lib/openExternal';
import type { PollSettings, DiscoveredInverter, DiscoveredEvc, TariffConfig } from '../lib/types';
import {
  defaultTariffConfig,
  flatTariffConfig,
  addTariffSlot,
  removeTariffSlot,
  updateTariffSlot,
  halfHourOptions,
  MAX_TARIFF_SLOTS,
  validateTariffConfig,
  isTariffConfigValid,
  FINAL_SLOT_END_MINUTES,
  parseHHMM,
} from '../lib/tariff';
import { useInverterStore } from '../store/useInverterStore';
import { isValidIpv4Host } from '../lib/validators';

function Toggle({ checked, onChange }: { checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <div className="relative cursor-pointer" onClick={() => onChange(!checked)}>
      <div className={`w-10 h-5 rounded-full transition-colors ${checked ? 'bg-flow-active/40' : 'bg-bg-elevated'}`} />
      <div className={`absolute left-0.5 top-0.5 w-4 h-4 rounded-full transition-all ${checked ? 'translate-x-5 bg-flow-active' : 'bg-text-secondary'}`} />
    </div>
  );
}

const TIME_OPTIONS = halfHourOptions();

/** Reusable tariff time-window editor for import/export tariffs. */
function TariffSlotEditor({
  label,
  config,
  onChange,
}: {
  label: string;
  config: TariffConfig;
  onChange: (cfg: TariffConfig) => void;
}) {
  const errors = validateTariffConfig(config);
  // Index validation errors by slot for inline display.
  const errorsBySlot = new Map<number, string[]>();
  for (const err of errors) {
    if (err.slotIndex < 0) continue;
    const list = errorsBySlot.get(err.slotIndex) ?? [];
    list.push(err.message);
    errorsBySlot.set(err.slotIndex, list);
  }

  // Allowed options for a slot's end select: must be ≥ slot start.
  // The end is freely selectable up to 23:59 — changing it cascades
  // forward to update the next slot's start, keeping the day tiled.
  const optionsForEnd = (slot: { start: string; end: string }) => {
    const startMin = parseHHMM(slot.start) ?? 0;
    return TIME_OPTIONS.filter((t) => {
      const m = parseHHMM(t);
      if (m === null) return false;
      return m >= startMin;
    });
  };

  // Allowed options for a slot's start select: must be ≥ previous slot's
  // end (or `00:00` if first) AND ≤ this slot's end. Prevents the user
  // from creating gaps or overlaps when editing start directly.
  const optionsForStart = (i: number, slot: { start: string; end: string }) => {
    const prevEndMin =
      i > 0
        ? parseHHMM(config.slots[i - 1]!.end) ?? 0
        : 0;
    const endMin = parseHHMM(slot.end) ?? FINAL_SLOT_END_MINUTES;
    return TIME_OPTIONS.filter((t) => {
      const m = parseHHMM(t);
      if (m === null) return false;
      return m >= prevEndMin && m <= endMin;
    });
  };

  return (
    <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <h3 className="text-text-primary text-sm font-sans font-medium">{label}</h3>
        {config.slots.length < MAX_TARIFF_SLOTS && (
          <button
            onClick={() => onChange(addTariffSlot(config, 0.15))}
            className="text-flow-active text-xs font-sans hover:opacity-80 transition-opacity"
          >
            + Add window
          </button>
        )}
      </div>

      {config.slots.map((slot, i) => {
        const isFirst = i === 0;
        const isLast = i === config.slots.length - 1;
        const slotErrors = errorsBySlot.get(i) ?? [];
        // The end <select> is locked to `23:59` for the last slot so the day
        // is always tiled through to the end. Intermediate slots allow
        // choosing any end from the slot's start onward — changing it
        // cascades forward to update the next slot's start automatically.
        const endOptions = optionsForEnd(slot);
        return (
          <div key={i} className="flex flex-col gap-1">
            <div className="grid grid-cols-2 sm:grid-cols-[1fr_1fr_1fr_auto] gap-2 items-end">
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">Start</span>
                <select
                  value={slot.start}
                  // First slot is locked to 00:00 (the day must start at
                  // midnight). All other slots can have their start edited
                  // directly — changing it cascades backward to update the
                  // previous slot's end, keeping the day tiled.
                  onChange={(e) => onChange(updateTariffSlot(config, i, 'start', e.target.value))}
                  disabled={isFirst}
                  className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors disabled:opacity-40"
                >
                  {isFirst
                    ? <option value={slot.start}>{slot.start}</option>
                    : optionsForStart(i, slot).map((t) => (
                        <option key={t} value={t}>{t}</option>
                      ))
                  }
                </select>
              </label>
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">End</span>
                <select
                  value={slot.end}
                  onChange={(e) => onChange(updateTariffSlot(config, i, 'end', e.target.value))}
                  disabled={isLast}
                  className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors disabled:opacity-40"
                >
                  {endOptions.map((t) => (
                    <option key={t} value={t}>{t}</option>
                  ))}
                </select>
              </label>
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">Rate (p/kWh)</span>
                <input
                  type="number" step="0.01" min="0"
                  value={Math.round(slot.rate * 100000) / 1000}
                  onChange={(e) => onChange(updateTariffSlot(config, i, 'rate', Number(e.target.value) / 100))}
                  className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                />
              </label>
              {config.slots.length > 1 && (
                <button
                  onClick={() => onChange(removeTariffSlot(config, i))}
                  className="text-text-secondary hover:text-red-400 text-sm font-sans pb-2 transition-colors"
                  title="Remove window"
                >
                  ✕
                </button>
              )}
            </div>
            {slotErrors.length > 0 && (
              <ul className="text-red-400 text-xs font-sans pl-1">
                {slotErrors.map((msg, j) => (
                  <li key={j}>{msg}</li>
                ))}
              </ul>
            )}
          </div>
        );
      })}

      {config.slots.length === 1 && (
        <p className="text-text-secondary/60 text-xs font-sans">
          Flat rate for the whole day. Add windows for time-of-use tariffs (e.g. Octopus Flux, Cosy, Eco7).
        </p>
      )}
      {!isTariffConfigValid(config) && (
        <div className="flex items-center justify-between gap-3">
          <p className="text-red-400 text-xs font-sans">
            Tariff configuration is invalid — windows must cover the full 24 hours contiguously with no overlaps. Saving is disabled.
          </p>
          <button
            // Recovery affordance: if the user lands in an unsaveable state
            // (e.g. after editing settings.json by hand), this resets to a
            // valid flat-rate config using the first slot's rate as a
            // reasonable default.
            onClick={() => onChange(flatTariffConfig(config.slots[0]?.rate ?? 0.15))}
            className="text-flow-active text-xs font-sans hover:opacity-80 transition-opacity whitespace-nowrap"
          >
            Reset to flat rate
          </button>
        </div>
      )}
    </div>
  );
}

function StoreQr({ url, alt }: { url: string; alt: string }) {
  const [dataUrl, setDataUrl] = useState('');
  useEffect(() => {
    let cancelled = false;
    import('qrcode').then((qr) => {
      qr.toString(url, { type: 'svg', width: 120, margin: 1, color: { dark: '#4ade80', light: '#1a1a2e' } })
        .then((svg) => { if (!cancelled) setDataUrl('data:image/svg+xml;utf8,' + encodeURIComponent(svg)); });
    }).catch(() => {});
    return () => { cancelled = true; };
  }, [url]);
  if (!dataUrl) return <div className="w-[120px] h-[120px] bg-bg-elevated rounded-lg animate-pulse" />;
  return <img src={dataUrl} alt={alt} className="w-[120px] h-[120px] rounded-lg" />;
}

/// Snap a poll interval to the nearest valid value (5, 10, 15, 20, 30, 45, or 60).
const VALID_INTERVALS = [5, 10, 15, 20, 30, 45, 60];

const clampInterval = (v: number) => VALID_INTERVALS.reduce((a, b) =>
  Math.abs(b - v) < Math.abs(a - v) ? b : a
);

export default function SettingsPage() {
  const {
    connectionState,
    connectedHost,
    developerMode,
    setDeveloperMode,
    panelGraphsEnabled,
    setPanelGraphsEnabled,
    panelGraphsScale,
    setPanelGraphsScale,
    panelGraphsYLock,
    setPanelGraphsYLock,
    visualNoiseThreshold,
    setVisualNoiseThreshold,
    snapshot,
  } = useInverterStore();

  // Connection fields
  const [host, setHost] = useState('');
  const [port, setPort] = useState(8899);
  const [serial, setSerial] = useState('');

  // Discover
  const [discovering, setDiscovering] = useState(false);
  const [discoverResults, setDiscoverResults] = useState<DiscoveredInverter[]>([]);
  const [discoverError, setDiscoverError] = useState('');

  // Refresh interval
  const [intervalSecs, setIntervalSecs] = useState(20);

  // HTTP server port
  const [httpPort, setHttpPort] = useState(7337);

  // EV Charger
  const [evcHost, setEvcHost] = useState('');
  const [evcPort, setEvcPort] = useState(502);
  const [evcDiscovering, setEvcDiscovering] = useState(false);
  const [evcDiscoverResults, setEvcDiscoverResults] = useState<DiscoveredEvc[]>([]);
  const [evcDiscoverError, setEvcDiscoverError] = useState('');
  // Empty host = EVC disabled (intentional, see handleEvcSave). Anything
  // non-empty must be a valid IPv4 dotted-quad (issue #138).
  const evcHostInvalid = evcHost !== '' && !isValidIpv4Host(evcHost);
  const [disableAutoDiscovery, setDisableAutoDiscovery] = useState(false);

  // Start on login (issue #117). The actual platform autostart entry is
  // managed by tauri-plugin-autostart; the persisted preference is the
  // source of truth and the Rust startup self-heal re-applies it.
  const [autostartEnabled, setAutostartEnabled] = useState(false);
  // Read-only API key and port (developer mode, external access).
  const [apiKey, setApiKey] = useState('');
  const [apiPort, setApiPort] = useState(7338);
  // `null` while we haven't asked the OS yet — we only show the toggle's
  // actual state if the plugin was reachable. The toggle is hidden
  // entirely in headless mode (no Tauri shell to register).
  const autostartSupported =
    typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

  // Tariffs
  const [importTariffCfg, setImportTariffCfg] = useState<TariffConfig>(() => defaultTariffConfig());
  const [exportTariffCfg, setExportTariffCfg] = useState<TariffConfig>(() =>
    flatTariffConfig(0.15),
  );

  // General
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<{ text: string; ok: boolean } | null>(null);
  const [showRestartModal, setShowRestartModal] = useState(false);
  const [pendingConnect, setPendingConnect] = useState(false);
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  const [lanIp, setLanIp] = useState<string | null>(null);
  const [clients, setClients] = useState<string[]>([]);
  const [hiddenPanels, setHiddenPanels] = useState<string[]>([]);
  const [panelSaving, setPanelSaving] = useState(false);

  // Email alerts
  const [alertsConfig, setAlertsConfig] = useState({
    enabled: false, telegram_bot_token: '', telegram_chat_id: '',
    cooldown_minutes: 30,
    batt_temp_min: 0, batt_temp_max: 0,
    soc_min: 4, soc_max: 100,
    grid_offline_enabled: false, battery_over_temp_enabled: false,
    connection_lost_enabled: false,
    solar_clipping_enabled: false, solar_clipping_ceiling_w: 0,
    ntfy_topic: '',
    ntfy_server: 'https://ntfy.sh',
    pushover_app_token: '',
    pushover_user_key: '',
  });
  const [alertsSaving, setAlertsSaving] = useState(false);
  const [alertsTesting, setAlertsTesting] = useState(false);

  // Support bundle submission (issue #125). The category list mirrors the
  // backend's `valid_categories()` exactly; keeping them in sync by hand is
  // the price of not coupling the frontend to the Rust enum.
  const [supportCategory, setSupportCategory] = useState('connection');
  const [supportDescription, setSupportDescription] = useState('');
  const [supportIssueNumber, setSupportIssueNumber] = useState('');
  const [supportIncludeHistory, setSupportIncludeHistory] = useState(false);
  const [supportSubmitting, setSupportSubmitting] = useState(false);

  // Weather (Open-Meteo) — local ambient temperature for the History charts.
  // `weatherState` is the full GET /api/weather payload (config + last fetch
  // result + backfill progress). Individual form fields below are local
  // state so the user can type before hitting Save; they're synced from
  // `weatherState` on load and on save.
  const [weatherState, setWeatherState] = useState<{
    config: {
      enabled: boolean;
      postcode: string;
      latitude: number | null;
      longitude: number | null;
      last_backfill_completed: string | null;
      open_meteo_base_url: string;
    };
    last_fetch_at: string | null;
    last_fetched_temperature_c: number | null;
    grid_cell_latitude: number | null;
    grid_cell_longitude: number | null;
    backfill_in_progress: boolean;
    last_error: string | null;
  } | null>(null);
  const [weatherPostcode, setWeatherPostcode] = useState('');
  const [weatherLat, setWeatherLat] = useState('');
  const [weatherLon, setWeatherLon] = useState('');
  const [weatherSaving, setWeatherSaving] = useState(false);
  const [weatherBackfilling, setWeatherBackfilling] = useState(false);

  const flash = useCallback((text: string, ok: boolean) => {
    setMessage({ text, ok });
    setTimeout(() => setMessage(null), 4000);
  }, []);

  // Load settings on mount
  useEffect(() => {
    (async () => {
      try {
        const res = await apiGet<{ok: boolean, data: PollSettings}>('/api/settings');
        const s = res.data;
        setHost(s.host ?? '');
        setPort(s.port ?? 8899);
        setSerial(s.serial ?? '');
        setIntervalSecs(clampInterval( s.interval_secs ?? 20));
        setHttpPort(s.http_port ?? 7337);
        if (s.import_tariff_config) {
          setImportTariffCfg(s.import_tariff_config);
        } else if (s.import_tariff != null) {
          setImportTariffCfg(flatTariffConfig(s.import_tariff));
        }
        if (s.export_tariff_config) {
          setExportTariffCfg(s.export_tariff_config);
        } else if (s.export_tariff != null) {
          setExportTariffCfg(flatTariffConfig(s.export_tariff));
        }
        if (s.hidden_panels) {
          setHiddenPanels(s.hidden_panels);
        }
        setEvcHost(s.evc_host ?? '');
        setEvcPort(s.evc_port ?? 502);
        setDisableAutoDiscovery(s.disable_auto_discovery ?? false);
        setAutostartEnabled(s.autostart_enabled ?? false);
        setApiKey(s.api_key ?? '');
        setApiPort(s.api_port ?? 7338);
        setSettingsLoaded(true);
      } catch (e: unknown) {
        console.warn('Failed to load settings:', e);
        setSettingsLoaded(true);
      }
    })();

    // Load alert config
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: { config: typeof alertsConfig } }>('/api/alerts');
        if (res.ok && res.data?.config) {
          setAlertsConfig(res.data.config);
        }
      } catch (e: unknown) {
        console.warn('Failed to load alerts config:', e);
      }
    })();

    // Load weather config + state. Form fields are seeded from the config
    // so the user sees what's currently persisted.
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: typeof weatherState }>('/api/weather');
        if (res.ok && res.data) {
          setWeatherState(res.data);
          setWeatherPostcode(res.data.config.postcode ?? '');
          setWeatherLat(res.data.config.latitude?.toString() ?? '');
          setWeatherLon(res.data.config.longitude?.toString() ?? '');
        }
      } catch (e: unknown) {
        console.warn('Failed to load weather config:', e);
      }
    })();

    // Fetch LAN IP and connected clients for network access display
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; lan_ip: string | null; clients: string[]; client_count: number }>('/api/status');
        if (res.lan_ip) setLanIp(res.lan_ip);
        if (res.clients) setClients(res.clients);
      } catch (e: unknown) { console.warn('Failed to fetch status:', e); }
    })();
  }, []);

  // Derived ntfy topic: fall back to a serial-based default when the user
  // hasn't entered a custom topic. Kept as a derived value (not an effect) so
  // manual edits are never overwritten and there's no setState-in-effect.
  const generatedNtfyTopic = (serial || snapshot?.inverter_serial) ? `hem-${serial || snapshot?.inverter_serial}` : '';
  const effectiveNtfyTopic = alertsConfig.ntfy_topic || generatedNtfyTopic;

  // Network URL — use LAN IP if available, otherwise fall back to getApiBase()
  const lanUrl = lanIp ? `http://${lanIp}:${getServerPort()}` : getApiBase();
  // Read-only URL — same as lanUrl with the `?RO` flag appended, which
  // hides the Control and Settings nav icons in the visitor's browser
  // (issue #114). Sticky via localStorage, so the recipient only needs
  // to visit it once.
  const lanReadOnlyUrl = `${lanUrl}?RO`;

  // Save connection
  const handleConnect = async () => {
    const savedHost = localStorage.getItem('saved_host');
    const hostChanged = savedHost && savedHost !== host;
    setSaving(true);
    try {
      await apiPost('/api/settings', { host, port, serial });
      localStorage.setItem('saved_host', host);
      if (hostChanged) {
        setPendingConnect(true);
        setShowRestartModal(true);
      } else {
        flash('Settings saved — reconnecting…', true);
      }
    } catch (error) {
      flash(error instanceof Error ? error.message : 'Failed to save settings', false);
    }
    setSaving(false);
  };

  // Save interval
  const handleIntervalChange = async (val: number) => {
    setIntervalSecs(val);
    try {
      await apiPost('/api/settings', { interval_secs: val });
      flash(`Refresh interval set to ${val}s`, true);
    } catch (error) {
      flash(error instanceof Error ? error.message : 'Failed to update interval', false);
    }
  };

  // Save HTTP port
  const handleHttpPortSave = async () => {
    try {
      await apiPost('/api/settings', { http_port: httpPort });
      flash(`HTTP port set to ${httpPort}. Restart required to take effect.`, true);
    } catch (error) {
      flash(error instanceof Error ? error.message : 'Failed to update HTTP port', false);
    }
  };

  // Save EV Charger settings
  const handleEvcSave = async () => {
    // Re-check client-side as defence-in-depth (issue #138): the Save
    // button is also disabled on invalid input, but programmatic callers
    // (or an old tab racing a stale form state) must still be rejected.
    if (evcHostInvalid) {
      flash('EV Charger address must be a valid IPv4 address (e.g. 192.168.1.50)', false);
      return;
    }
    try {
      await apiPost('/api/settings', { evc_host: evcHost, evc_port: evcPort });
      useInverterStore.getState().setEvcHost(evcHost);
      // Clear cached EVC state so the energy-flow node resets to its
      // "Not Found" / "Charging" / "Connected" state for the new host,
      // rather than carrying over power values from the previous host
      // (issue #138). The next EVC poll frame (or EvcDisconnected if the
      // new host is bad) will repopulate it.
      useInverterStore.getState().resetEvc();
      flash(evcHost ? 'EV Charger settings saved' : 'EV Charger disabled', true);
    } catch (error) {
      flash(error instanceof Error ? error.message : 'Failed to save EV charger settings', false);
    }
  };

  // Discover EV Charger
  const handleEvcDiscover = async () => {
    setEvcDiscovering(true);
    setEvcDiscoverError('');
    setEvcDiscoverResults([]);
    try {
      const res = await apiGet<{ok: boolean, subnets?: string[], chargers: DiscoveredEvc[]}>('/api/evc/discover');
      const results: DiscoveredEvc[] = (res.chargers || []).map((charger) => ({
        host: 'ip' in charger ? (charger as Record<string, unknown>).ip as string : charger.host,
        port: charger.port,
        serial: charger.serial ?? null,
      }));
      setEvcDiscoverResults(results);
      if (results.length === 0) {
        const scanned = res.subnets?.length ? ` Scanned: ${res.subnets.join('.x, ')}.x` : '';
        setEvcDiscoverError(`No EV chargers found on the network.${scanned}`);
      }
    } catch (error) {
      setEvcDiscoverError(error instanceof Error ? error.message : 'EV charger discovery failed — is the backend running?');
    }
    setEvcDiscovering(false);
  };

  const applyDiscoveredEvc = (charger: DiscoveredEvc) => {
    setEvcHost(charger.host);
    setEvcPort(charger.port);
  };

  // Save tariffs
  const handleTariffSave = async () => {
    // Re-check validity client-side as a defence-in-depth before posting —
    // the Save button is also disabled when invalid, but a programmatic
    // invocation (e.g. tests) should still be rejected.
    if (!isTariffConfigValid(importTariffCfg) || !isTariffConfigValid(exportTariffCfg)) {
      flash('Tariff configuration is invalid', false);
      return;
    }
    setSaving(true);
    try {
      await apiPost('/api/settings', {
        import_tariff_config: importTariffCfg,
        export_tariff_config: exportTariffCfg,
      });
      flash('Tariff rates saved', true);
    } catch (err) {
      // Surface the server's validation error (e.g. 400 with a message)
      // so the user sees *why* the save failed even though the UI looks
      // valid (e.g. a JSON file edited by hand).
      const msg = err instanceof Error ? err.message : String(err);
      flash(msg || 'Failed to save tariffs', false);
    }
    setSaving(false);
  };

  // Save panel visibility
  const handlePanelSave = async () => {
    setPanelSaving(true);
    try {
      await apiPost('/api/settings', { hidden_panels: hiddenPanels });
      // Push to store immediately so App.tsx picks it up without restart
      useInverterStore.getState().setHiddenPanels(hiddenPanels);
      flash('Panel visibility saved', true);
    } catch {
      flash('Failed to save panel visibility', false);
    }
    setPanelSaving(false);
  };

  // Save email alerts
  const handleAlertsSave = async () => {
    setAlertsSaving(true);
    // Persist the effective topic (user-entered, or the serial-based default
    // when left blank) so ntfy works without forcing the user to type one.
    const saveConfig = { ...alertsConfig, ntfy_topic: effectiveNtfyTopic };
    try {
      const res = await apiPost('/api/alerts', saveConfig) as { message: string; ok: boolean };
      flash(res.message, res.ok);
    } catch {
      flash('Failed to save alert settings', false);
    }
    setAlertsSaving(false);
  };

  // Send a test email
  const handleAlertsTest = async () => {
    setAlertsTesting(true);
    try {
      const res = await apiPost('/api/alerts/test', {}) as { ok: boolean; message: string };
      flash(res.message, res.ok);
    } catch {
      flash('Failed to Send Message — check API key and settings', false);
    }
    setAlertsTesting(false);
  };

  // Submit a diagnostic support bundle (logs + snapshot + settings) to the
  // maintainer via ntfy. The backend assembles everything and posts to the
  // shared `home-energy-manager-support` topic; we just forward the user's
  // description, the optional GitHub issue number, and privacy toggles.
  // See issue #125.
  const handleSubmitSupport = async () => {
    if (!supportDescription.trim()) {
      flash('Please describe the issue first.', false);
      return;
    }
    setSupportSubmitting(true);
    try {
      const res = await apiPost<{
        ok: boolean;
        bundle_id?: string;
        message: string;
      }>('/api/support/submit', {
        description: supportDescription,
        category: supportCategory,
        issue_number: supportIssueNumber,
        include_history: supportIncludeHistory,
      });
      flash(res.message, res.ok);
      if (res.ok) setSupportDescription('');
    } catch (e) {
      flash(e instanceof Error ? e.message : 'Failed to submit support bundle', false);
    }
    setSupportSubmitting(false);
  };

  // Save weather config. Sends postcode + manual coords (if any); the
  // backend resolves the postcode to lat/lon via api.postcodes.io when
  // coords aren't supplied. On success we re-fetch the full state so the
  // form reflects the resolved coordinates and canonicalised postcode.
  const handleWeatherSave = async () => {
    setWeatherSaving(true);
    try {
      const payload: Record<string, unknown> = {
        enabled: weatherState?.config.enabled ?? false,
        postcode: weatherPostcode.trim(),
      };
      const latNum = Number(weatherLat);
      const lonNum = Number(weatherLon);
      if (weatherLat.trim() !== '' && Number.isFinite(latNum)) {
        payload.latitude = latNum;
      }
      if (weatherLon.trim() !== '' && Number.isFinite(lonNum)) {
        payload.longitude = lonNum;
      }
      const res = await apiPost('/api/weather', payload) as { ok: boolean; message: string };
      flash(res.message, res.ok);
      // Re-fetch to pick up the resolved coords / canonicalised postcode.
      const refreshed = await apiGet<{ ok: boolean; data: typeof weatherState }>('/api/weather');
      if (refreshed.ok && refreshed.data) {
        setWeatherState(refreshed.data);
        setWeatherPostcode(refreshed.data.config.postcode ?? '');
        setWeatherLat(refreshed.data.config.latitude?.toString() ?? '');
        setWeatherLon(refreshed.data.config.longitude?.toString() ?? '');
      }
    } catch (e: unknown) {
      flash(e instanceof Error ? e.message : 'Failed to save weather config', false);
    }
    setWeatherSaving(false);
  };

  // Kick off a one-shot backfill. The backend runs it asynchronously;
  // we poll GET /api/weather every few seconds for progress until
  // `backfill_in_progress` clears.
  const handleWeatherBackfill = async () => {
    setWeatherBackfilling(true);
    try {
      await apiPost('/api/weather/backfill', {});
      flash('Backfill started — fetching historical weather in the background', true);
    } catch (e: unknown) {
      flash(e instanceof Error ? e.message : 'Failed to start backfill', false);
      setWeatherBackfilling(false);
    }
  };

  // Poll weather state while a backfill is running so the UI shows
  // live progress. Stops polling once `backfill_in_progress` clears.
  useEffect(() => {
    if (!weatherBackfilling) return;
    let cancelled = false;
    const poll = async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: typeof weatherState }>('/api/weather');
        if (cancelled) return;
        if (res.ok && res.data) {
          setWeatherState(res.data);
          if (!res.data.backfill_in_progress) {
            setWeatherBackfilling(false);
          }
        }
      } catch {
        // Swallow — we'll retry on the next interval.
      }
    };
    poll();
    const id = setInterval(poll, 3000);
    return () => { cancelled = true; clearInterval(id); };
  }, [weatherBackfilling]);

  // Flip the platform autostart entry and persist the new preference.
  // We optimistically apply the new value locally so the toggle reflects
  // the user's click immediately, then save to disk; if the platform
  // call fails we try the Windows Startup folder fallback (registry ACL
  // workaround — issue #117). If that also fails, we revert the toggle
  // and show the error; the Rust self-heal in lib.rs will retry on the
  // next launch.
  const handleAutostartToggle = async (next: boolean) => {
    if (!autostartSupported) return;
    const previous = autostartEnabled;
    setAutostartEnabled(next);
    try {
      // Apply the OS-level entry FIRST so the disk persistence matches
      // the actual platform state. If the user later rolls back the
      // toggle we still want both sides to be in sync.
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
      // Primary plugin call failed (e.g. registry ACL error on Windows).
      // Try the fallback: create/remove a shortcut in the Startup folder.
      try {
        const { invoke } = await import('@tauri-apps/api/core');
        await invoke('autostart_fallback', { enable: next });
        // Fallback succeeded — keep the optimistic update and persist.
        await apiPost('/api/settings', { autostart_enabled: next });
        flash(
          next
            ? 'Will start automatically when you log in (Startup folder)'
            : 'Will no longer start automatically when you log in',
          true,
        );
      } catch (e2) {
        // Both primary and fallback failed — revert the toggle.
        setAutostartEnabled(previous);
        const msg = e instanceof Error ? e.message : String(e);
        const msg2 = e2 instanceof Error ? e2.message : String(e2);
        flash(`Failed to ${next ? 'enable' : 'disable'} auto-start: ${msg} / ${msg2}`, false);
      }
    }
  };

  // Discover
  const handleDiscover = async () => {
    setDiscovering(true);
    setDiscoverError('');
    setDiscoverResults([]);
    try {
      const res = await apiGet<{ok: boolean, subnets?: string[], inverters: DiscoveredInverter[]}>('/api/discover');
      const results: DiscoveredInverter[] = (res.inverters || []).map((inv) => ({
        host: 'ip' in inv ? (inv as Record<string, unknown>).ip as string : (inv as DiscoveredInverter).host,
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
      setDiscoverError(error instanceof Error ? error.message : 'Discovery failed — is the backend running?');
    }
    setDiscovering(false);
  };

  const applyDiscovered = (inv: DiscoveredInverter) => {
    setHost(inv.host);
    setPort(inv.port);
    if (inv.serial) setSerial(inv.serial);
  };

  const copyUrl = (text: string) => {
    if (navigator.clipboard && window.isSecureContext) {
      navigator.clipboard.writeText(text).then(() => flash('URL copied!', true));
    } else {
      // Fallback for non-secure contexts (LAN HTTP)
      const ta = document.createElement('textarea');
      ta.value = text;
      ta.style.position = 'fixed';
      ta.style.opacity = '0';
      document.body.appendChild(ta);
      ta.select();
      try {
        document.execCommand('copy');
        flash('URL copied!', true);
      } catch {
        flash('Copy failed — please select and copy manually', false);
      }
      document.body.removeChild(ta);
    }
  };

  if (!settingsLoaded) {
    return (
      <div className="flex flex-col items-center justify-center min-h-[60vh] gap-4">
        <div className="w-10 h-10 border-4 border-flow-active border-t-transparent rounded-full animate-spin" />
        <p className="text-text-secondary text-sm font-sans">Loading settings…</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-8 max-w-2xl mx-auto px-4 py-6">
      {/* Restart modal */}
      {showRestartModal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
          <div className="bg-bg-surface rounded-2xl p-6 max-w-sm mx-4 shadow-2xl">
            <h3 className="text-text-primary text-lg font-semibold mb-2">Restart Required</h3>
            <p className="text-text-secondary text-sm mb-6">
              The connection to <strong className="text-text-primary">{host}</strong> has been saved.
              Please restart the app for the changes to take full effect.
            </p>
            <button
              onClick={() => {
                setShowRestartModal(false);
                if (pendingConnect) {
                  flash('Connection saved. Restart required.', true);
                  setPendingConnect(false);
                }
              }}
              className="w-full py-2.5 bg-flow-active/20 text-flow-active rounded-lg text-sm font-medium hover:bg-flow-active/30 transition"
            >
              Got it
            </button>
          </div>
        </div>
      )}

      {/* Toast */}
      {message && (
        <div
          className={`fixed top-4 right-4 z-50 px-4 py-2 rounded-lg text-sm font-sans shadow-lg ${
            message.ok ? 'bg-green-800/80 text-green-200' : 'bg-red-800/80 text-red-200'
          }`}
        >
          {message.text}
        </div>
      )}

      {/* ─── Section 1: Inverter Connection ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-4">
        <h2 className="text-text-primary text-lg font-semibold font-sans">Inverter Connection</h2>

        {/* Connection state badge */}
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

        <label className="flex flex-col gap-1">
          <span className="text-text-secondary text-xs font-sans">Inverter Address</span>
          <div className="flex gap-2">
            <input
              type="text"
              value={host}
              onChange={(e) => setHost(e.target.value)}
              placeholder="192.168.x.x"
              className="min-w-0 flex-1 bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
            />
            {developerMode && (
              <input
                type="number"
                value={port}
                onChange={(e) => setPort(Number(e.target.value))}
                title="Inverter Modbus port"
                className="w-[5.5em] shrink-0 bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
              />
            )}
          </div>
        </label>

        <label className="flex flex-col gap-1">
          <span className="text-text-secondary text-xs font-sans">Serial Number <span className="opacity-50">(auto-detected)</span></span>
          <input
            type="text"
            value={serial}
            onChange={(e) => setSerial(e.target.value)}
            placeholder="Leave blank to auto-detect"
            className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
          />
        </label>

        <div className="flex gap-3 pt-1">
          <button
            onClick={handleConnect}
            disabled={saving || !host}
            className="bg-flow-active text-bg-base font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 transition-opacity"
          >
            {saving ? 'Saving…' : 'Connect'}
          </button>
          <button
            onClick={handleDiscover}
            disabled={discovering}
            className="bg-bg-elevated text-text-primary font-sans text-sm px-5 py-2 rounded-lg hover:bg-bg-base transition-colors disabled:opacity-40"
          >
            {discovering ? 'Scanning…' : 'Scan Network'}
          </button>
        </div>

        {/* Discover results */}
        {discoverError && <p className="text-red-400 text-sm font-sans">{discoverError}</p>}
        {discoverResults.length > 0 && (
          <div className="flex flex-col gap-2 mt-1">
            <span className="text-text-secondary text-xs font-sans">Discovered Inverters</span>
            {discoverResults.map((inv, i) => (
              <div
                key={i}
                className="bg-bg-elevated rounded-lg px-4 py-3 flex items-center justify-between"
              >
                <div className="flex flex-col gap-0.5">
                  <span className="text-text-primary text-sm font-mono">{inv.host}:{inv.port}</span>
                  <span className="text-text-secondary text-xs font-sans">
                    {inv.serial ?? 'Unknown serial'}
                    {inv.generation ? ` · ${inv.generation}` : ''}
                  </span>
                </div>
                <button
                  onClick={() => applyDiscovered(inv)}
                  className="bg-flow-active/20 text-flow-active text-xs font-sans font-semibold px-3 py-1.5 rounded-md hover:bg-flow-active/30 transition-colors"
                >
                  Use
                </button>
              </div>
            ))}
          </div>
        )}

        {/* Auto-Discovery toggle */}
        <div className="flex items-center justify-between bg-bg-elevated rounded-xl px-4 py-3 border border-white/5">
          <div className="flex flex-col gap-0.5">
            <span className="text-text-primary text-sm font-sans font-medium">Enable Auto-Discovery</span>
            <span className="text-text-secondary text-xs font-sans">
              Scan the LAN for a new dongle IP after repeated connection failures. Disable if you have multiple inverters on the same network.
            </span>
          </div>
          <Toggle
            checked={!disableAutoDiscovery}
            onChange={(v) => {
              setDisableAutoDiscovery(!v);
              apiPost('/api/settings', { disable_auto_discovery: !v })
                .then(() => flash('Auto-Discovery setting saved', true))
                .catch((e) => flash(e.message ?? 'Failed to save', false));
            }}
          />
        </div>

      </section>

      {/* ─── Section 2: Remote / Mobile Network Access ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-3">
        <h2 className="text-text-primary text-lg font-semibold font-sans">Remote / Mobile Network Access</h2>

        <div className="flex items-center gap-3">
          <code className="bg-bg-elevated text-flow-active rounded-lg px-4 py-2 text-sm font-mono flex-1 min-w-0 select-all overflow-hidden text-ellipsis whitespace-nowrap">
            {lanUrl}
          </code>
          <button
            onClick={() => copyUrl(lanUrl)}
            className="bg-bg-elevated text-text-primary font-sans text-sm px-4 py-2 rounded-lg hover:bg-bg-base transition-colors shrink-0"
          >
            Copy
          </button>
        </div>

        <p className="text-text-secondary text-xs font-sans">
          Access this dashboard from any device on your network
        </p>

        {/* Read-only link (issue #114) — share this with family members
            who only need to view the data. Visiting the URL with the `?RO`
            flag hides the Control and Settings tabs in that browser, and
            the flag is pinned via localStorage so it stays hidden across
            reloads. No server-side enforcement — the link keeps anyone
            with browser devtools out of the way, but isn't a security
            boundary. */}
        <div className="flex items-center gap-3 mt-2">
          <code className="bg-bg-elevated text-flow-active rounded-lg px-4 py-2 text-sm font-mono flex-1 min-w-0 select-all overflow-hidden text-ellipsis whitespace-nowrap">
            {lanReadOnlyUrl}
          </code>
          <button
            onClick={() => copyUrl(lanReadOnlyUrl)}
            className="bg-bg-elevated text-text-primary font-sans text-sm px-4 py-2 rounded-lg hover:bg-bg-base transition-colors shrink-0"
          >
            Copy
          </button>
        </div>
        <p className="text-text-secondary text-xs font-sans">
          Read-only link — hides Control and Settings in the visitor's browser (safe to share with family)
        </p>

        {clients.length > 0 && (
          <div className="flex flex-col gap-1.5 mt-1">
            <span className="text-text-secondary text-xs font-sans">
              Connected clients ({clients.length})
            </span>
            {clients.map((addr, i) => {
              const ip = addr.replace(/:.*$/, '');
              const isLocal = ip === '127.0.0.1' || ip === '::1' || ip === lanIp;
              return (
                <div key={i} className="bg-bg-elevated rounded-lg px-3 py-2 flex items-center justify-between">
                  <span className="text-text-primary text-sm font-mono">{addr}</span>
                  {isLocal && (
                    <span className="text-text-secondary text-xs font-sans">This device</span>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </section>

      {/* ─── Section 3: App ─── */}
      {/* Behavioural settings for the app itself — how often it polls,
          what port it serves on, and whether to start on login. Issue #117
          brought Start on Login in; the other two were previously scattered
          around this page (Sections 3b and 4) and have been moved here so
          the three "how the app runs" controls live together. */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-4">
        <h2 className="text-text-primary text-lg font-semibold font-sans">App</h2>

        {/* Refresh Interval */}
        <div className="flex flex-col gap-2">
          <span className="text-text-primary text-sm font-sans font-medium">Refresh Interval</span>
          <p className="text-text-secondary text-xs font-sans">
            How often the app polls the inverter for fresh data.
          </p>
          <div className="flex gap-2">
            {VALID_INTERVALS.map((s) => (
              <button
                key={s}
                onClick={() => handleIntervalChange(s)}
                className={`flex-1 py-2 rounded-lg text-sm font-mono transition ${
                  intervalSecs === s
                    ? 'bg-flow-active text-white font-semibold'
                    : 'bg-bg-elevated text-text-primary hover:bg-bg-elevated/80'
                }`}
              >
                {s}s
              </button>
            ))}
          </div>
        </div>

        {/* HTTP Port */}
        <div className="flex flex-col gap-2">
          <span className="text-text-primary text-sm font-sans font-medium">HTTP Port</span>
          <p className="text-text-secondary text-xs font-sans">
            Change to run multiple instances on the same machine. Requires restart.
          </p>
          <div className="flex items-center gap-3">
            <input
              type="number"
              min={1024}
              max={65535}
              value={httpPort}
              onChange={(e) => setHttpPort(Number(e.target.value))}
              className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-28 border border-transparent focus:outline-none focus:border-accent"
            />
            <button
              onClick={handleHttpPortSave}
              className="bg-flow-active text-bg-base font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 transition-opacity"
            >
              Save
            </button>
          </div>
        </div>

        {/* Start on Login (issue #117). Hidden in headless mode because
            there's no Tauri shell to register as an autostart entry —
            headless / Pi users should manage the systemd service
            directly (see INSTALL.md). */}
        {autostartSupported && (
          <div className="flex items-center justify-between">
            <div className="flex flex-col gap-0.5">
              <span className="text-text-primary text-sm font-sans font-medium">Start on Login</span>
              <span className="text-text-secondary text-xs font-sans">
                Launch the app automatically when you log in. On Windows this adds an entry to the Startup apps list; on macOS a LaunchAgent; on Linux a desktop autostart file.
              </span>
            </div>
            <Toggle
              checked={autostartEnabled}
              onChange={handleAutostartToggle}
            />
          </div>
        )}
      </section>

      {/* ─── Section 4: Energy Tariffs ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-4">
        <h2 className="text-text-primary text-lg font-semibold font-sans">Energy Tariffs</h2>
        <p className="text-text-secondary text-xs font-sans">
          Configure your electricity tariff time windows and rates. Add multiple windows for time-of-use tariffs like Octopus Flux, Cosy, or Eco7. Used for cost calculations on the History page.
        </p>

        {/* Import Tariff */}
        <TariffSlotEditor
          label="Import"
          config={importTariffCfg}
          onChange={setImportTariffCfg}
        />

        {/* Export Tariff */}
        <TariffSlotEditor
          label="Export"
          config={exportTariffCfg}
          onChange={setExportTariffCfg}
        />

        <button
          onClick={handleTariffSave}
          disabled={saving || !isTariffConfigValid(importTariffCfg) || !isTariffConfigValid(exportTariffCfg)}
          title={
            !isTariffConfigValid(importTariffCfg) || !isTariffConfigValid(exportTariffCfg)
              ? 'Tariff configuration is invalid — see errors above.'
              : undefined
          }
          className="bg-flow-active text-bg-base font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 disabled:cursor-not-allowed transition-opacity self-start"
        >
          {saving ? 'Saving…' : 'Save Tariffs'}
        </button>
      </section>

      {/* ─── Section 5: Local Weather (Open-Meteo) ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-4">
        <div className="flex items-center justify-between">
          <h2 className="text-text-primary text-lg font-semibold font-sans">Local Weather</h2>
          <button
            onClick={() => openExternal('https://open-meteo.com/')}
            className="text-flow-active text-xs font-sans underline hover:opacity-80 transition-opacity"
          >
            Open-Meteo ↗
          </button>
        </div>
        <p className="text-text-secondary text-xs font-sans">
          Fetch the local ambient temperature from the free Open-Meteo API and overlay it on the History page temperature charts. No API key required. Enter your postcode to resolve your location automatically, or enter latitude/longitude manually (useful outside the UK or for self-hosted Open-Meteo instances).
        </p>

        <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
          <div className="flex items-center justify-between">
            <span className="text-text-primary text-sm font-sans">Enable Weather</span>
            <Toggle
              checked={weatherState?.config.enabled ?? false}
              onChange={(v) => {
                // Optimistic toggle — persist immediately, like the alerts
                // enable switch above.
                setWeatherState((p) => p ? { ...p, config: { ...p.config, enabled: v } } : p);
                apiPost('/api/weather', { enabled: v })
                  .then(() => flash(v ? 'Weather enabled' : 'Weather disabled', true))
                  .catch((e) => flash(e instanceof Error ? e.message : 'Failed to save', false));
              }}
            />
          </div>
        </div>

        {(weatherState?.config.enabled ?? false) && (
          <div className="flex flex-col gap-4">
            <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
              <h3 className="text-text-primary text-sm font-sans font-medium">Location</h3>
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">Postcode (UK — resolved via api.postcodes.io)</span>
                <input
                  type="text"
                  placeholder="e.g. SW1A 1AA"
                  value={weatherPostcode}
                  onChange={(e) => setWeatherPostcode(e.target.value)}
                  className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                />
              </label>

              <div className="flex items-center gap-3 my-1">
                <div className="flex-1 h-px bg-white/5" />
                <span className="text-text-secondary/60 text-[10px] font-sans uppercase tracking-wide">or manual coordinates</span>
                <div className="flex-1 h-px bg-white/5" />
              </div>

              <div className="grid grid-cols-2 gap-3">
                <label className="flex flex-col gap-1">
                  <span className="text-text-secondary text-xs font-sans">Latitude</span>
                  <input
                    type="number"
                    step="any"
                    placeholder="51.501"
                    value={weatherLat}
                    onChange={(e) => setWeatherLat(e.target.value)}
                    className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                  />
                </label>
                <label className="flex flex-col gap-1">
                  <span className="text-text-secondary text-xs font-sans">Longitude</span>
                  <input
                    type="number"
                    step="any"
                    placeholder="-0.141"
                    value={weatherLon}
                    onChange={(e) => setWeatherLon(e.target.value)}
                    className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                  />
                </label>
              </div>

              {/* Resolved location + last-fetch status */}
              {weatherState && (weatherState.config.latitude != null || weatherState.last_fetched_temperature_c != null) && (
                <div className="text-text-secondary/80 text-xs font-sans flex flex-col gap-1 mt-1">
                  {weatherState.config.latitude != null && weatherState.config.longitude != null && (
                    <span>
                      Resolved grid cell:{' '}
                      <code className="font-mono">
                        {(weatherState.grid_cell_latitude ?? weatherState.config.latitude).toFixed(3)},
                        {' '}{(weatherState.grid_cell_longitude ?? weatherState.config.longitude).toFixed(3)}
                      </code>
                    </span>
                  )}
                  {weatherState.last_fetched_temperature_c != null && weatherState.last_fetch_at && (
                    <span>
                      Last reading: <strong className="text-text-primary">{weatherState.last_fetched_temperature_c.toFixed(1)}°C</strong>{' '}
                      ({new Date(weatherState.last_fetch_at).toLocaleString()})
                    </span>
                  )}
                </div>
              )}
              {weatherState?.last_error && (
                <p className="text-red-400/80 text-xs font-sans mt-1">
                  {weatherState.last_error}
                </p>
              )}
            </div>

            <div className="flex flex-col sm:flex-row gap-2">
              <button
                onClick={handleWeatherSave}
                disabled={weatherSaving}
                className="bg-flow-active text-bg-base font-sans font-semibold text-sm px-4 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 transition-opacity sm:w-auto"
              >
                {weatherSaving ? 'Saving…' : 'Save Location'}
              </button>
              <button
                onClick={handleWeatherBackfill}
                disabled={weatherBackfilling || weatherState?.config.latitude == null}
                title={weatherState?.config.latitude == null ? 'Save a location first' : undefined}
                className="bg-bg-elevated text-text-primary font-sans font-semibold text-sm px-4 py-2 rounded-lg hover:opacity-80 disabled:opacity-40 transition-opacity border border-white/5 sm:w-auto"
              >
                {weatherBackfilling ? 'Backfilling…' : 'Backfill History'}
              </button>
            </div>

            {weatherBackfilling && (
              <p className="text-text-secondary/70 text-xs font-sans">
                Fetching historical weather one month at a time from the Open-Meteo archive. This runs in the background — you can leave this page.
              </p>
            )}
            {weatherState?.config.last_backfill_completed && !weatherBackfilling && (
              <p className="text-text-secondary/70 text-xs font-sans">
                History backfilled through{' '}
                <strong className="text-text-primary">{weatherState.config.last_backfill_completed}</strong>.
              </p>
            )}
          </div>
        )}

        {/* CC BY 4.0 attribution — required by the Open-Meteo licence */}
        <p className="text-text-secondary/60 text-[11px] font-sans">
          Weather data by{' '}
          <button
            onClick={() => openExternal('https://open-meteo.com/')}
            className="text-flow-active underline hover:opacity-80 inline"
          >
            Open-Meteo.com
          </button>
          {' '}— licensed under CC BY 4.0.
        </p>
      </section>

      {/* ─── Section 6: Notifications ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-4">
        <div className="flex items-center justify-between">
          <h2 className="text-text-primary text-lg font-semibold font-sans">Notifications</h2>
          <button
            onClick={() => openExternal('https://github.com/psylsph/home-energy-manager/blob/master/NOTIFICATIONS.md')}
            className="text-flow-active text-xs font-sans underline hover:opacity-80 transition-opacity"
          >
            Setup guide ↗
          </button>
        </div>

        <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
          <div className="flex items-center justify-between">
            <span className="text-text-primary text-sm font-sans">Enable Alerts</span>
            <Toggle
              checked={alertsConfig.enabled}
              onChange={(v) => {
                setAlertsConfig((p) => ({ ...p, enabled: v }));
                apiPost('/api/alerts', { enabled: v })
                  .then(() => flash(v ? 'Alerts enabled' : 'Alerts disabled', true))
                  .catch((e) => flash(e.message ?? 'Failed to save', false));
              }}
            />
          </div>
        </div>

        {alertsConfig.enabled && (
          <div className="flex flex-col gap-4">
            {/* Credentials */}
            <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
              <h3 className="text-text-primary text-sm font-sans font-medium">Telegram</h3>
              <p className="text-text-secondary text-xs font-sans">
                <strong className="text-green-400">Recommended</strong> — Send alerts when critical conditions are detected. Create a bot via{' '}
                <button onClick={() => openExternal('https://t.me/botfather')} className="text-flow-active underline hover:opacity-80 inline">@BotFather</button> on Telegram, get your bot token, then send /start to your bot and get your chat ID from{' '}
                <button onClick={() => openExternal('https://t.me/userinfobot')} className="text-flow-active underline hover:opacity-80 inline">@userinfobot</button>.
                Once configured, send <code>/status</code>, <code>/today</code>, <code>/battery</code>, or <code>/help</code> in the chat to ask your inverter a question. The bot only replies to this chat id.
              </p>
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">Bot Token (from @BotFather)</span>
                <input
                  type="password" placeholder="123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11"
                  value={alertsConfig.telegram_bot_token}
                  onChange={(e) => setAlertsConfig((p) => ({ ...p, telegram_bot_token: e.target.value }))}
                  className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                />
              </label>
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">Chat ID (from @userinfobot)</span>
                <input
                  type="text" placeholder="123456789"
                  value={alertsConfig.telegram_chat_id}
                  onChange={(e) => setAlertsConfig((p) => ({ ...p, telegram_chat_id: e.target.value }))}
                  className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                />
              </label>
              <div className="flex justify-center gap-4 mt-1">
                <div className="flex flex-col items-center gap-1">
                  <StoreQr url="https://play.google.com/store/apps/details?id=org.telegram.messenger" alt="Telegram Android" />
                  <span className="text-text-secondary text-[10px] font-sans">Android</span>
                </div>
                <div className="flex flex-col items-center gap-1">
                  <StoreQr url="https://apps.apple.com/app/telegram/id686449807" alt="Telegram iOS" />
                  <span className="text-text-secondary text-[10px] font-sans">iOS</span>
                </div>
              </div>
            </div>

            {/* ntfy.sh */}
            <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
              <h3 className="text-text-primary text-sm font-sans font-medium">ntfy Push Notifications</h3>
              <p className="text-text-secondary/70 text-xs font-sans">
                Free push notifications via&nbsp;
                <button onClick={() => openExternal('https://ntfy.sh')} className="text-flow-active underline hover:opacity-80 inline">ntfy.sh</button>.
                Install the app on your phone and subscribe to the topic below.
                A topic is auto-generated from your inverter serial (unique to you) — edit it only if you want a custom one.
              </p>
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">Topic (subscribe to this in the ntfy app)</span>
                <div className="flex items-center gap-2">
                  <input
                    type="text"
                    placeholder={generatedNtfyTopic || 'hem-your-inverter-serial'}
                    value={alertsConfig.ntfy_topic}
                    onChange={(e) => setAlertsConfig((p) => ({ ...p, ntfy_topic: e.target.value }))}
                    className="flex-1 bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                  />
                  <button
                    onClick={() => { navigator.clipboard.writeText(effectiveNtfyTopic); flash('Topic copied!', true); }}
                    disabled={!effectiveNtfyTopic}
                    className="shrink-0 bg-flow-active text-bg-base text-xs font-sans font-semibold px-3 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 transition-opacity"
                  >
                    Copy
                  </button>
                </div>
                {!alertsConfig.ntfy_topic && generatedNtfyTopic && (
                  <span className="text-text-secondary/60 text-xs font-sans">
                    Using generated topic: <code className="font-mono">{generatedNtfyTopic}</code>
                  </span>
                )}
                {!alertsConfig.ntfy_topic && !generatedNtfyTopic && (
                  <span className="text-text-secondary/50 text-xs font-sans italic">
                    Connect to an inverter to auto-generate a topic, or enter one manually.
                  </span>
                )}
              </label>
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">Server (optional, default: ntfy.sh)</span>
                <input
                  type="text" placeholder="https://ntfy.sh"
                  value={alertsConfig.ntfy_server}
                  onChange={(e) => setAlertsConfig((p) => ({ ...p, ntfy_server: e.target.value }))}
                  className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                />
              </label>
            </div>

            {/* Pushover */}
            <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
              <h3 className="text-text-primary text-sm font-sans font-medium">Pushover</h3>
              <p className="text-text-secondary/70 text-xs font-sans">
                Paid-once-per-platform push notifications via{' '}
                <button onClick={() => openExternal('https://pushover.net')} className="text-flow-active underline hover:opacity-80 inline">Pushover</button>.
                Create an application at{' '}
                <button onClick={() => openExternal('https://pushover.net/apps/build')} className="text-flow-active underline hover:opacity-80 inline">pushover.net/apps/build</button>{' '}
                to get your App API Token, then copy your User Key from your Pushover account settings.
              </p>
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">App API Token (from pushover.net/apps/build)</span>
                <input
                  type="password" placeholder="Your App API Token"
                  value={alertsConfig.pushover_app_token}
                  onChange={(e) => setAlertsConfig((p) => ({ ...p, pushover_app_token: e.target.value }))}
                  className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                />
              </label>
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">User Key (from your Pushover account)</span>
                <input
                  type="password" placeholder="Your User Key"
                  value={alertsConfig.pushover_user_key}
                  onChange={(e) => setAlertsConfig((p) => ({ ...p, pushover_user_key: e.target.value }))}
                  className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                />
              </label>
            </div>

            {/* Battery temperature & SOC thresholds */}
            <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
              <div>
                <h3 className="text-text-primary text-sm font-sans font-medium">Battery Temperature &amp; SOC</h3>
                <p className="text-text-secondary/70 text-xs font-sans">
                  Battery temperature alerts only work with inverters that report
                  temperature. Not available with a Gateway at this time.
                </p>
              </div>
              <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
                <label className="flex flex-col gap-1">
                  <span className="text-text-secondary text-xs font-sans">Temp below °C</span>
                  <input
                    type="number" step="0.5" min="0" max="50"
                    value={alertsConfig.batt_temp_min}
                    onChange={(e) => setAlertsConfig((p) => ({ ...p, batt_temp_min: Number(e.target.value) }))}
                    className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-full border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                  />
                </label>
                <label className="flex flex-col gap-1">
                  <span className="text-text-secondary text-xs font-sans">Temp above °C</span>
                  <input
                    type="number" step="0.5" min="0" max="80"
                    value={alertsConfig.batt_temp_max}
                    onChange={(e) => setAlertsConfig((p) => ({ ...p, batt_temp_max: Number(e.target.value) }))}
                    className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-full border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                  />
                </label>
                <label className="flex flex-col gap-1">
                  <span className="text-text-secondary text-xs font-sans">SOC below %</span>
                  <input
                    type="number" min="0" max="100"
                    value={alertsConfig.soc_min}
                    onChange={(e) => setAlertsConfig((p) => ({ ...p, soc_min: Number(e.target.value) }))}
                    className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-full border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                  />
                </label>
                <label className="flex flex-col gap-1">
                  <span className="text-text-secondary text-xs font-sans">SOC above %</span>
                  <input
                    type="number" min="0" max="100"
                    value={alertsConfig.soc_max}
                    onChange={(e) => setAlertsConfig((p) => ({ ...p, soc_max: Number(e.target.value) }))}
                    className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-full border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                  />
                </label>
              </div>
            </div>

            {/* Toggle alerts */}
            <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
              <h3 className="text-text-primary text-sm font-sans font-medium">Alert Triggers & Cooldown</h3>
              <div className="flex items-center justify-between">
                <span className="text-text-primary text-sm font-sans">Grid Offline</span>
                <Toggle
                  checked={alertsConfig.grid_offline_enabled}
                  onChange={(v) => setAlertsConfig((p) => ({ ...p, grid_offline_enabled: v }))}
                />
              </div>
              <div className="flex items-center justify-between">
                <span className="text-text-primary text-sm font-sans">Inverter Battery Warning</span>
                <Toggle
                  checked={alertsConfig.battery_over_temp_enabled}
                  onChange={(v) => setAlertsConfig((p) => ({ ...p, battery_over_temp_enabled: v }))}
                />
              </div>
              <div className="flex items-center justify-between">
                <span className="text-text-primary text-sm font-sans">
                  Solar Clipping
                  <span className="block text-text-secondary text-xs font-sans">
                    Alert when generation sustains above your inverter's rated limit
                  </span>
                </span>
                <Toggle
                  checked={alertsConfig.solar_clipping_enabled}
                  onChange={(v) => setAlertsConfig((p) => ({ ...p, solar_clipping_enabled: v }))}
                />
              </div>
              {alertsConfig.solar_clipping_enabled && (
                <label className="flex items-center justify-between gap-3">
                  <span className="text-text-secondary text-xs font-sans">
                    Clipping Ceiling (W) — rated AC output, 0 = off
                  </span>
                  <input
                    type="number" step="100" min="0" max="100000"
                    value={alertsConfig.solar_clipping_ceiling_w}
                    onChange={(e) => setAlertsConfig((p) => ({ ...p, solar_clipping_ceiling_w: Number(e.target.value) }))}
                    className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-28 border border-bg-elevated focus:border-flow-active outline-none transition-colors text-left"
                  />
                </label>
              )}
              <div className="flex items-center justify-between">
                <span className="text-text-primary text-sm font-sans">
                  Connection Lost
                  <span className="block text-text-secondary text-xs font-sans">
                    Notify when contact with the inverter is dropped and restored
                  </span>
                </span>
                <Toggle
                  checked={alertsConfig.connection_lost_enabled}
                  onChange={(v) => setAlertsConfig((p) => ({ ...p, connection_lost_enabled: v }))}
                />
              </div>
              <div className="flex flex-col gap-1">
                <span className="text-text-primary text-sm font-sans font-medium">Cooldown Timer</span>
                <label className="flex items-center justify-between gap-2">
                  <span className="text-text-secondary text-xs font-sans">minutes between all alerts</span>
                  <input
                    type="number" min={1} max={1440}
                    value={alertsConfig.cooldown_minutes}
                    onChange={(e) => setAlertsConfig((p) => ({ ...p, cooldown_minutes: Number(e.target.value) }))}
                    className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-28 border border-bg-elevated focus:border-flow-active outline-none transition-colors text-left"
                  />
                </label>
              </div>
            </div>

          </div>
        )}

        <div className="flex flex-col sm:flex-row gap-2">
          <button
            onClick={handleAlertsSave}
            disabled={alertsSaving}
            className="bg-flow-active text-bg-base font-sans font-semibold text-sm px-4 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 transition-opacity sm:w-auto"
          >
            {alertsSaving ? 'Saving…' : 'Save Notification Settings'}
          </button>
          <button
            onClick={handleAlertsTest}
            disabled={alertsTesting || ((!alertsConfig.telegram_bot_token || !alertsConfig.telegram_chat_id) && !effectiveNtfyTopic && (!alertsConfig.pushover_app_token || !alertsConfig.pushover_user_key))}
            className="bg-bg-elevated text-text-primary font-sans font-semibold text-sm px-4 py-2 rounded-lg hover:opacity-80 disabled:opacity-40 transition-opacity border border-white/5 sm:w-auto"
          >
            {alertsTesting ? 'Sending…' : 'Send Test Notification'}
          </button>
        </div>
      </section>

      {/* ─── Section 6b: Support ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-4">
        <h2 className="text-text-primary text-lg font-semibold font-sans">Submit a Support Bundle</h2>
        <p className="text-text-secondary text-xs font-sans">
          Bundles your current inverter data, developer logs, and sanitised settings (secrets
          stripped) into a single file and sends it to the maintainer. Describe what's wrong and
          click submit — no need to copy logs by hand.
        </p>

        <div className="flex flex-col gap-3">
          <label className="flex flex-col gap-1">
            <span className="text-text-secondary text-xs font-sans">What's the issue?</span>
            <select
              value={supportCategory}
              onChange={(e) => setSupportCategory(e.target.value)}
              className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-sans border border-bg-elevated focus:border-flow-active outline-none transition-colors"
            >
              <option value="connection">Connection</option>
              <option value="schedule">Charge / discharge schedule</option>
              <option value="battery">Battery</option>
              <option value="control">Control</option>
              <option value="alerts">Notifications / alerts</option>
              <option value="other">Other</option>
            </select>
          </label>

          <label className="flex flex-col gap-1">
            <span className="text-text-secondary text-xs font-sans">
              GitHub issue number (optional)
            </span>
            <input
              type="text"
              value={supportIssueNumber}
              onChange={(e) => setSupportIssueNumber(e.target.value)}
              placeholder="e.g. 125 — links this bundle to the ticket"
              className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
            />
          </label>

          <label className="flex flex-col gap-1">
            <span className="text-text-secondary text-xs font-sans">
              Describe the problem{' '}
              <span className="text-text-secondary/60">
                ({supportDescription.length}/2000)
              </span>
            </span>
            <textarea
              value={supportDescription}
              maxLength={2000}
              onChange={(e) => setSupportDescription(e.target.value)}
              placeholder="What were you trying to do? What did you expect? What happened instead?"
              rows={4}
              className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-sans border border-bg-elevated focus:border-flow-active outline-none transition-colors resize-y"
            />
          </label>

          <div className="flex flex-col gap-2">
            <label className="flex items-center gap-2 cursor-pointer select-none">
              <input
                type="checkbox"
                checked={supportIncludeHistory}
                onChange={(e) => setSupportIncludeHistory(e.target.checked)}
                className="accent-flow-active"
              />
              <span className="text-text-secondary text-xs font-sans">
                Include last 24 h of history readings
              </span>
            </label>
          </div>

          <button
            onClick={handleSubmitSupport}
            disabled={supportSubmitting || !supportDescription.trim()}
            className="bg-flow-active text-bg-base font-sans font-semibold text-sm px-4 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 transition-opacity self-start"
          >
            {supportSubmitting ? 'Submitting…' : 'Submit Support Bundle'}
          </button>
        </div>
      </section>

      {/* ─── Section 7: Panel Controls ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-4">
        <h2 className="text-text-primary text-lg font-semibold font-sans">Panel Controls</h2>

        {/* ── Sub-section: Panel Visibility ── */}
        <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
          <h3 className="text-text-primary text-sm font-sans font-medium">Panel Visibility</h3>
          <p className="text-text-secondary text-xs font-sans">
            Hide panels you don't use from the bottom navigation bar
          </p>
          <div className="grid grid-cols-2 sm:grid-cols-3 gap-3">
            {([
              ['power', 'Power'],
              ['battery', 'Battery'],
              ['solar', 'Solar'],
              ['meters', 'Meters'],
              ['history', 'History'],
              ['control', 'Control'],
            ] as const).map(([key, label]) => (
              <label key={key} className="flex items-center gap-2 cursor-pointer select-none bg-bg-elevated rounded-xl px-4 py-3 border border-white/5 hover:border-white/10 transition-colors">
                <input
                  type="checkbox"
                  checked={!hiddenPanels.includes(key)}
                  onChange={() => {
                    setHiddenPanels(prev =>
                      prev.includes(key)
                        ? prev.filter(p => p !== key)
                        : [...prev, key]
                    );
                  }}
                  className="w-4 h-4 accent-battery rounded"
                />
                <span className="text-text-primary text-sm font-sans">{label}</span>
              </label>
            ))}
          </div>
          <button
            onClick={handlePanelSave}
            className="self-start bg-flow-active text-bg-base font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 transition-opacity"
          >
            {panelSaving ? 'Saving…' : 'Save Panel Visibility'}
          </button>
        </div>

        {/* ── Sub-section: Panel Graphs ── */}
        <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
          <h3 className="text-text-primary text-sm font-sans font-medium">Panel Graphs</h3>
          <p className="text-text-secondary text-xs font-sans">
            Toggle the trend charts on the Battery and Solar tabs, and choose their time scale
          </p>

          {/* Show graphs toggle */}
          <div className="flex items-center justify-between">
            <span className="text-text-primary text-sm font-sans">Show Graphs</span>
            <Toggle
              checked={panelGraphsEnabled}
              onChange={setPanelGraphsEnabled}
            />
          </div>

          {/* Time scale selector — disabled when graphs are off */}
          <div className={`flex flex-col gap-2 transition-opacity ${panelGraphsEnabled ? '' : 'opacity-40 pointer-events-none'}`}>
            <span className="text-text-secondary text-xs font-sans">Time Scale</span>
            <div className="flex gap-2">
              {([
                ['today', 'Today'],
                ['24h', 'Rolling 24H'],
              ] as const).map(([key, label]) => (
                <button
                  key={key}
                  type="button"
                  onClick={() => setPanelGraphsScale(key)}
                  className={`flex-1 py-2 rounded-lg text-sm font-sans transition ${
                    panelGraphsScale === key
                      ? 'bg-flow-active text-white font-semibold'
                      : 'bg-bg-elevated text-text-primary hover:bg-bg-elevated/80'
                  }`}
                >
                  {label}
                </button>
              ))}
            </div>
          </div>

          {/* Y-axis lock toggle */}
          <div className="flex items-center justify-between">
            <span className="text-text-primary text-sm font-sans">Lock Y-axis scale</span>
            <Toggle
              checked={panelGraphsYLock}
              onChange={setPanelGraphsYLock}
            />
          </div>
          {panelGraphsYLock && (
            <p className="text-text-secondary text-xs font-sans">
              Charts Y-axis locks to a clean ceiling based on the data maximum instead of auto-fitting
            </p>
          )}
        </div>

        {/* ── Sub-section: Energy Flow Diagram ── */}
        <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
          <h3 className="text-text-primary text-sm font-sans font-medium">Energy Flow Diagram</h3>
          <p className="text-text-secondary text-xs font-sans">
            Flows below this wattage are treated as zero — no animated line, no arrow, displayed value rounds to 0W.
            Prevents tiny readings from cluttering the diagram.
          </p>
          <div className="flex items-center gap-3">
            <input
              type="number"
              min={0}
              max={100}
              step={5}
              value={visualNoiseThreshold}
              onChange={(e) => {
                const v = parseInt(e.target.value, 10);
                if (!Number.isFinite(v)) return;
                setVisualNoiseThreshold(Math.max(0, Math.min(100, v)));
              }}
              className="w-20 bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors text-center"
            />
            <span className="text-text-secondary text-sm font-sans">watts</span>
          </div>
          <div className="flex items-center gap-2">
            <input
              type="range"
              min={0}
              max={100}
              step={5}
              value={visualNoiseThreshold}
              onChange={(e) => setVisualNoiseThreshold(Number(e.target.value))}
              className="w-full"
            />
            <span className="text-text-secondary text-xs font-sans w-8 text-right tabular-nums">{visualNoiseThreshold}W</span>
          </div>
        </div>
      </section>

      {/* ─── Section 8: EV Charger ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-3">
        <h2 className="text-text-primary text-lg font-semibold font-sans">EV Charger</h2>
        <p className="text-text-secondary text-xs font-sans">
          Optional. Connect a GivEnergy EV Charger on your local network for read-only monitoring. Uses standard Modbus TCP (port 502), not the proprietary inverter protocol.
        </p>

        <label className="flex flex-col gap-1">
          <span className="text-text-secondary text-xs font-sans">Charger Address</span>
          <div className="flex gap-2">
            <input
              type="text"
              value={evcHost}
              onChange={(e) => setEvcHost(e.target.value)}
              placeholder="e.g. 192.168.1.50 (leave blank to disable)"
              aria-invalid={evcHostInvalid}
              className={`min-w-0 flex-1 bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border outline-none transition-colors ${
                evcHostInvalid
                  ? 'border-red-500 focus:border-red-400'
                  : 'border-bg-elevated focus:border-flow-active'
              }`}
            />
            {developerMode && (
              <input
                type="number"
                value={evcPort}
                onChange={(e) => setEvcPort(Number(e.target.value))}
                title="EV Charger Modbus port"
                className="w-[5.5em] shrink-0 bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
              />
            )}
          </div>
          {evcHostInvalid && (
            <span className="text-red-400 text-xs font-sans">
              Must be four numbers separated by dots, e.g. <span className="font-mono">192.168.1.50</span>.
            </span>
          )}
        </label>

        <div className="flex gap-3 pt-1">
          <button
            onClick={handleEvcSave}
            disabled={evcHostInvalid}
            className="bg-flow-active text-bg-base font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 transition-opacity disabled:opacity-40 disabled:cursor-not-allowed"
          >
            Save
          </button>
          <button
            onClick={handleEvcDiscover}
            disabled={evcDiscovering}
            className="bg-bg-elevated text-text-primary font-sans text-sm px-5 py-2 rounded-lg hover:bg-bg-base transition-colors disabled:opacity-40"
          >
            {evcDiscovering ? 'Scanning…' : 'Scan Network'}
          </button>
        </div>

        {/* EVC discover results */}
        {evcDiscoverError && <p className="text-red-400 text-sm font-sans">{evcDiscoverError}</p>}
        {evcDiscoverResults.length > 0 && (
          <div className="flex flex-col gap-2 mt-1">
            <span className="text-text-secondary text-xs font-sans">Discovered EV Chargers</span>
            {evcDiscoverResults.map((charger, i) => (
              <div
                key={i}
                className="bg-bg-elevated rounded-lg px-4 py-3 flex items-center justify-between"
              >
                <div className="flex flex-col gap-0.5">
                  <span className="text-text-primary text-sm font-mono">{charger.host}:{charger.port}</span>
                  <span className="text-text-secondary text-xs font-sans">
                    {charger.serial ?? 'Standard Modbus TCP device'}
                  </span>
                </div>
                <button
                  onClick={() => applyDiscoveredEvc(charger)}
                  className="bg-flow-active/20 text-flow-active text-xs font-sans font-semibold px-3 py-1.5 rounded-md hover:bg-flow-active/30 transition-colors"
                >
                  Use
                </button>
              </div>
            ))}
          </div>
        )}
      </section>



      {/* ─── Section 9: Developer Mode ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-3">
        <h2 className="text-text-primary text-lg font-semibold font-sans">Developer</h2>
        <div className="flex items-center justify-between">
          <div className="flex flex-col gap-0.5">
            <span className="text-text-primary text-sm font-sans">Developer Mode</span>
          </div>
          <Toggle checked={developerMode} onChange={setDeveloperMode} />
        </div>
        {developerMode && (
          <div className="flex flex-col gap-3 pt-2 border-t border-bg-elevated">
            <p className="text-text-secondary text-xs font-sans">
              Read-only API for external access (e.g. SolarWatch). Starts a
              second HTTP server on a separate port with Bearer-token auth.
              Only <code className="text-text-primary">GET /api/snapshot</code> is exposed.
            </p>
            <label className="flex flex-col gap-1">
              <span className="text-text-secondary text-xs font-sans">API Key</span>
              <input
                type="text"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder="Leave empty to disable"
                className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
              />
            </label>
            <label className="flex flex-col gap-1">
              <span className="text-text-secondary text-xs font-sans">Port</span>
              <input
                type="number"
                value={apiPort || ''}
                onChange={(e) => setApiPort(Number(e.target.value))}
                placeholder="e.g. 7338"
                className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors w-32"
              />
            </label>
            <button
              onClick={async () => {
                await apiPost('/api/settings', { api_key: apiKey, api_port: apiPort });
                setMessage({ text: 'API key saved. Restart the app for the read-only server to start.', ok: true });
              }}
              className="self-start bg-flow-active text-bg-base font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 transition-opacity"
            >
              Save API Key
            </button>
          </div>
        )}
      </section>

      {/* ─── Section 10: About ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-2">
        <h2 className="text-text-primary text-lg font-semibold font-sans">About</h2>
        <button
          type="button"
          onClick={() =>
            openExternal('https://psylsph.github.io/home-energy-manager/')
          }
          className="text-flow-active text-sm font-sans hover:underline mt-1 text-left"
        >
          psylsph.github.io/home-energy-manager
        </button>
      </section>
    </div>
  );
}
