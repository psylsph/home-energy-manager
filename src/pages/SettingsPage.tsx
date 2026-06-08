import { useState, useEffect, useCallback } from 'react';
import { apiGet, apiPost, getApiBase, getServerPort } from '../lib/api';
import type { PollSettings, DiscoveredInverter, TariffConfig } from '../lib/types';
import { useInverterStore } from '../store/useInverterStore';

function Toggle({ checked, onChange }: { checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <div className="relative cursor-pointer" onClick={() => onChange(!checked)}>
      <div className={`w-10 h-5 rounded-full transition-colors ${checked ? 'bg-flow-active/40' : 'bg-bg-elevated'}`} />
      <div className={`absolute left-0.5 top-0.5 w-4 h-4 rounded-full transition-all ${checked ? 'translate-x-5 bg-flow-active' : 'bg-text-secondary'}`} />
    </div>
  );
}

function DevTools() {
  const [forced, setForced] = useState(
    () => localStorage.getItem('dev_force_cold_warning') === 'true'
  );

  return (
    <div className="flex items-center justify-between pt-2 border-t border-white/5">
      <div className="flex flex-col gap-0.5">
        <span className="text-text-primary text-sm font-sans">Test Cold Battery Warning</span>
        <span className="text-text-secondary text-xs font-sans">
          Force the warning banner to show on Status / Battery pages
        </span>
      </div>
      <Toggle
        checked={forced}
        onChange={(v) => {
          localStorage.setItem('dev_force_cold_warning', String(v));
          setForced(v);
        }}
      />
    </div>
  );
}

export default function SettingsPage() {
  const { connectionState, connectedHost, developerMode, setDeveloperMode } = useInverterStore();

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

  // Snap a poll interval to the nearest valid value (5, 10, 15, or 20).
  // Used on load so the UI always shows one of the 4 buttons as active.
  const VALID_INTERVALS = [5, 10, 15, 20];
  const clampInterval = (v: number) => VALID_INTERVALS.reduce((a, b) =>
    Math.abs(b - v) < Math.abs(a - v) ? b : a
  );

  // HTTP server port
  const [httpPort, setHttpPort] = useState(7337);

  // Tariffs
  const [importTariffCfg, setImportTariffCfg] = useState<TariffConfig>({
    peak_rate: 0.285, off_peak_rate: 0.09, off_peak_start: '00:30', off_peak_end: '05:30',
  });
  const [exportTariffCfg, setExportTariffCfg] = useState<TariffConfig>({
    peak_rate: 0.15, off_peak_rate: 0.05, off_peak_start: '00:30', off_peak_end: '05:30',
  });

  // General
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<{ text: string; ok: boolean } | null>(null);
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  const [lanIp, setLanIp] = useState<string | null>(null);
  const [clients, setClients] = useState<string[]>([]);

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
        } else {
          setImportTariffCfg((p) => ({ ...p, peak_rate: s.import_tariff ?? 0.285 }));
        }
        if (s.export_tariff_config) {
          setExportTariffCfg(s.export_tariff_config);
        } else {
          setExportTariffCfg((p) => ({ ...p, peak_rate: s.export_tariff ?? 0.15 }));
        }
        setSettingsLoaded(true);
      } catch (e: unknown) {
        console.warn('Failed to load settings:', e);
        setSettingsLoaded(true);
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

  // Network URL — use LAN IP if available, otherwise fall back to getApiBase()
  const lanUrl = lanIp ? `http://${lanIp}:${getServerPort()}` : getApiBase();

  // Save connection
  const handleConnect = async () => {
    setSaving(true);
    try {
      await apiPost('/api/settings', { host, port, serial });
      flash('Settings saved — reconnecting…', true);
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

  // Save tariffs
  const handleTariffSave = async () => {
    setSaving(true);
    try {
      await apiPost('/api/settings', {
        import_tariff_config: importTariffCfg,
        export_tariff_config: exportTariffCfg,
      });
      flash('Tariff rates saved', true);
    } catch {
      flash('Failed to save tariffs', false);
    }
    setSaving(false);
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

  const copyUrl = () => {
    const text = lanUrl;
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

      {/* ─── Section 1: Connection ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-4">
        <h2 className="text-text-primary text-lg font-semibold font-sans">Connection</h2>

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
            <input
              type="number"
              value={port}
              onChange={(e) => setPort(Number(e.target.value))}
              className="w-[5.5em] shrink-0 bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
            />
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
      </section>

      {/* ─── Section 2: Network Access ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-3">
        <h2 className="text-text-primary text-lg font-semibold font-sans">Network Access</h2>

        <div className="flex items-center gap-3">
          <code className="bg-bg-elevated text-flow-active rounded-lg px-4 py-2 text-sm font-mono flex-1 min-w-0 select-all overflow-hidden text-ellipsis whitespace-nowrap">
            {lanUrl}
          </code>
          <button
            onClick={copyUrl}
            className="bg-bg-elevated text-text-primary font-sans text-sm px-4 py-2 rounded-lg hover:bg-bg-base transition-colors shrink-0"
          >
            Copy
          </button>
        </div>

        <p className="text-text-secondary text-xs font-sans">
          Access this dashboard from any device on your network
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

      {/* ─── Section 3: Refresh Interval ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-3">
        <h2 className="text-text-primary text-lg font-semibold font-sans">Refresh Interval</h2>

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
      </section>

      {/* ─── Section 3b: HTTP Port ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-3">
        <h2 className="text-text-primary text-lg font-semibold font-sans">HTTP Port</h2>
        <p className="text-text-secondary text-sm font-sans">
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
      </section>

      {/* ─── Section 4: Energy Tariffs ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-4">
        <h2 className="text-text-primary text-lg font-semibold font-sans">Energy Tariffs</h2>
        <p className="text-text-secondary text-xs font-sans">
          Used for cost calculations on the History page — supports peak and off-peak rates
        </p>

        {/* Import Tariff */}
        <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
          <h3 className="text-text-primary text-sm font-sans font-medium">Import</h3>
          <div className="grid grid-cols-2 gap-3">
            <label className="flex flex-col gap-1">
              <span className="text-text-secondary text-xs font-sans">Peak rate (£/kWh)</span>
              <input
                type="number" step="0.001" min="0"
                value={importTariffCfg.peak_rate}
                onChange={(e) => setImportTariffCfg((p) => ({ ...p, peak_rate: Number(e.target.value) }))}
                className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
              />
            </label>
            <label className="flex flex-col gap-1">
              <span className="text-text-secondary text-xs font-sans">Off-peak rate (£/kWh)</span>
              <input
                type="number" step="0.001" min="0"
                value={importTariffCfg.off_peak_rate}
                onChange={(e) => setImportTariffCfg((p) => ({ ...p, off_peak_rate: Number(e.target.value) }))}
                className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
              />
            </label>
          </div>
          <div className="grid grid-cols-2 gap-3">
            <label className="flex flex-col gap-1">
              <span className="text-text-secondary text-xs font-sans">Off-peak start</span>
              <input
                type="text" placeholder="HH:MM"
                value={importTariffCfg.off_peak_start}
                onChange={(e) => setImportTariffCfg((p) => ({ ...p, off_peak_start: e.target.value }))}
                className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
              />
            </label>
            <label className="flex flex-col gap-1">
              <span className="text-text-secondary text-xs font-sans">Off-peak end</span>
              <input
                type="text" placeholder="HH:MM"
                value={importTariffCfg.off_peak_end}
                onChange={(e) => setImportTariffCfg((p) => ({ ...p, off_peak_end: e.target.value }))}
                className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
              />
            </label>
          </div>
          <p className="text-text-secondary/60 text-xs font-sans">
            Times in 24h format. End before start = crosses midnight (e.g. 23:00 — 05:30).
          </p>
        </div>

        {/* Export Tariff */}
        <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
          <h3 className="text-text-primary text-sm font-sans font-medium">Export</h3>
          <div className="grid grid-cols-2 gap-3">
            <label className="flex flex-col gap-1">
              <span className="text-text-secondary text-xs font-sans">Peak rate (£/kWh)</span>
              <input
                type="number" step="0.001" min="0"
                value={exportTariffCfg.peak_rate}
                onChange={(e) => setExportTariffCfg((p) => ({ ...p, peak_rate: Number(e.target.value) }))}
                className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
              />
            </label>
            <label className="flex flex-col gap-1">
              <span className="text-text-secondary text-xs font-sans">Off-peak rate (£/kWh)</span>
              <input
                type="number" step="0.001" min="0"
                value={exportTariffCfg.off_peak_rate}
                onChange={(e) => setExportTariffCfg((p) => ({ ...p, off_peak_rate: Number(e.target.value) }))}
                className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
              />
            </label>
          </div>
          <div className="grid grid-cols-2 gap-3">
            <label className="flex flex-col gap-1">
              <span className="text-text-secondary text-xs font-sans">Off-peak start</span>
              <input
                type="text" placeholder="HH:MM"
                value={exportTariffCfg.off_peak_start}
                onChange={(e) => setExportTariffCfg((p) => ({ ...p, off_peak_start: e.target.value }))}
                className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
              />
            </label>
            <label className="flex flex-col gap-1">
              <span className="text-text-secondary text-xs font-sans">Off-peak end</span>
              <input
                type="text" placeholder="HH:MM"
                value={exportTariffCfg.off_peak_end}
                onChange={(e) => setExportTariffCfg((p) => ({ ...p, off_peak_end: e.target.value }))}
                className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
              />
            </label>
          </div>
        </div>

        <button
          onClick={handleTariffSave}
          disabled={saving}
          className="bg-flow-active text-bg-base font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 transition-opacity self-start"
        >
          {saving ? 'Saving…' : 'Save Tariffs'}
        </button>
      </section>

      {/* ─── Section 5: Developer Mode ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-3">
        <h2 className="text-text-primary text-lg font-semibold font-sans">Developer</h2>
        <div className="flex items-center justify-between">
          <div className="flex flex-col gap-0.5">
            <span className="text-text-primary text-sm font-sans">Developer Mode</span>
            <span className="text-text-secondary text-xs font-sans">
              Shows the Logs page for debugging
            </span>
          </div>
          <Toggle checked={developerMode} onChange={setDeveloperMode} />
        </div>
        {developerMode && (
          <DevTools />
        )}
      </section>

      {/* ─── Section 7: About ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-2">
        <h2 className="text-text-primary text-lg font-semibold font-sans">About</h2>
        <a
          href="https://github.com/psylsph/home-energy-manager"
          target="_blank"
          rel="noopener noreferrer"
          className="text-flow-active text-sm font-sans hover:underline mt-1"
        >
          github.com/psylsph/home-energy-manager
        </a>
      </section>
    </div>
  );
}
