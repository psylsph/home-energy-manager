import { useState, useEffect, useCallback } from 'react';
import { apiGet, apiPost, getApiBase, isTauri } from '../lib/api';
import type { PollSettings, DiscoveredInverter } from '../lib/types';
import { useInverterStore } from '../store/useInverterStore';

export default function SettingsPage() {
  const { connectionState, connectedHost } = useInverterStore();

  // Connection fields
  const [host, setHost] = useState('');
  const [port, setPort] = useState(8899);
  const [serial, setSerial] = useState('');

  // Discover
  const [discovering, setDiscovering] = useState(false);
  const [discoverResults, setDiscoverResults] = useState<DiscoveredInverter[]>([]);
  const [discoverError, setDiscoverError] = useState('');

  // Refresh interval
  const [intervalSecs, setIntervalSecs] = useState(10);

  // General
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<{ text: string; ok: boolean } | null>(null);
  const [settingsLoaded, setSettingsLoaded] = useState(false);

  const flash = useCallback((text: string, ok: boolean) => {
    setMessage({ text, ok });
    setTimeout(() => setMessage(null), 4000);
  }, []);

  // Load settings on mount
  useEffect(() => {
    (async () => {
      try {
        const s = await apiGet<PollSettings>('/api/settings');
        setHost(s.host ?? '');
        setPort(s.port ?? 8899);
        setSerial(s.serial ?? '');
        setIntervalSecs(s.interval_secs ?? 10);
        setSettingsLoaded(true);
      } catch {
        setSettingsLoaded(true);
      }
    })();
  }, []);

  // Network URL
  const lanUrl = getApiBase().replace(/^http/, 'http');

  // Save connection
  const handleConnect = async () => {
    setSaving(true);
    try {
      await apiPost('/api/settings', { host, port, serial });
      flash('Settings saved — reconnecting…', true);
    } catch {
      flash('Failed to save settings', false);
    }
    setSaving(false);
  };

  // Save interval
  const handleIntervalChange = async (val: number) => {
    setIntervalSecs(val);
    try {
      await apiPost('/api/settings', { interval_secs: val });
      flash(`Refresh interval set to ${val}s`, true);
    } catch {
      flash('Failed to update interval', false);
    }
  };

  // Discover
  const handleDiscover = async () => {
    setDiscovering(true);
    setDiscoverError('');
    setDiscoverResults([]);
    try {
      const results = await apiGet<DiscoveredInverter[]>('/api/discover');
      setDiscoverResults(results);
      if (results.length === 0) setDiscoverError('No inverters found on the network.');
    } catch {
      setDiscoverError('Discovery failed — is the backend running?');
    }
    setDiscovering(false);
  };

  const useDiscovered = (inv: DiscoveredInverter) => {
    setHost(inv.host);
    setPort(inv.port);
    if (inv.serial) setSerial(inv.serial);
  };

  const copyUrl = () => {
    navigator.clipboard.writeText(lanUrl).then(() => flash('URL copied!', true));
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
          <span className="text-text-secondary text-xs font-sans">Inverter IP / Host</span>
          <input
            type="text"
            value={host}
            onChange={(e) => setHost(e.target.value)}
            placeholder="192.168.x.x"
            className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
          />
        </label>

        <label className="flex flex-col gap-1">
          <span className="text-text-secondary text-xs font-sans">Port</span>
          <input
            type="number"
            value={port}
            onChange={(e) => setPort(Number(e.target.value))}
            className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors w-32"
          />
        </label>

        <label className="flex flex-col gap-1">
          <span className="text-text-secondary text-xs font-sans">Serial Number</span>
          <input
            type="text"
            value={serial}
            onChange={(e) => setSerial(e.target.value)}
            placeholder="SA…"
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
                  onClick={() => useDiscovered(inv)}
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
          <code className="bg-bg-elevated text-flow-active rounded-lg px-4 py-2 text-sm font-mono flex-1 select-all">
            {lanUrl}
          </code>
          <button
            onClick={copyUrl}
            className="bg-bg-elevated text-text-primary font-sans text-sm px-4 py-2 rounded-lg hover:bg-bg-base transition-colors"
          >
            Copy
          </button>
        </div>

        <p className="text-text-secondary text-xs font-sans">
          Access this dashboard from any device on your network
        </p>

        <div className="bg-bg-elevated rounded-lg px-4 py-6 flex items-center justify-center">
          <span className="text-text-secondary text-sm font-sans">QR code coming soon</span>
        </div>
      </section>

      {/* ─── Section 3: Refresh Interval ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-3">
        <h2 className="text-text-primary text-lg font-semibold font-sans">Refresh Interval</h2>

        <div className="flex items-center gap-4">
          <input
            type="range"
            min={5}
            max={60}
            step={1}
            value={intervalSecs}
            onChange={(e) => handleIntervalChange(Number(e.target.value))}
            className="flex-1 accent-flow-active h-2 rounded-full appearance-none bg-bg-elevated [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:w-4 [&::-webkit-slider-thumb]:h-4 [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-flow-active [&::-webkit-slider-thumb]:cursor-pointer"
          />
          <span className="text-text-primary text-sm font-mono w-12 text-right">{intervalSecs}s</span>
        </div>

        <div className="flex justify-between text-text-secondary text-xs font-sans">
          <span>5s</span>
          <span>60s</span>
        </div>
      </section>

      {/* ─── Section 4: Desktop-only ─── */}
      {isTauri && (
        <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-4">
          <h2 className="text-text-primary text-lg font-semibold font-sans">Desktop Settings</h2>

          <label className="flex items-center justify-between cursor-pointer">
            <span className="text-text-primary text-sm font-sans">Auto-start on login</span>
            <div className="relative">
              <input type="checkbox" className="sr-only peer" />
              <div className="w-10 h-5 bg-bg-elevated rounded-full peer-checked:bg-flow-active/40 transition-colors" />
              <div className="absolute left-0.5 top-0.5 w-4 h-4 bg-text-secondary rounded-full peer-checked:translate-x-5 peer-checked:bg-flow-active transition-all" />
            </div>
          </label>

          <label className="flex items-center justify-between cursor-pointer">
            <span className="text-text-primary text-sm font-sans">Minimise to system tray</span>
            <div className="relative">
              <input type="checkbox" className="sr-only peer" />
              <div className="w-10 h-5 bg-bg-elevated rounded-full peer-checked:bg-flow-active/40 transition-colors" />
              <div className="absolute left-0.5 top-0.5 w-4 h-4 bg-text-secondary rounded-full peer-checked:translate-x-5 peer-checked:bg-flow-active transition-all" />
            </div>
          </label>

          <p className="text-text-secondary text-xs font-sans">
            These settings only appear in the desktop app
          </p>
        </section>
      )}

      {/* ─── Section 5: About ─── */}
      <section className="bg-bg-surface rounded-xl p-5 flex flex-col gap-2">
        <h2 className="text-text-primary text-lg font-semibold font-sans">About</h2>

        <div className="flex items-center gap-2 text-sm font-sans">
          <span className="text-text-secondary">Version</span>
          <span className="text-text-primary font-mono">0.1.0</span>
        </div>

        <p className="text-text-secondary text-xs font-sans">
          Built with Tauri + React
        </p>

        <a
          href="https://github.com/psylsph/givenergy-local"
          target="_blank"
          rel="noopener noreferrer"
          className="text-flow-active text-sm font-sans hover:underline mt-1"
        >
          github.com/psylsph/givenergy-local
        </a>
      </section>
    </div>
  );
}
