import { describe, it, expect } from 'vitest';
import {
  forecastStatusMessages,
  toSolarChartData,
  toBatteryChartData,
  tomorrowSummary,
} from '../../src/lib/forecast';
import type { ForecastData, ForecastBattery } from '../../src/lib/forecast';

describe('forecastStatusMessages', () => {
  it('maps every documented degradation code to a human explanation', () => {
    const messages = forecastStatusMessages([
      'weather_disabled',
      'no_coordinates',
      'no_forecast_data',
      'calibrating',
      'insufficient_consumption_history',
      'no_snapshot',
      'no_battery_capacity',
    ]);
    expect(messages).toHaveLength(7);
    expect(messages[0]).toMatch(/weather integration/i);
    expect(messages[1]).toMatch(/location/i);
    expect(messages[2]).toMatch(/no forecast data/i);
    expect(messages[3]).toMatch(/calibrat/i);
    expect(messages[4]).toMatch(/consumption history/i);
    expect(messages[5]).toMatch(/inverter/i);
    expect(messages[6]).toMatch(/battery/i);
  });

  it('passes through unknown codes verbatim so new backend codes stay visible', () => {
    expect(forecastStatusMessages(['something_new'])).toEqual([
      'something_new',
    ]);
  });

  it('returns empty for a healthy payload', () => {
    expect(forecastStatusMessages([])).toEqual([]);
  });
});

describe('toSolarChartData', () => {
  it('maps points with the band clamped at zero', () => {
    const points = toSolarChartData([
      { timestamp: 100, kwh: 2.0, band_low: 1.6, band_high: 2.4 },
      { timestamp: 200, kwh: 0.1, band_low: -0.05, band_high: 0.25 },
    ]);
    expect(points).toEqual([
      { timestamp: 100, kwh: 2.0, low: 1.6, high: 2.4 },
      { timestamp: 200, kwh: 0.1, low: 0, high: 0.25 },
    ]);
  });
});

describe('toBatteryChartData', () => {
  it('maps the hourly SOC series', () => {
    const battery: ForecastBattery = {
      capacity_kwh: 10,
      start_soc_pct: 50,
      reserve_soc_pct: 10,
      hours: [
        [100, 55],
        [200, 60],
      ],
      end_soc_pct: 60,
    };
    expect(toBatteryChartData(battery)).toEqual([
      { timestamp: 100, soc: 55 },
      { timestamp: 200, soc: 60 },
    ]);
  });

  it('returns empty for an empty series', () => {
    const battery: ForecastBattery = {
      capacity_kwh: 10,
      start_soc_pct: 50,
      reserve_soc_pct: 10,
      hours: [],
      end_soc_pct: 50,
    };
    expect(toBatteryChartData(battery)).toEqual([]);
  });
});

describe('tomorrowSummary', () => {
  const base: ForecastData = {
    generated_at: 1_700_000_000,
    status: [],
    performance_ratio: 0.8,
    performance_ratio_days: 12,
    solar: [],
    solar_today_remaining_kwh: 10,
    solar_tomorrow_kwh: 18.4,
    consumption: [],
    consumption_days_observed: 14,
    consumption_sufficient: true,
    consumption_tomorrow_kwh: 11.2,
    battery: {
      capacity_kwh: 9.5,
      start_soc_pct: 62,
      reserve_soc_pct: 15,
      hours: [[100, 70]],
      end_soc_pct: 70,
    },
    import_tomorrow_kwh: 1.1,
    export_tomorrow_kwh: 7.2,
  };

  it('derives the summary card numbers from the payload', () => {
    expect(tomorrowSummary(base)).toEqual({
      solarKwh: 18.4,
      consumptionKwh: 11.2,
      surplusKwh: 7.2,
      importKwh: 1.1,
      startSocPct: 62,
    });
  });

  it('handles a payload without a battery projection', () => {
    const data: ForecastData = { ...base, battery: null };
    expect(tomorrowSummary(data).startSocPct).toBeNull();
    // Other numbers still derived.
    expect(tomorrowSummary(data).solarKwh).toBe(18.4);
  });
});
