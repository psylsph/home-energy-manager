import { useState, useEffect, useCallback } from 'react';
import { apiGet, apiPost, getApiBase, getServerPort } from '../lib/api';
import { openExternal } from '../lib/openExternal';
import type { PollSettings, DiscoveredInverter, DiscoveredEvc, TariffConfig } from '../lib/types';
import { useInverterStore } from '../store/useInverterStore';

function Toggle({ checked, onChange }: { checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <div className="relative cursor-pointer" onClick={() => onChange(!checked)}>
      <div className={`w-10 h-5 rounded-full transition-colors ${checked ? 'bg-flow-active/40' : 'bg-bg-elevated'}`} />
      <div className={`absolute left-0.5 top-0.5 w-4 h-4 rounded-full transition-all ${checked ? 'translate-x-5 bg-flow-active' : 'bg-text-secondary'}`} />
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
  }, []);
  if (!dataUrl) return <div className="w-[120px] h-[120px] bg-bg-elevated rounded-lg animate-pulse" />;
  return <img src={dataUrl} alt={alt} className="w-[120px] h-[120px] rounded-lg" />;
}

function WhatsAppPairing() {
  const [state, setState] = useState<'idle' | 'waiting' | 'paired' | 'error'>('idle');
  const [qrData, setQrData] = useState('');
  const [qrSvg, setQrSvg] = useState('');

  useEffect(() => {
    let cancelled = false;
    const poll = async () => {
      try {
        const res = await fetch(`${getApiBase()}/api/whatsapp/status`);
        const data = await res.json();
        if (cancelled) return;
        setState(data.state);
        setQrData(data.qr || '');
      } catch { /* ignore */ }
    };
    poll();
    const interval = setInterval(poll, 3000);
    return () => { cancelled = true; clearInterval(interval); };
  }, []);

  // Render QR code SVG when we have the pairing data
  useEffect(() => {
    if (!qrData || state !== 'waiting') { return; }
    let cancelled = false;
    import('qrcode').then((qr) => {
      qr.toString(qrData, { type: 'svg', width: 200, margin: 1, color: { dark: '#000000', light: '#ffffff' } })
        .then((svg) => { if (!cancelled) setQrSvg('data:image/svg+xml;utf8,' + encodeURIComponent(svg)); });
    }).catch(() => {});
    return () => { cancelled = true; };
  }, [qrData, state]);

  if (state === 'paired') {
    return <span className="text-flow-active text-sm font-sans">✅ WhatsApp connected</span>;
  }
  if (state === 'error') {
    return <span className="text-red-400 text-xs font-sans">Connection error — will retry automatically</span>;
  }
  if (state === 'waiting' && qrSvg) {
    return (
      <div className="flex flex-col items-center gap-2">
        <img src={qrSvg} alt="WhatsApp QR" className="w-[200px] h-[200px] rounded-lg bg-white p-1" />
        <span className="text-text-secondary text-xs font-sans">Scan with WhatsApp → Linked Devices</span>
      </div>
    );
  }
  return <span className="text-text-secondary text-xs font-sans animate-pulse">Waiting for QR code...</span>;
}

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
  } = useInverterStore();

  // Connection fields
  const [host, setHost] = useState('');
  const [port, setPort] = useState(8899);
  const [serial, setSerial] = useState('');

/// Snap a poll interval to the nearest valid value (5, 10, 15, or 20).
const VALID_INTERVALS = [5, 10, 15, 20];

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
    solar_clipping_enabled: false, pv_string_loss_enabled: false,
    grid_offline_enabled: false, battery_over_temp_enabled: false,
    whatsapp_recipient: '',
    daily_report_enabled: false, daily_report_hour: 8, daily_report_minute: 0,
  });
  const [alertsSaving, setAlertsSaving] = useState(false);
  const [alertsTesting, setAlertsTesting] = useState(false);

  const flash = useCallback((text: string, ok: boolean) => {
    setMessage({ text, ok });
    setTimeout(() => setMessage(null), 4000);
  }, []);

  // Load settings on mount
  useEffect(() => {
    const clampInterval = (v: number) => [5, 10, 15, 20].reduce((a, b) =>
      Math.abs(b - v) < Math.abs(a - v) ? b : a
    );
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
        if (s.hidden_panels) {
          setHiddenPanels(s.hidden_panels);
        }
        setEvcHost(s.evc_host ?? '');
        setEvcPort(s.evc_port ?? 502);
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
    try {
      await apiPost('/api/settings', { evc_host: evcHost, evc_port: evcPort });
      useInverterStore.getState().setEvcHost(evcHost);
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
    try {
      const res = await apiPost('/api/alerts', alertsConfig) as { message: string; ok: boolean };
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
      </section>

      {/* ─── Section 2: Remote / Mobile Network Access ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-3">
        <h2 className="text-text-primary text-lg font-semibold font-sans">Remote / Mobile Network Access</h2>

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

      {/* ─── Section 2b: EV Charger ─── */}
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
              placeholder="Leave blank to disable"
              className="min-w-0 flex-1 bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
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
        </label>

        <div className="flex gap-3 pt-1">
          <button
            onClick={handleEvcSave}
            className="bg-flow-active text-bg-base font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 transition-opacity"
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


      {/* ─── Section 4: Refresh Interval ─── */}
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

      {/* ─── Section 3.5: Panel Controls ─── */}
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

      {/* ─── Section 4.5: Notifications ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-4">
        <h2 className="text-text-primary text-lg font-semibold font-sans">Notifications</h2>
        <p className="text-text-secondary text-xs font-sans">
          Send Telegram alerts when critical conditions are detected, plus a daily consumption report. Create a bot via{' '}
          <button onClick={() => openExternal('https://t.me/botfather')} className="text-flow-active underline hover:opacity-80 inline">@BotFather</button> on Telegram, get your bot token, then send /start to your bot and get your chat ID from{' '}
          <button onClick={() => openExternal('https://t.me/userinfobot')} className="text-flow-active underline hover:opacity-80 inline">@userinfobot</button>.
        </p>

        <div className="flex items-center justify-between">
          <div className="flex flex-col gap-0.5">
            <span className="text-text-primary text-sm font-sans">Enable Alerts</span>
          </div>
          <Toggle
            checked={alertsConfig.enabled}
            onChange={(v) => setAlertsConfig((p) => ({ ...p, enabled: v }))}
          />
        </div>

        {alertsConfig.enabled && (
          <div className="flex flex-col gap-4">
            {/* Credentials */}
            <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
              <h3 className="text-text-primary text-sm font-sans font-medium">Telegram Configuration</h3>
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
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">Cooldown (minutes between alerts)</span>
                <input
                  type="number" min={1} max={1440}
                  value={alertsConfig.cooldown_minutes}
                  onChange={(e) => setAlertsConfig((p) => ({ ...p, cooldown_minutes: Number(e.target.value) }))}
                  className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-28 border border-bg-elevated focus:border-flow-active outline-none transition-colors"
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

            {/* WhatsApp */}
            <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
              <h3 className="text-text-primary text-sm font-sans font-medium">WhatsApp</h3>
              <p className="text-text-secondary text-xs font-sans">
                Pair your WhatsApp account to send alerts. On your phone, open WhatsApp Settings, tap "Linked Devices", then "Link a Device" and scan the QR code below. Then enter the phone number that should <strong>receive</strong> the alerts — this must be a different number from the linked account (you cannot message yourself).
              </p>
              <div className="flex flex-col gap-2">
                <label className="flex flex-col gap-1">
                  <span className="text-text-secondary text-xs font-sans">Recipient phone (digits only, intl format, e.g. 447700900123)</span>
                  <input
                    type="tel"
                    value={alertsConfig.whatsapp_recipient}
                    onChange={(e) => setAlertsConfig((p) => ({ ...p, whatsapp_recipient: e.target.value.replace(/\D/g, '') }))}
                    placeholder="447700900123"
                    className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                  />
                </label>
              </div>
              <div id="whatsapp-pairing" className="flex items-center justify-center py-4">
                <WhatsAppPairing />
              </div>
            </div>

            {/* Battery temperature & SOC thresholds */}
            <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
              <h3 className="text-text-primary text-sm font-sans font-medium">Battery Temperature &amp; SOC</h3>
              <div className="grid grid-cols-2 gap-4">
                {/* Temperature pair */}
                <div className="border border-white/5 rounded-lg p-3 flex flex-col gap-2">
                  <span className="text-text-secondary text-xs font-sans font-medium">Temperature (°C)</span>
                  <div className="flex items-center gap-2">
                    <label className="flex flex-col gap-1">
                      <span className="text-text-secondary text-xs font-sans">below (0 = off)</span>
                      <input
                        type="number" step="0.5" min="0" max="50"
                        value={alertsConfig.batt_temp_min}
                        onChange={(e) => setAlertsConfig((p) => ({ ...p, batt_temp_min: Number(e.target.value) }))}
                        className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-24 border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                      />
                    </label>
                    <label className="flex flex-col gap-1">
                      <span className="text-text-secondary text-xs font-sans">above (0 = off)</span>
                      <input
                        type="number" step="0.5" min="0" max="80"
                        value={alertsConfig.batt_temp_max}
                        onChange={(e) => setAlertsConfig((p) => ({ ...p, batt_temp_max: Number(e.target.value) }))}
                        className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-24 border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                      />
                    </label>
                  </div>
                </div>
                {/* SOC pair */}
                <div className="border border-white/5 rounded-lg p-3 flex flex-col gap-2">
                  <span className="text-text-secondary text-xs font-sans font-medium">SOC (%)</span>
                  <div className="flex items-center gap-2">
                    <label className="flex flex-col gap-1">
                      <span className="text-text-secondary text-xs font-sans">below</span>
                      <input
                        type="number" min="0" max="100"
                        value={alertsConfig.soc_min}
                        onChange={(e) => setAlertsConfig((p) => ({ ...p, soc_min: Number(e.target.value) }))}
                        className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-24 border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                      />
                    </label>
                    <label className="flex flex-col gap-1">
                      <span className="text-text-secondary text-xs font-sans">above (100 = off)</span>
                      <input
                        type="number" min="0" max="100"
                        value={alertsConfig.soc_max}
                        onChange={(e) => setAlertsConfig((p) => ({ ...p, soc_max: Number(e.target.value) }))}
                        className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-24 border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                      />
                    </label>
                  </div>
                </div>
              </div>
            </div>

            {/* Toggle alerts */}
            <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
              <h3 className="text-text-primary text-sm font-sans font-medium">Other Alerts</h3>
              <div className="flex items-center justify-between">
                <span className="text-text-primary text-sm font-sans">Solar Clipping</span>
                <Toggle
                  checked={alertsConfig.solar_clipping_enabled}
                  onChange={(v) => setAlertsConfig((p) => ({ ...p, solar_clipping_enabled: v }))}
                />
              </div>
              <div className="flex items-center justify-between">
                <span className="text-text-primary text-sm font-sans">PV String Loss</span>
                <Toggle
                  checked={alertsConfig.pv_string_loss_enabled}
                  onChange={(v) => setAlertsConfig((p) => ({ ...p, pv_string_loss_enabled: v }))}
                />
              </div>
              <div className="flex items-center justify-between">
                <span className="text-text-primary text-sm font-sans">Grid Offline</span>
                <Toggle
                  checked={alertsConfig.grid_offline_enabled}
                  onChange={(v) => setAlertsConfig((p) => ({ ...p, grid_offline_enabled: v }))}
                />
              </div>
              <div className="flex items-center justify-between">
                <span className="text-text-primary text-sm font-sans">Battery Over-Temperature</span>
                <Toggle
                  checked={alertsConfig.battery_over_temp_enabled}
                  onChange={(v) => setAlertsConfig((p) => ({ ...p, battery_over_temp_enabled: v }))}
                />
              </div>
            </div>

            {/* Daily report */}
            <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
              <h3 className="text-text-primary text-sm font-sans font-medium">Daily Consumption Report</h3>
              <div className="flex items-center justify-between">
                <span className="text-text-primary text-sm font-sans">Send Daily Report</span>
                <Toggle
                  checked={alertsConfig.daily_report_enabled}
                  onChange={(v) => setAlertsConfig((p) => ({ ...p, daily_report_enabled: v }))}
                />
              </div>
              {alertsConfig.daily_report_enabled && (
                <div className="grid grid-cols-2 gap-3">
                  <label className="flex flex-col gap-1">
                    <span className="text-text-secondary text-xs font-sans">Send at (hour 0-23)</span>
                    <input
                      type="number" min="0" max="23"
                      value={alertsConfig.daily_report_hour}
                      onChange={(e) => setAlertsConfig((p) => ({ ...p, daily_report_hour: Number(e.target.value) }))}
                      className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-20 border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                    />
                  </label>
                  <label className="flex flex-col gap-1">
                    <span className="text-text-secondary text-xs font-sans">Minute</span>
                    <input
                      type="number" min="0" max="59"
                      value={alertsConfig.daily_report_minute}
                      onChange={(e) => setAlertsConfig((p) => ({ ...p, daily_report_minute: Number(e.target.value) }))}
                      className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-20 border border-bg-elevated focus:border-flow-active outline-none transition-colors"
                    />
                  </label>
                </div>
              )}
            </div>

            <div className="flex items-center gap-3">
              <button
                onClick={handleAlertsSave}
                disabled={alertsSaving}
                className="bg-flow-active text-bg-base font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 transition-opacity"
              >
                {alertsSaving ? 'Saving…' : 'Save Notification Settings'}
              </button>
              <button
                onClick={handleAlertsTest}
                disabled={alertsTesting || !alertsConfig.telegram_bot_token || !alertsConfig.telegram_chat_id}
                className="bg-bg-elevated text-text-primary font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-80 disabled:opacity-40 transition-opacity border border-white/5"
              >
                {alertsTesting ? 'Sending…' : 'Send Test Notification'}
              </button>
            </div>
          </div>
        )}
      </section>

      {/* ─── Section 5: Developer Mode ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-3">
        <h2 className="text-text-primary text-lg font-semibold font-sans">Developer</h2>
        <div className="flex items-center justify-between">
          <div className="flex flex-col gap-0.5">
            <span className="text-text-primary text-sm font-sans">Developer Mode</span>
          </div>
          <Toggle checked={developerMode} onChange={setDeveloperMode} />
        </div>
        {developerMode}
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
