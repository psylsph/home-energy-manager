import { describe, it, expect } from 'vitest';
import {
  actualSocSeries,
  actualSolarKwhByHour,
  compareBattery,
  compareSolar,
  decisionRows,
  mergeSeries,
  pastAxisDomain,
  pastHistoryPath,
  planHistoryPath,
  pastRangeSeconds,
  pastTickStepSeconds,
  stitchPlannedBattery,
  stitchPlannedSolar,
} from '../../src/lib/planHistory';
import type { PlanHistorySnapshot } from '../../src/lib/planHistory';
import type { ForecastData, PlanResponse } from '../../src/lib/forecast';

// Hour-aligned base so every expectation is timezone-independent.
const H = 3600;
const T0 = 1_700_000_000 - (1_700_000_000 % H); // an exact hour boundary

function forecast(
  generatedAt: number,
  startSoc: number,
  hours: [number, number][],
  solar: [number, number][] = [],
): ForecastData {
  return {
    generated_at: generatedAt,
    forecast_fetched_at: generatedAt,
    forecast_age_seconds: 0,
    status: [],
    performance_ratio: 0.8,
    performance_ratio_days: 10,
    solar: solar.map(([timestamp, kwh]) => ({
      timestamp,
      kwh,
      band_low: kwh * 0.8,
      band_high: kwh * 1.2,
    })),
    solar_today_remaining_kwh: 0,
    solar_tomorrow_kwh: 0,
    consumption_weekday: [],
    consumption_weekend: [],
    consumption_days_observed: 0,
    consumption_sufficient: false,
    consumption_tomorrow_kwh: 0,
    battery: {
      capacity_kwh: 9.5,
      start_soc_pct: startSoc,
      reserve_soc_pct: 4,
      hours,
      end_soc_pct: hours.length ? hours[hours.length - 1][1] : startSoc,
      with_current_schedule: null,
    },
    import_tomorrow_kwh: 0,
    export_tomorrow_kwh: 0,
  };
}

const noPlan: PlanResponse = {
  recommendation: { kind: 'no_plan', reason: 'x' },
  apply: null,
};

function chargePlan(withCharge: [number, number][]): PlanResponse {
  return {
    recommendation: {
      kind: 'charge',
      window: { start: '02:00', end: '04:00', rate: 0.07, tomorrow: false },
      kwh: 3,
      min_soc_pct: 20,
      observed_min_soc_pct: 5,
      after_min_soc_pct: 60,
      current_soc_pct: 30,
      rationale: 'Charging lifts the battery to 60%.',
      with_charge_series: withCharge,
      import_tomorrow_with_charge_kwh: 1,
      export_tomorrow_with_charge_kwh: 0,
    },
    apply: null,
  };
}

function snap(
  recordedAt: number,
  fc: ForecastData | null,
  plan: PlanResponse | null = noPlan,
  kind = 'before_window',
): PlanHistorySnapshot {
  return { recorded_at: recordedAt, kind, strategy: 'min_soc', plan, forecast: fc };
}

describe('pastRangeSeconds', () => {
  it('maps each choice to its length and Off to nothing', () => {
    expect(pastRangeSeconds('off')).toBe(0);
    expect(pastRangeSeconds('24h')).toBe(24 * H);
    expect(pastRangeSeconds('3d')).toBe(3 * 24 * H);
    expect(pastRangeSeconds('7d')).toBe(7 * 24 * H);
  });
});

describe('pastAxisDomain', () => {
  it('keeps the forward-only axis when the past view is off', () => {
    expect(pastAxisDomain('off', T0, T0 + 72 * H)).toEqual([T0, T0 + 72 * H]);
  });

  it('pulls the left edge back by the chosen range', () => {
    expect(pastAxisDomain('24h', T0, T0 + 72 * H)).toEqual([T0 - 24 * H, T0 + 72 * H]);
  });
});

describe('stitchPlannedBattery', () => {
  it('draws each decision from when it was made until the next one replaces it', () => {
    const first = snap(
      T0,
      forecast(T0, 50, [
        [T0, 48],
        [T0 + H, 46],
        [T0 + 2 * H, 44],
        [T0 + 3 * H, 42],
      ]),
    );
    const second = snap(
      T0 + 2 * H,
      forecast(T0 + 2 * H, 40, [
        [T0 + 2 * H, 38],
        [T0 + 3 * H, 36],
      ]),
    );
    const out = stitchPlannedBattery([first, second], T0 + 10 * H);
    // Series points are end-of-bucket states, so they're re-labelled +1h,
    // and each decision is anchored at the live SOC it started from. The
    // first decision stops before the second one's instant and a null
    // breaks the line so the two are not joined.
    expect(out).toEqual([
      [T0, 50],
      [T0 + H, 48],
      [T0 + H + 1, null],
      [T0 + 2 * H, 40],
      [T0 + 3 * H, 38],
      [T0 + 4 * H, 36],
    ]);
  });

  it('stops at now so the past view never shows the future', () => {
    const only = snap(
      T0,
      forecast(T0, 50, [
        [T0, 48],
        [T0 + H, 46],
        [T0 + 2 * H, 44],
      ]),
    );
    const out = stitchPlannedBattery([only], T0 + H);
    expect(out.map(([ts]) => ts)).toEqual([T0, T0 + H]);
  });

  it('plots the what-if line when the decision was a charge plan', () => {
    const fc = forecast(T0, 30, [
      [T0, 28],
      [T0 + H, 26],
    ]);
    const plan = chargePlan([
      [T0, 40],
      [T0 + H, 55],
    ]);
    const out = stitchPlannedBattery([snap(T0, fc, plan)], T0 + 5 * H);
    expect(out).toEqual([
      [T0, 30],
      [T0 + H, 40],
      [T0 + 2 * H, 55],
    ]);
  });

  it('falls back to the solar-only projection when the charge plan has no series', () => {
    const fc = forecast(T0, 30, [[T0, 28]]);
    const out = stitchPlannedBattery([snap(T0, fc, chargePlan([]))], T0 + 5 * H);
    expect(out).toEqual([
      [T0, 30],
      [T0 + H, 28],
    ]);
  });

  it('skips decisions that stored no battery projection', () => {
    const noBattery = { ...forecast(T0, 30, []), battery: null };
    expect(stitchPlannedBattery([snap(T0, noBattery), snap(T0, null)], T0 + H)).toEqual([]);
  });

  it('is order-independent and handles an empty history', () => {
    expect(stitchPlannedBattery([], T0)).toEqual([]);
    const a = snap(T0, forecast(T0, 50, [[T0, 48]]));
    const b = snap(T0 + 5 * H, forecast(T0 + 5 * H, 40, [[T0 + 5 * H, 38]]));
    expect(stitchPlannedBattery([b, a], T0 + 9 * H)).toEqual(
      stitchPlannedBattery([a, b], T0 + 9 * H),
    );
  });
});

describe('stitchPlannedSolar', () => {
  it('uses the latest forecast made before each hour, completed hours only', () => {
    const first = snap(
      T0,
      forecast(T0, 0, [], [
        [T0, 1],
        [T0 + H, 2],
        [T0 + 2 * H, 3],
      ]),
    );
    const second = snap(
      T0 + 2 * H,
      forecast(T0 + 2 * H, 0, [], [
        [T0 + 2 * H, 4],
        [T0 + 3 * H, 5],
      ]),
    );
    // now is mid-way through the T0+3h hour, so that hour is not complete.
    const out = stitchPlannedSolar([first, second], T0 + 3 * H + 1800);
    expect(out).toEqual([
      [T0, 1],
      [T0 + H, 2],
      [T0 + 2 * H, 4],
    ]);
  });

  it('returns nothing for an empty history or one without forecasts', () => {
    expect(stitchPlannedSolar([], T0)).toEqual([]);
    expect(stitchPlannedSolar([snap(T0, null)], T0 + 5 * H)).toEqual([]);
  });
});

describe('actualSocSeries', () => {
  it('converts history milliseconds to seconds and keeps the visible window', () => {
    const points = [
      { t: (T0 - H) * 1000, v: 90 },
      { t: T0 * 1000, v: 80 },
      { t: (T0 + H) * 1000, v: 70 },
      { t: (T0 + 2 * H) * 1000, v: 60 },
    ];
    expect(actualSocSeries(points, T0, T0 + H)).toEqual([
      [T0, 80],
      [T0 + H, 70],
    ]);
  });

  it('copes with a missing series', () => {
    expect(actualSocSeries(undefined, 0, 100)).toEqual([]);
  });

  it('drops non-finite readings', () => {
    expect(actualSocSeries([{ t: T0 * 1000, v: Number.NaN }], 0, T0 + 1)).toEqual([]);
  });
});

describe('actualSolarKwhByHour', () => {
  it('averages the watts in each completed hour into kWh', () => {
    const points = [
      { t: (T0 + 0) * 1000, v: 1000 },
      { t: (T0 + 1800) * 1000, v: 3000 },
      { t: (T0 + H) * 1000, v: 500 },
      { t: (T0 + H + 1800) * 1000, v: 500 },
    ];
    expect(actualSolarKwhByHour(points, T0 + 2 * H)).toEqual([
      [T0, 2],
      [T0 + H, 0.5],
    ]);
  });

  it('leaves out the hour still in progress', () => {
    const points = [
      { t: T0 * 1000, v: 1000 },
      { t: (T0 + H) * 1000, v: 1000 },
    ];
    expect(actualSolarKwhByHour(points, T0 + H + 600)).toEqual([[T0, 1]]);
  });

  it('copes with a missing series', () => {
    expect(actualSolarKwhByHour(undefined, T0)).toEqual([]);
  });
});

describe('compareBattery', () => {
  it('reports the mean gap and the bias between plan and reality', () => {
    // Each decision starts at the SOC it was made from, so that first point
    // is the measured SOC by construction and is not scored.
    const planned: [number, number | null][] = [
      [T0, 50],
      [T0 + H, 40],
      [T0 + H + 1, null],
      [T0 + 2 * H, 30],
      [T0 + 3 * H, 20],
    ];
    const actual: [number, number][] = [
      [T0, 48],
      [T0 + H, 44],
      [T0 + 2 * H, 30],
      [T0 + 3 * H, 25],
    ];
    const result = compareBattery(planned, actual);
    expect(result).not.toBeNull();
    expect(result!.points).toBe(2);
    // Errors are actual − planned: +4 and +5.
    expect(result!.meanAbsErrorPct).toBeCloseTo(4.5, 5);
    expect(result!.biasPct).toBeCloseTo(4.5, 5);
  });

  it('does not let the starting points flatter the plan', () => {
    // Only anchors: nothing was actually predicted, so nothing to score.
    expect(
      compareBattery(
        [
          [T0, 50],
          [T0 + 1, null],
          [T0 + 5 * H, 30],
        ],
        [
          [T0, 50],
          [T0 + 5 * H, 90],
        ],
      ),
    ).toBeNull();
  });

  it('ignores planned points with no reading nearby', () => {
    const result = compareBattery(
      [
        [T0, 50],
        [T0 + H, 40],
        [T0 + 10 * H, 10],
      ],
      [[T0 + H, 40]],
    );
    expect(result!.points).toBe(1);
    expect(result!.meanAbsErrorPct).toBe(0);
  });

  it('picks the nearest reading however the readings are ordered', () => {
    const result = compareBattery(
      [
        [T0, 50],
        [T0 + 2 * H, 40],
      ],
      [
        [T0 + 3 * H, 90],
        [T0 + 2 * H + 600, 43],
        [T0, 50],
        [T0 + 2 * H - 1500, 10],
      ],
    );
    // 43 is 600 s away, 10 is 1500 s away.
    expect(result!.biasPct).toBeCloseTo(3, 5);
  });

  it('returns null when nothing can be compared', () => {
    expect(compareBattery([], [[T0, 1]])).toBeNull();
    expect(compareBattery([[T0, 1]], [])).toBeNull();
    expect(compareBattery([[T0, null]], [[T0, 1]])).toBeNull();
  });
});

describe('compareSolar', () => {
  it('totals only the hours that have both a forecast and a reading', () => {
    const result = compareSolar(
      [
        [T0, 2],
        [T0 + H, 3],
        [T0 + 2 * H, 9],
      ],
      [
        [T0, 1],
        [T0 + H, 4],
      ],
    );
    expect(result).toEqual({ hours: 2, forecastKwh: 5, actualKwh: 5 });
  });

  it('returns null when the series do not overlap', () => {
    expect(compareSolar([[T0, 1]], [[T0 + H, 1]])).toBeNull();
    expect(compareSolar([], [])).toBeNull();
  });
});

describe('mergeSeries', () => {
  it('merges named series into rows sorted by timestamp, keeping the base rows', () => {
    const rows = mergeSeries([{ timestamp: T0 + 2 * H, soc: 70 }], {
      planned: [
        [T0, 50],
        [T0 + H, null],
      ],
      actual: [[T0, 48]],
    });
    expect(rows).toEqual([
      { timestamp: T0, planned: 50, actual: 48 },
      { timestamp: T0 + H, planned: null },
      { timestamp: T0 + 2 * H, soc: 70 },
    ]);
  });

  it('adds onto an existing row at the same instant', () => {
    const rows = mergeSeries([{ timestamp: T0, soc: 70 }], { actual: [[T0, 68]] });
    expect(rows).toEqual([{ timestamp: T0, soc: 70, actual: 68 }]);
  });
});

describe('decisionRows', () => {
  it('lists decisions newest first with a title and the reason given', () => {
    const rows = decisionRows([
      snap(T0, forecast(T0, 30, []), chargePlan([]), 'before_window'),
      snap(
        T0 + 5 * H,
        forecast(T0 + 5 * H, 30, []),
        {
          recommendation: {
            kind: 'no_charge_needed',
            min_soc_pct: 20,
            observed_min_soc_pct: 60,
            current_soc_pct: 80,
            rationale: 'Sunny day.',
          },
          apply: null,
        },
        'auto_apply',
      ),
    ]);
    expect(rows.map((r) => r.recordedAt)).toEqual([T0 + 5 * H, T0]);
    expect(rows[0].kindLabel).toBe('Auto-applied');
    expect(rows[0].title).toBe('No overnight charge needed — solar covers the day');
    expect(rows[0].rationale).toBe('Sunny day.');
    expect(rows[1].kindLabel).toBe('Before the cheap window');
    expect(rows[1].title).toContain('Overnight charge');
    expect(rows[1].rationale).toBe('Charging lifts the battery to 60%.');
  });

  it('copes with a decision whose plan could not be read', () => {
    const rows = decisionRows([snap(T0, null, null, 'refresh')]);
    expect(rows).toHaveLength(1);
    expect(rows[0].title).toBe('Plan unavailable');
    expect(rows[0].rationale).toBeNull();
    expect(rows[0].kindLabel).toBe('Refreshed');
  });

  it('passes an unknown kind through untouched', () => {
    expect(decisionRows([snap(T0, null, null, 'future_kind')])[0].kindLabel).toBe('future_kind');
  });

  it('uses the reason when the planner had no plan', () => {
    const rows = decisionRows([snap(T0, null, noPlan)]);
    expect(rows[0].rationale).toBe('x');
  });
});

describe('pastHistoryPath', () => {
  it('asks for the exact window, with a bucket size that suits its length', () => {
    const now = 1_800_000_000;
    const day = new URLSearchParams(pastHistoryPath('24h', now).split('?')[1]);
    expect(pastHistoryPath('24h', now).startsWith('/api/history?')).toBe(true);
    expect(day.get('range')).toBe('24h');
    expect(day.get('fields')).toBe('soc,solar_power');
    expect(day.get('start_ms')).toBe(String((now - 24 * H) * 1000));
    expect(day.get('end_ms')).toBe(String(now * 1000));

    // The history endpoint has no 3-day bucket; the 7-day one is used with
    // the explicit window cutting it to length.
    const three = new URLSearchParams(pastHistoryPath('3d', now).split('?')[1]);
    expect(three.get('range')).toBe('7d');
    expect(three.get('start_ms')).toBe(String((now - 3 * 24 * H) * 1000));
    expect(new URLSearchParams(pastHistoryPath('7d', now).split('?')[1]).get('range')).toBe('7d');
  });
});

describe('planHistoryPath', () => {
  it('asks for the decisions in the visible window, in unix seconds', () => {
    const now = 1_800_000_000;
    const params = new URLSearchParams(planHistoryPath('3d', now).split('?')[1]);
    expect(planHistoryPath('3d', now).startsWith('/api/forecast/plan-history?')).toBe(true);
    expect(params.get('from')).toBe(String(now - 3 * 24 * H));
    expect(params.get('to')).toBe(String(now + 1));
  });
});

describe('pastTickStepSeconds', () => {
  it('labels every 12 hours up to a day back and daily beyond that', () => {
    expect(pastTickStepSeconds('off')).toBe(12 * H);
    expect(pastTickStepSeconds('24h')).toBe(12 * H);
    expect(pastTickStepSeconds('3d')).toBe(24 * H);
    expect(pastTickStepSeconds('7d')).toBe(24 * H);
  });
});
