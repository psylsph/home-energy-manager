import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  Area,
  AreaChart,
  CartesianGrid,
  Legend,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import { apiGet, apiPost } from '../lib/api';
import {
  cumulativeOctopusSeries,
  mergeOctopusSeries,
  octopusSeriesTotal,
} from '../lib/octopusSeries';
import type { OctopusPoint } from '../lib/octopusSeries';

type OctopusRange = '7d' | '30d' | '6m' | '1y' | 'all';

interface OctopusStatus {
  syncing: boolean;
  last_sync_at: string | null;
  last_error: string | null;
  backfill_complete: boolean;
  discovered_streams: number;
  imported_intervals: number;
}

interface StatusResponse {
  ok: boolean;
  configured: boolean;
  data: OctopusStatus;
  bounds: [number, number] | null;
  gas_unit_note: string;
}

interface HistoryResponse {
  ok: boolean;
  data: Record<string, OctopusPoint[]>;
}

const RANGES: { key: OctopusRange; label: string }[] = [
  { key: '7d', label: '7 days' },
  { key: '30d', label: '30 days' },
  { key: '6m', label: '6 months' },
  { key: '1y', label: '1 year' },
  { key: 'all', label: 'All' },
];

function formatTick(value: number, range: OctopusRange) {
  const date = new Date(value);
  if (range === '7d') {
    return date.toLocaleDateString([], { weekday: 'short', hour: '2-digit' });
  }
  if (range === '30d') {
    return date.toLocaleDateString([], { month: 'short', day: 'numeric' });
  }
  return date.toLocaleDateString([], { month: 'short', year: '2-digit' });
}

function ConsumptionTooltip({ active, payload, label }: {
  active?: boolean;
  payload?: Array<{ name: string; value: number; color: string }>;
  label?: number;
}) {
  if (!active || !payload?.length || label == null) return null;
  return (
    <div className="rounded-lg border border-white/10 bg-bg-elevated px-3 py-2 text-xs shadow-xl">
      <div className="mb-1 text-text-secondary">{new Date(label).toLocaleString()}</div>
      {payload.map((entry) => (
        <div key={entry.name} style={{ color: entry.color }}>
          {entry.name}: {Number(entry.value).toFixed(3)}
        </div>
      ))}
    </div>
  );
}

export default function OctopusPage() {
  const [range, setRange] = useState<OctopusRange>('30d');
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [series, setSeries] = useState<Record<string, OctopusPoint[]>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [nextStatus, history] = await Promise.all([
        apiGet<StatusResponse>('/api/octopus/status'),
        apiGet<HistoryResponse>(`/api/octopus/history?range=${range}`),
      ]);
      setStatus(nextStatus);
      setSeries(history.data ?? {});
      setError(null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : 'Unable to load Octopus data');
    } finally {
      setLoading(false);
    }
  }, [range]);

  useEffect(() => {
    let cancelled = false;
    const run = async () => {
      if (!cancelled) await load();
    };
    void run();
    const id = window.setInterval(() => void run(), 30_000);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [load]);

  const electricity = useMemo(
    () => mergeOctopusSeries(series, ['electricity_import', 'electricity_export']),
    [series],
  );
  const cumulativeElectricity = useMemo(
    () => cumulativeOctopusSeries(series, ['electricity_import', 'electricity_export']),
    [series],
  );
  const gas = useMemo(() => mergeOctopusSeries(series, ['gas']), [series]);
  const cumulativeGas = useMemo(() => cumulativeOctopusSeries(series, ['gas']), [series]);
  const importTotal = octopusSeriesTotal(series.electricity_import);
  const exportTotal = octopusSeriesTotal(series.electricity_export);
  const gasTotal = octopusSeriesTotal(series.gas);

  const syncNow = async () => {
    try {
      await apiPost('/api/octopus/sync');
      setStatus((current) => current ? {
        ...current,
        data: { ...current.data, syncing: true },
      } : current);
      setError(null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : 'Unable to start sync');
    }
  };

  if (loading) {
    return <div className="mx-auto max-w-5xl text-sm text-text-secondary">Loading Octopus data…</div>;
  }

  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-4">
      <section className="rounded-xl bg-bg-surface p-4">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <h2 className="text-lg font-semibold text-text-primary">Octopus Energy</h2>
            <p className="mt-1 text-sm text-text-secondary">
              Supplier smart-meter readings. These can arrive later than inverter data and may be corrected by Octopus.
            </p>
          </div>
          <button
            type="button"
            onClick={syncNow}
            disabled={status?.data.syncing}
            className="rounded-lg bg-flow-active px-3 py-2 text-sm font-medium text-bg-base disabled:opacity-50"
          >
            {status?.data.syncing ? 'Syncing…' : 'Sync now'}
          </button>
        </div>
        <div className="mt-3 flex flex-wrap gap-x-5 gap-y-1 text-xs text-text-secondary">
          <span>{status?.data.discovered_streams ?? 0} meter stream(s)</span>
          <span>
            {status?.data.last_sync_at
              ? `Last sync ${new Date(status.data.last_sync_at).toLocaleString()}`
              : 'Waiting for first sync'}
          </span>
          <span>{status?.data.backfill_complete ? 'Full history imported' : 'Older history backfilling'}</span>
        </div>
        {(error || status?.data.last_error) && (
          <div role="alert" className="mt-3 rounded-lg border border-red-500/30 bg-red-950/30 px-3 py-2 text-sm text-red-300">
            {error ?? status?.data.last_error}
          </div>
        )}
      </section>

      <div className="flex gap-1 overflow-x-auto rounded-xl bg-bg-surface p-2">
        {RANGES.map((item) => (
          <button
            key={item.key}
            type="button"
            aria-pressed={range === item.key}
            onClick={() => setRange(item.key)}
            className={`shrink-0 rounded-lg px-3 py-1.5 text-xs font-medium ${
              range === item.key
                ? 'bg-flow-active text-bg-base'
                : 'bg-bg-elevated text-text-secondary hover:text-text-primary'
            }`}
          >
            {item.label}
          </button>
        ))}
      </div>

      <section className="grid gap-3 sm:grid-cols-3" aria-label="Selected period totals">
        <div className="rounded-xl bg-bg-surface p-4">
          <div className="text-xs text-text-secondary">Electricity imported</div>
          <div className="mt-1 text-2xl font-semibold text-red-400">{importTotal.toFixed(3)} kWh</div>
        </div>
        <div className="rounded-xl bg-bg-surface p-4">
          <div className="text-xs text-text-secondary">Electricity exported</div>
          <div className="mt-1 text-2xl font-semibold text-green-400">{exportTotal.toFixed(3)} kWh</div>
        </div>
        <div className="rounded-xl bg-bg-surface p-4">
          <div className="text-xs text-text-secondary">Gas</div>
          <div className="mt-1 text-2xl font-semibold text-amber-400">{gasTotal.toFixed(3)}</div>
          <div className="text-xs text-text-secondary">Octopus-reported units</div>
        </div>
      </section>

      <section className="rounded-xl bg-bg-surface p-4">
        <h3 className="mb-3 font-medium text-text-primary">Electricity consumption</h3>
        {electricity.length === 0 ? (
          <div className="py-16 text-center text-sm text-text-secondary">No electricity readings imported yet.</div>
        ) : (
          <div className="h-72">
            <ResponsiveContainer width="100%" height="100%">
              <AreaChart data={electricity}>
                <CartesianGrid stroke="var(--color-grid-stroke-subtle)" strokeDasharray="3 4" />
                <XAxis dataKey="t" tickFormatter={(v) => formatTick(Number(v), range)} tick={{ fontSize: 11 }} />
                <YAxis tick={{ fontSize: 11 }} label={{ value: 'kWh', angle: -90, position: 'insideLeft' }} />
                <Tooltip content={<ConsumptionTooltip />} />
                <Legend />
                <Area type="monotone" dataKey="electricity_import" name="Import" stroke="#ef4444" fill="#ef4444" fillOpacity={0.18} connectNulls />
                <Area type="monotone" dataKey="electricity_export" name="Export" stroke="#22c55e" fill="#22c55e" fillOpacity={0.18} connectNulls />
              </AreaChart>
            </ResponsiveContainer>
          </div>
        )}
      </section>

      <section className="rounded-xl bg-bg-surface p-4">
        <h3 className="mb-3 font-medium text-text-primary">Cumulative electricity</h3>
        {cumulativeElectricity.length === 0 ? (
          <div className="py-16 text-center text-sm text-text-secondary">No electricity readings imported yet.</div>
        ) : (
          <div className="h-72">
            <ResponsiveContainer width="100%" height="100%">
              <AreaChart data={cumulativeElectricity}>
                <CartesianGrid stroke="var(--color-grid-stroke-subtle)" strokeDasharray="3 4" />
                <XAxis dataKey="t" tickFormatter={(v) => formatTick(Number(v), range)} tick={{ fontSize: 11 }} />
                <YAxis tick={{ fontSize: 11 }} label={{ value: 'Cumulative kWh', angle: -90, position: 'insideLeft' }} />
                <Tooltip content={<ConsumptionTooltip />} />
                <Legend />
                <Area type="monotone" dataKey="electricity_import" name="Cumulative import" stroke="#ef4444" fill="#ef4444" fillOpacity={0.12} connectNulls />
                <Area type="monotone" dataKey="electricity_export" name="Cumulative export" stroke="#22c55e" fill="#22c55e" fillOpacity={0.12} connectNulls />
              </AreaChart>
            </ResponsiveContainer>
          </div>
        )}
      </section>

      <section className="rounded-xl bg-bg-surface p-4">
        <h3 className="font-medium text-text-primary">Gas consumption</h3>
        <p className="mb-3 mt-1 text-xs text-text-secondary">
          {status?.gas_unit_note ?? 'Values are shown in the units reported by Octopus.'}
        </p>
        {gas.length === 0 ? (
          <div className="py-16 text-center text-sm text-text-secondary">No gas readings imported yet.</div>
        ) : (
          <div className="h-72">
            <ResponsiveContainer width="100%" height="100%">
              <AreaChart data={gas}>
                <CartesianGrid stroke="var(--color-grid-stroke-subtle)" strokeDasharray="3 4" />
                <XAxis dataKey="t" tickFormatter={(v) => formatTick(Number(v), range)} tick={{ fontSize: 11 }} />
                <YAxis tick={{ fontSize: 11 }} label={{ value: 'Reported units', angle: -90, position: 'insideLeft' }} />
                <Tooltip content={<ConsumptionTooltip />} />
                <Area type="monotone" dataKey="gas" name="Gas" stroke="#f59e0b" fill="#f59e0b" fillOpacity={0.2} connectNulls />
              </AreaChart>
            </ResponsiveContainer>
          </div>
        )}
      </section>

      <section className="rounded-xl bg-bg-surface p-4">
        <h3 className="font-medium text-text-primary">Cumulative gas</h3>
        <p className="mb-3 mt-1 text-xs text-text-secondary">Running total in the units reported by Octopus.</p>
        {cumulativeGas.length === 0 ? (
          <div className="py-16 text-center text-sm text-text-secondary">No gas readings imported yet.</div>
        ) : (
          <div className="h-72">
            <ResponsiveContainer width="100%" height="100%">
              <AreaChart data={cumulativeGas}>
                <CartesianGrid stroke="var(--color-grid-stroke-subtle)" strokeDasharray="3 4" />
                <XAxis dataKey="t" tickFormatter={(v) => formatTick(Number(v), range)} tick={{ fontSize: 11 }} />
                <YAxis tick={{ fontSize: 11 }} label={{ value: 'Cumulative units', angle: -90, position: 'insideLeft' }} />
                <Tooltip content={<ConsumptionTooltip />} />
                <Area type="monotone" dataKey="gas" name="Cumulative gas" stroke="#f59e0b" fill="#f59e0b" fillOpacity={0.14} connectNulls />
              </AreaChart>
            </ResponsiveContainer>
          </div>
        )}
      </section>
    </div>
  );
}
