import { useEffect, useState } from 'react';
import {
  Area,
  AreaChart,
  CartesianGrid,
  Line,
  LineChart,
  ReferenceLine,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import { apiGet } from '../lib/api';
import { formatEnergy } from '../lib/format';
import {
  forecastStatusMessages,
  toBatteryChartData,
  toSolarChartData,
  tomorrowSummary,
} from '../lib/forecast';
import type { ForecastData } from '../lib/forecast';

/**
 * Forecast & planning page (issue #283).
 *
 * Displays the backend's assembled prediction: tomorrow's solar and
 * consumption with uncertainty bands, the projected battery SOC, and the
 * headline numbers. Everything is computed backend-side from Open-Meteo
 * radiation plus the user's own history; this page only renders. Every
 * degradation state renders an explanation — never zeros pretending to be
 * a prediction.
 */

const REFRESH_INTERVAL_MS = 10 * 60 * 1000;

function hourLabel(tsSeconds: number): string {
  const d = new Date(tsSeconds * 1000);
  return `${String(d.getHours()).padStart(2, '0')}:00`;
}

function ChartCard({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="bg-bg-surface rounded-lg p-3 sm:p-4">
      <h2 className="text-sm font-semibold text-text-primary mb-3">{title}</h2>
      <div className="h-56 sm:h-64">{children}</div>
    </section>
  );
}

function SummaryTile({
  label,
  value,
  testId,
  hint,
}: {
  label: string;
  value: string;
  testId: string;
  hint?: string;
}) {
  return (
    <div className="bg-bg-surface rounded-lg p-3 sm:p-4 flex flex-col gap-1">
      <span className="text-xs text-text-secondary">{label}</span>
      <span data-testid={testId} className="text-lg sm:text-xl font-bold text-text-primary">
        {value}
      </span>
      {hint ? <span className="text-[11px] text-text-secondary">{hint}</span> : null}
    </div>
  );
}

export default function ForecastPage() {
  const [data, setData] = useState<ForecastData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: ForecastData }>('/api/forecast');
        if (cancelled) return;
        setData(res.data);
        setError(null);
      } catch (e) {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : 'Failed to load forecast');
      } finally {
        if (!cancelled) setLoaded(true);
      }
    };
    void load();
    const timer = setInterval(() => void load(), REFRESH_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  if (error) {
    return (
      <div data-testid="forecast-error" className="bg-red-500/10 border border-red-500/30 rounded-lg p-4 text-sm text-text-primary">
        Couldn’t load the forecast: {error}
      </div>
    );
  }

  if (!loaded || !data) {
    return <div className="text-text-secondary text-sm animate-pulse">Loading forecast…</div>;
  }

  const statusMessages = forecastStatusMessages(data.status);
  const summary = tomorrowSummary(data);
  const solarChart = toSolarChartData(data.solar);
  const batteryChart = data.battery ? toBatteryChartData(data.battery) : [];
  const consumptionChart = data.consumption.map((c) => ({
    hour: `${String(c.hour).padStart(2, '0')}:00`,
    kwh: c.kwh,
    p25: c.p25,
    p75: c.p75,
  }));

  return (
    <div className="flex flex-col gap-3 sm:gap-4 max-w-5xl">
      <div>
        <h1 className="text-lg font-bold text-text-primary">Forecast</h1>
        <p className="text-xs text-text-secondary">
          Predictions from Open-Meteo radiation calibrated against your own
          generation history.
        </p>
      </div>

      {statusMessages.length > 0 && (
        <div
          data-testid="forecast-status-banner"
          className="bg-amber-500/10 border border-amber-500/30 rounded-lg p-3 sm:p-4 flex flex-col gap-1"
        >
          {statusMessages.map((m) => (
            <p key={m} className="text-xs text-text-primary">
              {m}
            </p>
          ))}
        </div>
      )}

      <div className="grid grid-cols-2 md:grid-cols-5 gap-2 sm:gap-3">
        <div className="col-span-2 md:col-span-5 text-xs font-semibold uppercase tracking-wide text-text-secondary">
          Tomorrow
        </div>
        <SummaryTile
          label="Solar prediction"
          value={formatEnergy(summary.solarKwh)}
          testId="forecast-solar-tomorrow"
          hint={
            data.performance_ratio != null
              ? `calibrated, ${data.performance_ratio_days ?? '?'} days`
              : 'preliminary'
          }
        />
        <SummaryTile
          label="House consumption"
          value={formatEnergy(summary.consumptionKwh)}
          testId="forecast-consumption-tomorrow"
          hint={`${data.consumption_days_observed} days observed`}
        />
        <SummaryTile
          label="Expected export"
          value={formatEnergy(summary.surplusKwh)}
          testId="forecast-surplus-tomorrow"
        />
        <SummaryTile
          label="Expected import"
          value={formatEnergy(summary.importKwh)}
          testId="forecast-import-tomorrow"
        />
        {summary.startSocPct != null && (
          <SummaryTile
            label="Battery now"
            value={`${Math.round(summary.startSocPct)}%`}
            testId="forecast-start-soc"
          />
        )}
      </div>

      <ChartCard title="Solar forecast (next 48 h)">
        {solarChart.length === 0 ? (
          <div className="h-full flex items-center justify-center text-xs text-text-secondary">
            No forecast data yet.
          </div>
        ) : (
          <ResponsiveContainer width="100%" height="100%">
            <AreaChart data={solarChart}>
              <CartesianGrid strokeDasharray="3 3" stroke="rgba(255,255,255,0.08)" />
              <XAxis
                dataKey="timestamp"
                tickFormatter={hourLabel}
                stroke="#94a3b8"
                fontSize={10}
              />
              <YAxis stroke="#94a3b8" fontSize={10} unit="kWh" width={48} />
              <Tooltip
                labelFormatter={(ts) => hourLabel(Number(ts))}
                formatter={(value: number | string) => `${Number(value).toFixed(2)} kWh`}
              />
              <Area
                type="monotone"
                dataKey="kwh"
                stroke="#f59e0b"
                fill="#f59e0b"
                fillOpacity={0.25}
                dot={false}
                name="Prediction"
              />
              <Area
                type="monotone"
                dataKey="low"
                stroke="none"
                fill="transparent"
                name="Low"
                legendType="none"
              />
              <Area
                type="monotone"
                dataKey="high"
                stroke="none"
                fill="transparent"
                name="High"
                legendType="none"
              />
            </AreaChart>
          </ResponsiveContainer>
        )}
      </ChartCard>

      <ChartCard title="Consumption profile (typical day)">
        {consumptionChart.length === 0 ? (
          <div className="h-full flex items-center justify-center text-xs text-text-secondary">
            Not enough history yet.
          </div>
        ) : (
          <ResponsiveContainer width="100%" height="100%">
            <AreaChart data={consumptionChart}>
              <CartesianGrid strokeDasharray="3 3" stroke="rgba(255,255,255,0.08)" />
              <XAxis dataKey="hour" stroke="#94a3b8" fontSize={10} interval={3} />
              <YAxis stroke="#94a3b8" fontSize={10} unit="kWh" width={48} />
              <Tooltip formatter={(value: number | string) => `${Number(value).toFixed(2)} kWh`} />
              <Area
                type="monotone"
                dataKey="p25"
                stroke="none"
                fill="#38bdf8"
                fillOpacity={0.15}
                name="p25"
              />
              <Area
                type="monotone"
                dataKey="p75"
                stroke="none"
                fill="#38bdf8"
                fillOpacity={0.15}
                name="p75"
              />
              <Area
                type="monotone"
                dataKey="kwh"
                stroke="#38bdf8"
                fill="#38bdf8"
                fillOpacity={0.2}
                dot={false}
                name="Median"
              />
            </AreaChart>
          </ResponsiveContainer>
        )}
      </ChartCard>

      <ChartCard title="Battery projection">
        {batteryChart.length === 0 ? (
          <div className="h-full flex items-center justify-center text-xs text-text-secondary">
            No battery projection — connect to the inverter.
          </div>
        ) : (
          <ResponsiveContainer width="100%" height="100%">
            <LineChart data={batteryChart}>
              <CartesianGrid strokeDasharray="3 3" stroke="rgba(255,255,255,0.08)" />
              <XAxis
                dataKey="timestamp"
                tickFormatter={hourLabel}
                stroke="#94a3b8"
                fontSize={10}
              />
              <YAxis stroke="#94a3b8" fontSize={10} domain={[0, 100]} unit="%" width={44} />
              <Tooltip
                labelFormatter={(ts) => hourLabel(Number(ts))}
                formatter={(value: number | string) => `${Number(value).toFixed(0)}%`}
              />
              {data.battery && (
                <ReferenceLine
                  y={data.battery.reserve_soc_pct}
                  stroke="#f87171"
                  strokeDasharray="4 4"
                  label={{ value: 'reserve', fill: '#f87171', fontSize: 10 }}
                />
              )}
              <Line
                type="monotone"
                dataKey="soc"
                stroke="#34d399"
                strokeWidth={2}
                dot={false}
                name="SOC"
              />
            </LineChart>
          </ResponsiveContainer>
        )}
      </ChartCard>

      <p className="text-[11px] text-text-secondary">
        Forecast data by Open-Meteo.com (CC-BY 4.0). Solar band is a ±20%
        model estimate until measured accuracy statistics accumulate.
      </p>
    </div>
  );
}
