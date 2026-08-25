/**
 * Pure helpers and types for the Forecast page (issue #283).
 *
 * Everything here is a transformation of the `GET /api/forecast` payload —
 * no fetching, no React — so the maths is unit-testable in isolation.
 */

export type ForecastSolarHour = {
  timestamp: number;
  kwh: number;
  band_low: number;
  band_high: number;
};

export type ForecastConsumptionHour = {
  hour: number;
  kwh: number;
  p25: number;
  p75: number;
};

export type ForecastBattery = {
  capacity_kwh: number;
  start_soc_pct: number;
  reserve_soc_pct: number;
  hours: [number, number][];
  end_soc_pct: number;
};

export type ForecastData = {
  generated_at: number;
  status: string[];
  performance_ratio: number | null;
  performance_ratio_days: number | null;
  solar: ForecastSolarHour[];
  solar_today_remaining_kwh: number;
  solar_tomorrow_kwh: number;
  consumption: ForecastConsumptionHour[];
  consumption_days_observed: number;
  consumption_sufficient: boolean;
  consumption_tomorrow_kwh: number;
  battery: ForecastBattery | null;
  import_tomorrow_kwh: number;
  export_tomorrow_kwh: number;
};

/**
 * Map backend degradation codes to a human explanation. Unknown codes pass
 * through verbatim so a newer backend's codes stay visible on an older UI.
 */
export function forecastStatusMessages(status: string[]): string[] {
  return status.map((code) => {
    switch (code) {
      case 'weather_disabled':
        return 'Weather integration is off — enable it in Settings so solar forecasts can be fetched.';
      case 'no_coordinates':
        return 'No location configured — set a postcode or coordinates in Settings.';
      case 'no_forecast_data':
        return 'No forecast data yet — the fetcher runs every three hours once weather is enabled.';
      case 'calibrating':
        return 'Solar model is still calibrating against your generation history; numbers are preliminary.';
      case 'insufficient_consumption_history':
        return 'Learning your consumption history — about a week of data is needed.';
      case 'no_snapshot':
        return 'No inverter connection yet — the battery projection needs a live snapshot.';
      case 'no_battery_capacity':
        return 'Battery capacity unknown — connect to the inverter so it can be read.';
      default:
        return code;
    }
  });
}

export type SolarChartPoint = {
  timestamp: number;
  kwh: number;
  low: number;
  high: number;
};

/** Chart-ready solar points with the band clamped at zero. */
export function toSolarChartData(points: ForecastSolarHour[]): SolarChartPoint[] {
  return points.map((p) => ({
    timestamp: p.timestamp,
    kwh: p.kwh,
    low: Math.max(0, p.band_low),
    high: Math.max(0, p.band_high),
  }));
}

export type BatteryChartPoint = { timestamp: number; soc: number };

/** Chart-ready SOC projection points. */
export function toBatteryChartData(battery: ForecastBattery): BatteryChartPoint[] {
  return battery.hours.map(([timestamp, soc]) => ({ timestamp, soc }));
}

export type TomorrowSummary = {
  solarKwh: number;
  consumptionKwh: number;
  surplusKwh: number;
  importKwh: number;
  startSocPct: number | null;
};

/** The "Tomorrow" summary card numbers. Surplus is the predicted export. */
export function tomorrowSummary(data: ForecastData): TomorrowSummary {
  return {
    solarKwh: data.solar_tomorrow_kwh,
    consumptionKwh: data.consumption_tomorrow_kwh,
    surplusKwh: data.export_tomorrow_kwh,
    importKwh: data.import_tomorrow_kwh,
    startSocPct: data.battery ? data.battery.start_soc_pct : null,
  };
}

// ---------------------------------------------------------------------------
// Planner (Phase 2)
// ---------------------------------------------------------------------------

export type PlannerChargeWindow = {
  /** "HH:MM" wall-clock start — 02:00 even on tomorrow's calendar day. */
  start: string;
  /** "HH:MM" wall-clock end. */
  end: string;
  /** £/kWh inside the window. */
  rate: number;
  /** True when the start occurs on the day after the planner's `now`. */
  tomorrow: boolean;
};

export type PlanRecommendation =
  | {
      kind: 'charge';
      window: PlannerChargeWindow;
      kwh: number;
      target_soc_pct: number;
      projected_end_soc_pct: number;
      rationale: string;
    }
  | {
      kind: 'no_charge_needed';
      projected_end_soc_pct: number;
      rationale: string;
    }
  | { kind: 'no_plan'; reason: string };

export type PlanApply = {
  charge_slot: {
    slot: 1;
    enabled: true;
    start_hour: number;
    start_minute: number;
    end_hour: number;
    end_minute: number;
    target_soc: number;
  };
  timed_charge: { enabled: true };
} | null;

export type PlanResponse = {
  recommendation: PlanRecommendation;
  apply: PlanApply;
};

/** Short headline for the Plan card. Degrades gracefully per kind. */
export function forecastPlanTitle(rec: PlanRecommendation): string {
  if (rec.kind === 'charge') {
    const when = rec.window.tomorrow ? 'Tomorrow' : 'Tonight';
    return `Overnight charge — ${when} ${rec.window.start}\u2013${rec.window.end}, ${rec.kwh.toFixed(1)} kWh`;
  }
  if (rec.kind === 'no_charge_needed') {
    return `No overnight charge needed — solar covers the day`;
  }
  return `Plan not ready yet — ${rec.reason}`;
}
