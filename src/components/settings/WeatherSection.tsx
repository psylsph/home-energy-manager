import { useCallback, useEffect, useState } from 'react';
import { apiGet, apiPost } from '../../lib/api';
import { openExternal } from '../../lib/openExternal';
import Toggle from '../Toggle';
import { SettingsGroup, SettingsSection } from './SettingsSection';
import { TEXT_INPUT as INPUT } from './styles';
import { errorText, useSettingsSave, type Flash } from './useSettingsSave';

/** GET /api/weather: the config, the last fetch, and backfill progress. */
interface WeatherState {
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
}

const BACKFILL_POLL_MS = 3000;


const fetchWeather = () => apiGet<{ ok: boolean; data: WeatherState | null }>('/api/weather');

/**
 * Settings → Local Weather (Open-Meteo): outdoor temperature for the History
 * charts. The location saves by postcode (resolved by the backend) or by
 * manual coordinates; a backfill fetches past weather in the background.
 */
export default function WeatherSection({ id, flash }: { id?: string; flash: Flash }) {
  const [state, setState] = useState<WeatherState | null>(null);
  // The form fields, seeded from `state` on load and after each save.
  const [postcode, setPostcode] = useState('');
  const [lat, setLat] = useState('');
  const [lon, setLon] = useState('');
  const [backfilling, setBackfilling] = useState(false);
  const { busy: saving, run } = useSettingsSave(flash);

  const loadForm = useCallback((data: WeatherState) => {
    setState(data);
    setPostcode(data.config.postcode ?? '');
    setLat(data.config.latitude?.toString() ?? '');
    setLon(data.config.longitude?.toString() ?? '');
  }, []);

  useEffect(() => {
    let cancelled = false;
    fetchWeather()
      .then((res) => {
        if (!cancelled && res.ok && res.data) loadForm(res.data);
      })
      .catch((e: unknown) => console.warn('Failed to load weather config:', e));
    return () => { cancelled = true; };
  }, [loadForm]);

  // Follow a running backfill until `backfill_in_progress` clears.
  useEffect(() => {
    if (!backfilling) return;
    let cancelled = false;
    const poll = async () => {
      try {
        const res = await fetchWeather();
        if (cancelled || !res.ok || !res.data) return;
        setState(res.data);
        if (!res.data.backfill_in_progress) setBackfilling(false);
      } catch {
        // Retry on the next tick.
      }
    };
    void poll();
    const timer = setInterval(() => void poll(), BACKFILL_POLL_MS);
    return () => { cancelled = true; clearInterval(timer); };
  }, [backfilling]);

  // Sends the postcode plus any manual coordinates; without coordinates the
  // backend resolves the postcode. Re-fetches afterwards so the form shows
  // the resolved coordinates and canonical postcode.
  const handleSave = () =>
    run(async () => {
      const payload: Record<string, unknown> = {
        enabled: state?.config.enabled ?? false,
        postcode: postcode.trim(),
      };
      const latNum = Number(lat);
      const lonNum = Number(lon);
      if (lat.trim() !== '' && Number.isFinite(latNum)) payload.latitude = latNum;
      if (lon.trim() !== '' && Number.isFinite(lonNum)) payload.longitude = lonNum;
      const res = await apiPost('/api/weather', payload) as { ok: boolean; message: string };
      flash(res.message, res.ok);
      const refreshed = await fetchWeather();
      if (refreshed.ok && refreshed.data) loadForm(refreshed.data);
    }, 'Failed to save weather config');

  const handleBackfill = async () => {
    setBackfilling(true);
    try {
      await apiPost('/api/weather/backfill', {});
      flash('Backfill started — fetching historical weather in the background', true);
    } catch (e: unknown) {
      flash(errorText(e, 'Failed to start backfill'), false);
      setBackfilling(false);
    }
  };

  const enabled = state?.config.enabled ?? false;

  return (
    <SettingsSection
      id={id}
      title="Local Weather"
      action={
        <button
          onClick={() => openExternal('https://open-meteo.com/')}
          className="text-accent text-xs font-sans underline hover:opacity-80 transition-opacity"
        >
          Open-Meteo ↗
        </button>
      }
      description="Outdoor temperature on the History charts, from the free Open-Meteo API."
      help="Fetch the local outdoor temperature from the free Open-Meteo API and overlay it on the History page temperature charts. No API key required. Enter your postcode to resolve your location automatically, or enter latitude/longitude manually (useful outside the UK or for self-hosted Open-Meteo instances). The saved location also lets HEM suppress low phantom PV readings when the sun is down."
    >
      <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
        <div className="flex items-center justify-between">
          <span className="text-text-primary text-sm font-sans">Enable Weather</span>
          <Toggle
            checked={enabled}
            ariaLabel="Enable Weather"
            onChange={(v) => {
              // Saves at once, like the alerts switch.
              const setEnabled = (on: boolean) =>
                setState((p) => (p ? { ...p, config: { ...p.config, enabled: on } } : p));
              setEnabled(v);
              apiPost('/api/weather', { enabled: v })
                .then(() => flash(v ? 'Weather enabled' : 'Weather disabled', true))
                .catch((e) => {
                  setEnabled(!v);
                  flash(errorText(e, 'Failed to save'), false);
                });
            }}
          />
        </div>
      </div>

      {enabled && state && (
        <div className="flex flex-col gap-4">
          <SettingsGroup title="Location">
            <label className="flex flex-col gap-1">
              <span className="text-text-secondary text-xs font-sans">Postcode (UK — resolved via api.postcodes.io)</span>
              <input
                type="text"
                placeholder="e.g. SW1A 1AA"
                value={postcode}
                onChange={(e) => setPostcode(e.target.value)}
                className={INPUT}
              />
            </label>

            <div className="flex items-center gap-3 my-1">
              <div className="flex-1 h-px bg-white/5" />
              <span className="text-text-secondary/60 text-[10px] font-sans uppercase tracking-wide">Or Manual Coordinates</span>
              <div className="flex-1 h-px bg-white/5" />
            </div>

            <div className="grid grid-cols-2 gap-3">
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">Latitude</span>
                <input type="number" step="any" placeholder="51.501" value={lat} onChange={(e) => setLat(e.target.value)} className={INPUT} />
              </label>
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">Longitude</span>
                <input type="number" step="any" placeholder="-0.141" value={lon} onChange={(e) => setLon(e.target.value)} className={INPUT} />
              </label>
            </div>

            {(state.config.latitude != null || state.last_fetched_temperature_c != null) && (
              <div className="text-text-secondary/80 text-xs font-sans flex flex-col gap-1 mt-1">
                {state.config.latitude != null && state.config.longitude != null && (
                  <span>
                    Resolved grid cell:{' '}
                    <code className="font-mono">
                      {(state.grid_cell_latitude ?? state.config.latitude).toFixed(3)},
                      {' '}{(state.grid_cell_longitude ?? state.config.longitude).toFixed(3)}
                    </code>
                  </span>
                )}
                {state.last_fetched_temperature_c != null && state.last_fetch_at && (
                  <span>
                    Last reading: <strong className="text-text-primary">{state.last_fetched_temperature_c.toFixed(1)}°C</strong>{' '}
                    ({new Date(state.last_fetch_at).toLocaleString()})
                  </span>
                )}
              </div>
            )}
            {state.last_error && (
              <p className="text-red-400/80 text-xs font-sans mt-1">{state.last_error}</p>
            )}
          </SettingsGroup>

          <div className="flex flex-col sm:flex-row gap-2">
            <button
              onClick={() => void handleSave()}
              disabled={saving}
              className="bg-accent text-on-accent font-sans font-semibold text-sm px-4 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 transition-opacity sm:w-auto"
            >
              {saving ? 'Saving…' : 'Save Location'}
            </button>
            <button
              onClick={() => void handleBackfill()}
              disabled={backfilling || state.config.latitude == null}
              title={state.config.latitude == null ? 'Save a location first' : undefined}
              className="bg-bg-elevated text-text-primary font-sans font-semibold text-sm px-4 py-2 rounded-lg hover:opacity-80 disabled:opacity-40 transition-opacity border border-white/5 sm:w-auto"
            >
              {backfilling ? 'Backfilling…' : 'Backfill History'}
            </button>
          </div>

          {backfilling && (
            <p className="text-text-secondary/70 text-xs font-sans">
              Fetching historical weather one month at a time from the Open-Meteo archive. This runs in the background — you can leave this page.
            </p>
          )}
          {state.config.last_backfill_completed && !backfilling && (
            <p className="text-text-secondary/70 text-xs font-sans">
              History backfilled through{' '}
              <strong className="text-text-primary">{state.config.last_backfill_completed}</strong>.
            </p>
          )}
        </div>
      )}

      {/* CC BY 4.0 attribution — required by the Open-Meteo licence */}
      <p className="text-text-secondary/60 text-[11px] font-sans">
        Weather data by{' '}
        <button onClick={() => openExternal('https://open-meteo.com/')} className="text-accent underline hover:opacity-80 inline">
          Open-Meteo.com
        </button>
        {' '}— licensed under CC BY 4.0.
      </p>
    </SettingsSection>
  );
}
