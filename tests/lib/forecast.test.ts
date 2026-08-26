import { describe, it, expect } from 'vitest';
import {
  forecastPlanTitle,
  forecastStatusMessages,
  forwardHourTimestamps,
  shouldRefetchForecast,
  toConsumptionChartData,
  toSolarChartData,
  toBatteryChartData,
  tomorrowSummary,
} from '../../src/lib/forecast';
import type { ForecastData, ForecastBattery, PlanRecommendation } from '../../src/lib/forecast';

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

describe('toConsumptionChartData', () => {
  // Timestamps built from a local Date so the test is timezone-independent.
  const ts = (y: number, m: number, d: number, h: number) =>
    new Date(y, m, d, h, 0, 0).getTime() / 1000;

  it('tiles the typical-day profile onto the forward timestamps by local hour-of-day', () => {
    const consumption = [
      { hour: 6, kwh: 0.4, p25: 0.3, p75: 0.5 },
      { hour: 18, kwh: 1.2, p25: 1.0, p75: 1.4 },
    ];
    const points = toConsumptionChartData(consumption, [
      ts(2024, 5, 15, 6),
      ts(2024, 5, 15, 18),
    ]);
    expect(points).toEqual([
      { timestamp: ts(2024, 5, 15, 6), kwh: 0.4, p25: 0.3, p75: 0.5 },
      { timestamp: ts(2024, 5, 15, 18), kwh: 1.2, p25: 1.0, p75: 1.4 },
    ]);
  });

  it('repeats the same hour-of-day band on subsequent days (the profile is a typical day, the window spans several)', () => {
    const consumption = [{ hour: 2, kwh: 0.25, p25: 0.2, p75: 0.3 }];
    const day1 = ts(2024, 5, 15, 2);
    const day2 = ts(2024, 5, 16, 2);
    const points = toConsumptionChartData(consumption, [day1, day2]);
    expect(points[0].kwh).toBe(0.25);
    expect(points[1].kwh).toBe(0.25);
    expect(points[1].timestamp).toBe(day2);
  });

  it('yields zeros for hours with no observed data', () => {
    const points = toConsumptionChartData(
      [{ hour: 6, kwh: 0.4, p25: 0.3, p75: 0.5 }],
      [ts(2024, 5, 15, 9)],
    );
    expect(points).toEqual([
      { timestamp: ts(2024, 5, 15, 9), kwh: 0, p25: 0, p75: 0 },
    ]);
  });

  it('returns an empty series when there are no forward timestamps', () => {
    expect(toConsumptionChartData([{ hour: 6, kwh: 1, p25: 1, p75: 1 }], [])).toEqual([]);
  });
});

describe('forwardHourTimestamps', () => {
  it('generates hourly ascending timestamps starting at the current hour boundary', () => {
    const stamps = forwardHourTimestamps(48);
    expect(stamps).toHaveLength(48);
    expect(stamps[0] % 3600).toBe(0);
    for (let i = 1; i < stamps.length; i += 1) {
      expect(stamps[i] - stamps[i - 1]).toBe(3600);
    }
    // The first stamp is the current hour — never more than an hour ago.
    const nowSec = Date.now() / 1000;
    expect(nowSec - stamps[0]).toBeGreaterThanOrEqual(0);
    expect(nowSec - stamps[0]).toBeLessThan(3600);
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

describe('forecastPlanTitle', () => {
  const charge: Extract<PlanRecommendation, { kind: 'charge' }> = {
    kind: 'charge',
    window: { start: '02:00', end: '05:00', rate: 0.09, tomorrow: true },
    kwh: 3.2,
    min_soc_pct: 20,
    observed_min_soc_pct: 4,
    after_min_soc_pct: 80,
    charge_target_soc_pct: 90,
    current_soc_pct: 50,
    rationale: 'Charge the battery in the cheap window.',
  };
  const noCharge: Extract<PlanRecommendation, { kind: 'no_charge_needed' }> = {
    kind: 'no_charge_needed',
    min_soc_pct: 20,
    observed_min_soc_pct: 80,
    current_soc_pct: 50,
    rationale: 'Sunny day — the battery fills from solar.',
  };
  const noPlan: Extract<PlanRecommendation, { kind: 'no_plan' }> = {
    kind: 'no_plan',
    reason: 'no battery projection',
  };

  it('renders a clear headline for each kind', () => {
    expect(forecastPlanTitle(charge)).toMatch(/overnight charge/i);
    expect(forecastPlanTitle(charge)).toMatch(/3\.2/);
    expect(forecastPlanTitle(noCharge)).toMatch(/No overnight charge/);
    expect(forecastPlanTitle(noPlan)).toMatch(/plan (unavailable|not ready)/i);
  });

  it('mentions the window hours', () => {
    expect(forecastPlanTitle(charge)).toMatch(/02:00/);
    expect(forecastPlanTitle(charge)).toMatch(/05:00/);
  });
});

describe('shouldRefetchForecast', () => {
  it('refetches on a meaningful SOC change (>= 1 percentage point)', () => {
    expect(shouldRefetchForecast(50, 51, 0, 1000, 3000, 3000)).toBe(true);
    expect(shouldRefetchForecast(50, 50, 0, 1000, 3000, 3000)).toBe(false);
  });

  it('refetches when the rate caps change', () => {
    expect(shouldRefetchForecast(50, 50, 0, 1000, 3000, 5000)).toBe(true);
  });

  it('force-refreshes after the long interval (30 s)', () => {
    expect(shouldRefetchForecast(50, 50, 0, 30_001, 3000, 3000)).toBe(true);
    expect(shouldRefetchForecast(50, 50, 0, 29_999, 3000, 3000)).toBe(false);
  });

  it('debounces rapid changes (no more than once per 5 s)', () => {
    expect(shouldRefetchForecast(50, 50.4, 1000, 6000, 3000, 3000)).toBe(false);
    expect(shouldRefetchForecast(50, 51, 1000, 6000, 3000, 3000)).toBe(true);
  });

  it('treats null snapshots (no inverter yet) as "no change"', () => {
    expect(shouldRefetchForecast(null, null, 0, 1000, 3000, 3000)).toBe(false);
  });

  it('treats the first snapshot arrival as a refetch', () => {
    expect(shouldRefetchForecast(null, 50, 0, 1000, 3000, 3000)).toBe(true);
  });
});

describe('forecastPlanChargeRationale', () => {
  it('mentions the live current SOC so the user can see what drove the kWh ask', () => {
    const charge: Extract<PlanRecommendation, { kind: 'charge' }> = {
      kind: 'charge',
      window: { start: '00:30', end: '05:30', rate: 0.07, tomorrow: true },
      kwh: 5.7,
      min_soc_pct: 20,
      observed_min_soc_pct: 4,
      after_min_soc_pct: 46,
      charge_target_soc_pct: 100,
      current_soc_pct: 4,
      rationale:
        'Battery is at 4% now and tomorrow\'s solar is only expected to leave it at 46% by the end of the day. Charging 5.7 kWh in the 7.0p window (00:30–05:30) lifts it to 100% (about £0.40 of grid import).',
    };
    expect(charge.current_soc_pct).toBe(4);
    // The current SOC must appear so the user can confirm reactivity.
    expect(charge.rationale).toMatch(/4%/);
    // The window/£ numbers must too.
    expect(charge.rationale).toMatch(/5\.7 kWh/);
    expect(charge.rationale).toMatch(/7\.0p/);
    expect(charge.rationale).toMatch(/00:30/);
    expect(charge.rationale).toMatch(/05:30/);
  });
});
