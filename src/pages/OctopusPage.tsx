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
  tariff_prices?: number;
  last_tariff_error?: string | null;
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

interface BillingSummary {
  electricity_import_kwh: number;
  electricity_export_kwh: number;
  gas_usage: number;
  electricity_energy_cost_gbp: number;
  electricity_standing_cost_gbp: number;
  electricity_total_cost_gbp: number;
  export_income_gbp: number;
  gas_energy_cost_gbp: number | null;
  gas_standing_cost_gbp: number;
  gas_total_cost_gbp: number | null;
  net_cost_gbp: number | null;
  pricing_complete: boolean;
}

interface BillingPeriod extends BillingSummary { period: string }

interface SummaryResponse {
  ok: boolean;
  data: {
    totals: BillingSummary;
    monthly: BillingPeriod[];
    yearly: BillingPeriod[];
    gas_cost_available: boolean;
  };
  gas_unit: 'unknown' | 'kwh' | 'm3';
  estimated: boolean;
}

interface ComparisonDay {
  date: string;
  octopus_import_kwh: number | null;
  hem_import_kwh: number | null;
  import_difference_kwh: number | null;
  import_difference_percent: number | null;
  octopus_export_kwh: number | null;
  hem_export_kwh: number | null;
  export_difference_kwh: number | null;
  export_difference_percent: number | null;
  expected_import_intervals: number;
  import_intervals: number;
  missing_import_intervals: number;
  expected_export_intervals: number;
  export_intervals: number;
  missing_export_intervals: number;
  expected_gas_intervals: number;
  gas_intervals: number;
  missing_gas_intervals: number;
}

interface ComparisonResponse {
  ok: boolean;
  data: {
    totals: {
      octopus_import_kwh: number;
      hem_import_kwh: number;
      import_difference_kwh: number;
      octopus_export_kwh: number;
      hem_export_kwh: number;
      export_difference_kwh: number;
      expected_import_intervals: number;
      import_intervals: number;
      missing_import_intervals: number;
      expected_export_intervals: number;
      export_intervals: number;
      missing_export_intervals: number;
      expected_gas_intervals: number;
      gas_intervals: number;
      missing_gas_intervals: number;
    };
    days: ComparisonDay[];
    import_stream_available: boolean;
    export_stream_available: boolean;
    gas_stream_available: boolean;
  };
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

function formatComparison(value: number | null, suffix = ' kWh'): string {
  return value == null ? '—' : `${value.toFixed(3)}${suffix}`;
}

function formatMoney(value: number | null): string {
  return value == null ? 'Unavailable' : value.toLocaleString([], {
    style: 'currency',
    currency: 'GBP',
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  });
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
  const [billing, setBilling] = useState<SummaryResponse | null>(null);
  const [comparison, setComparison] = useState<ComparisonResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [nextStatus, history, summary, nextComparison] = await Promise.all([
        apiGet<StatusResponse>('/api/octopus/status'),
        apiGet<HistoryResponse>(`/api/octopus/history?range=${range}`),
        apiGet<SummaryResponse>(`/api/octopus/summary?range=${range}`),
        apiGet<ComparisonResponse>(`/api/octopus/comparison?range=${range}`),
      ]);
      setStatus(nextStatus);
      setSeries(history.data ?? {});
      setBilling(summary);
      setComparison(nextComparison);
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

      {billing && (
        <section className="rounded-xl bg-bg-surface p-4">
          <div>
            <h3 className="font-medium text-text-primary">Estimated supplier costs</h3>
            <p className="mt-1 text-xs text-text-secondary">
              VAT-inclusive historical Octopus rates and standing charges matched to each supplier reading. These are estimates rather than an Octopus bill.
            </p>
          </div>
          <div className="mt-4 grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
            <div className="rounded-lg bg-bg-elevated p-3">
              <div className="text-xs text-text-secondary">Electricity import</div>
              <div className="mt-1 text-xl font-semibold text-red-400">{formatMoney(billing.data.totals.electricity_total_cost_gbp)}</div>
              <div className="text-xs text-text-secondary">
                {formatMoney(billing.data.totals.electricity_energy_cost_gbp)} energy + {formatMoney(billing.data.totals.electricity_standing_cost_gbp)} standing
              </div>
            </div>
            <div className="rounded-lg bg-bg-elevated p-3">
              <div className="text-xs text-text-secondary">Electricity export income</div>
              <div className="mt-1 text-xl font-semibold text-green-400">{formatMoney(billing.data.totals.export_income_gbp)}</div>
            </div>
            <div className="rounded-lg bg-bg-elevated p-3">
              <div className="text-xs text-text-secondary">Gas cost</div>
              <div className="mt-1 text-xl font-semibold text-amber-400">{formatMoney(billing.data.totals.gas_total_cost_gbp)}</div>
              {!billing.data.gas_cost_available && (
                <div className="text-xs text-text-secondary">Choose kWh in Settings to calculate</div>
              )}
            </div>
            <div className="rounded-lg bg-bg-elevated p-3">
              <div className="text-xs text-text-secondary">
                {billing.data.gas_cost_available ? 'Net supplier cost' : 'Net electricity cost'}
              </div>
              <div className="mt-1 text-xl font-semibold text-text-primary">
                {formatMoney(
                  billing.data.totals.net_cost_gbp
                  ?? billing.data.totals.electricity_total_cost_gbp - billing.data.totals.export_income_gbp,
                )}
              </div>
            </div>
          </div>
          {(!billing.data.totals.pricing_complete || status?.data.last_tariff_error) && (
            <div className="mt-3 rounded-lg border border-yellow-500/30 bg-yellow-950/20 px-3 py-2 text-xs text-yellow-300">
              Some historical tariff prices could not be matched, so the estimate may be incomplete.
              {status?.data.last_tariff_error ? ` ${status.data.last_tariff_error}` : ''}
            </div>
          )}

          {billing.data.monthly.length > 0 && (
            <div className="mt-5 overflow-x-auto">
              <h4 className="mb-2 text-sm font-medium text-text-primary">Monthly summary</h4>
              <table className="w-full min-w-[1080px] text-left text-xs">
                <thead className="text-text-secondary">
                  <tr className="border-b border-white/10">
                    <th className="px-2 py-2">Month</th>
                    <th className="px-2 py-2">Import</th>
                    <th className="px-2 py-2">Energy cost</th>
                    <th className="px-2 py-2">Elec. standing</th>
                    <th className="px-2 py-2">Import total</th>
                    <th className="px-2 py-2">Export</th>
                    <th className="px-2 py-2">Income</th>
                    <th className="px-2 py-2">Gas</th>
                    <th className="px-2 py-2">Gas energy</th>
                    <th className="px-2 py-2">Gas standing</th>
                    <th className="px-2 py-2">Gas total</th>
                    <th className="px-2 py-2">Net</th>
                  </tr>
                </thead>
                <tbody>
                  {[...billing.data.monthly].reverse().map((row) => (
                    <tr key={row.period} className="border-b border-white/5 text-text-primary">
                      <td className="px-2 py-2 font-medium">{row.period}</td>
                      <td className="px-2 py-2">{row.electricity_import_kwh.toFixed(3)} kWh</td>
                      <td className="px-2 py-2">{formatMoney(row.electricity_energy_cost_gbp)}</td>
                      <td className="px-2 py-2">{formatMoney(row.electricity_standing_cost_gbp)}</td>
                      <td className="px-2 py-2">{formatMoney(row.electricity_total_cost_gbp)}</td>
                      <td className="px-2 py-2">{row.electricity_export_kwh.toFixed(3)} kWh</td>
                      <td className="px-2 py-2">{formatMoney(row.export_income_gbp)}</td>
                      <td className="px-2 py-2">{row.gas_usage.toFixed(3)}</td>
                      <td className="px-2 py-2">{formatMoney(row.gas_energy_cost_gbp)}</td>
                      <td className="px-2 py-2">{formatMoney(row.gas_standing_cost_gbp)}</td>
                      <td className="px-2 py-2">{formatMoney(row.gas_total_cost_gbp)}</td>
                      <td className="px-2 py-2">{formatMoney(row.net_cost_gbp ?? row.electricity_total_cost_gbp - row.export_income_gbp)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}

          {billing.data.yearly.length > 0 && (
            <div className="mt-5 overflow-x-auto">
              <h4 className="mb-2 text-sm font-medium text-text-primary">Yearly summary</h4>
              <table className="w-full min-w-[620px] text-left text-xs">
                <thead className="text-text-secondary">
                  <tr className="border-b border-white/10">
                    <th className="px-2 py-2">Year</th>
                    <th className="px-2 py-2">Imported</th>
                    <th className="px-2 py-2">Import cost</th>
                    <th className="px-2 py-2">Exported</th>
                    <th className="px-2 py-2">Export income</th>
                    <th className="px-2 py-2">Gas</th>
                    <th className="px-2 py-2">Net</th>
                  </tr>
                </thead>
                <tbody>
                  {[...billing.data.yearly].reverse().map((row) => (
                    <tr key={row.period} className="border-b border-white/5 text-text-primary">
                      <td className="px-2 py-2 font-medium">{row.period}</td>
                      <td className="px-2 py-2">{row.electricity_import_kwh.toFixed(3)} kWh</td>
                      <td className="px-2 py-2">{formatMoney(row.electricity_total_cost_gbp)}</td>
                      <td className="px-2 py-2">{row.electricity_export_kwh.toFixed(3)} kWh</td>
                      <td className="px-2 py-2">{formatMoney(row.export_income_gbp)}</td>
                      <td className="px-2 py-2">{row.gas_usage.toFixed(3)}</td>
                      <td className="px-2 py-2">{formatMoney(row.net_cost_gbp ?? row.electricity_total_cost_gbp - row.export_income_gbp)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </section>
      )}

      {comparison && (
        <section className="rounded-xl bg-bg-surface p-4">
          <h3 className="font-medium text-text-primary">Octopus versus HEM</h3>
          <p className="mt-1 text-xs text-text-secondary">
            Daily supplier totals compared with the inverter’s daily counters. Difference is HEM minus Octopus; only days containing both readings contribute to the totals.
          </p>
          <div className="mt-4 grid gap-3 sm:grid-cols-2">
            <div className="rounded-lg bg-bg-elevated p-3">
              <div className="text-xs text-text-secondary">Import difference</div>
              <div className="mt-1 text-xl font-semibold text-text-primary">
                {formatComparison(comparison.data.totals.import_difference_kwh)}
              </div>
              <div className="text-xs text-text-secondary">
                Octopus {comparison.data.totals.octopus_import_kwh.toFixed(3)} · HEM {comparison.data.totals.hem_import_kwh.toFixed(3)} kWh
              </div>
            </div>
            <div className="rounded-lg bg-bg-elevated p-3">
              <div className="text-xs text-text-secondary">Export difference</div>
              <div className="mt-1 text-xl font-semibold text-text-primary">
                {formatComparison(comparison.data.totals.export_difference_kwh)}
              </div>
              <div className="text-xs text-text-secondary">
                Octopus {comparison.data.totals.octopus_export_kwh.toFixed(3)} · HEM {comparison.data.totals.hem_export_kwh.toFixed(3)} kWh
              </div>
            </div>
          </div>
          <div className="mt-4 max-h-96 overflow-auto">
            <table className="w-full min-w-[940px] text-left text-xs">
              <thead className="sticky top-0 bg-bg-surface text-text-secondary">
                <tr className="border-b border-white/10">
                  <th className="px-2 py-2">Date</th>
                  <th className="px-2 py-2">Octopus import</th>
                  <th className="px-2 py-2">HEM import</th>
                  <th className="px-2 py-2">Difference</th>
                  <th className="px-2 py-2">Octopus export</th>
                  <th className="px-2 py-2">HEM export</th>
                  <th className="px-2 py-2">Difference</th>
                </tr>
              </thead>
              <tbody>
                {[...comparison.data.days].reverse().map((day) => (
                  <tr key={day.date} className="border-b border-white/5 text-text-primary">
                    <td className="px-2 py-2 font-medium">{day.date}</td>
                    <td className="px-2 py-2">{formatComparison(day.octopus_import_kwh)}</td>
                    <td className="px-2 py-2">{formatComparison(day.hem_import_kwh)}</td>
                    <td className="px-2 py-2">
                      {formatComparison(day.import_difference_kwh)}
                      {day.import_difference_percent != null ? ` (${day.import_difference_percent.toFixed(1)}%)` : ''}
                    </td>
                    <td className="px-2 py-2">{formatComparison(day.octopus_export_kwh)}</td>
                    <td className="px-2 py-2">{formatComparison(day.hem_export_kwh)}</td>
                    <td className="px-2 py-2">
                      {formatComparison(day.export_difference_kwh)}
                      {day.export_difference_percent != null ? ` (${day.export_difference_percent.toFixed(1)}%)` : ''}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      )}

      {comparison && (
        <section className="rounded-xl bg-bg-surface p-4">
          <h3 className="font-medium text-text-primary">Supplier data completeness</h3>
          <p className="mt-1 text-xs text-text-secondary">
            Expected half-hour slots are adjusted for the selected partial day, imported history, and UK daylight-saving transitions. Recent Octopus readings commonly arrive late.
          </p>
          <div className="mt-4 grid gap-3 sm:grid-cols-3">
            {([
              ['Electricity import', comparison.data.import_stream_available, comparison.data.totals.import_intervals, comparison.data.totals.expected_import_intervals, comparison.data.totals.missing_import_intervals],
              ['Electricity export', comparison.data.export_stream_available, comparison.data.totals.export_intervals, comparison.data.totals.expected_export_intervals, comparison.data.totals.missing_export_intervals],
              ['Gas', comparison.data.gas_stream_available, comparison.data.totals.gas_intervals, comparison.data.totals.expected_gas_intervals, comparison.data.totals.missing_gas_intervals],
            ] as const).map(([label, available, actual, expected, missing]) => (
              <div key={label} className="rounded-lg bg-bg-elevated p-3">
                <div className="text-xs text-text-secondary">{label}</div>
                {!available ? (
                  <div className="mt-1 text-lg font-semibold text-text-secondary">Not configured</div>
                ) : (
                  <>
                    <div className={`mt-1 text-xl font-semibold ${missing > 0 ? 'text-yellow-300' : 'text-green-400'}`}>
                      {expected > 0 ? `${Math.min(100, actual / expected * 100).toFixed(1)}%` : '100%'}
                    </div>
                    <div className="text-xs text-text-secondary">
                      {actual} of {expected} intervals · {missing} missing
                    </div>
                  </>
                )}
              </div>
            ))}
          </div>
          {comparison.data.days.some((day) => day.missing_import_intervals + day.missing_export_intervals + day.missing_gas_intervals > 0) && (
            <div className="mt-4 max-h-64 overflow-auto">
              <table className="w-full min-w-[560px] text-left text-xs">
                <thead className="sticky top-0 bg-bg-surface text-text-secondary">
                  <tr className="border-b border-white/10">
                    <th className="px-2 py-2">Date</th>
                    <th className="px-2 py-2">Import missing</th>
                    <th className="px-2 py-2">Export missing</th>
                    <th className="px-2 py-2">Gas missing</th>
                  </tr>
                </thead>
                <tbody>
                  {[...comparison.data.days].reverse().filter((day) => day.missing_import_intervals + day.missing_export_intervals + day.missing_gas_intervals > 0).map((day) => (
                    <tr key={day.date} className="border-b border-white/5 text-text-primary">
                      <td className="px-2 py-2 font-medium">{day.date}</td>
                      <td className="px-2 py-2">{day.missing_import_intervals} / {day.expected_import_intervals}</td>
                      <td className="px-2 py-2">{day.missing_export_intervals} / {day.expected_export_intervals}</td>
                      <td className="px-2 py-2">{day.missing_gas_intervals} / {day.expected_gas_intervals}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </section>
      )}

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
