export type AdaptiveChargePeriod = {
  enabled: boolean;
  all_day: boolean;
  start_hour: number;
  start_minute: number;
  end_hour: number;
  end_minute: number;
  low_soc: number;
  recovery_soc: number;
  preferred_rate_percent: number;
  recovery_rate_percent: number;
};

export type AdaptiveChargeConfig = {
  periods: AdaptiveChargePeriod[];
  confirmation_readings: number;
};

export const DEFAULT_ADAPTIVE_PERIOD: AdaptiveChargePeriod = {
  enabled: true,
  all_day: false,
  start_hour: 8,
  start_minute: 0,
  end_hour: 17,
  end_minute: 0,
  low_soc: 30,
  recovery_soc: 40,
  preferred_rate_percent: 50,
  recovery_rate_percent: 100,
};

export function validateAdaptiveChargeConfig(config: AdaptiveChargeConfig): string | null {
  const enabled = config.periods.filter((period) => period.enabled);
  if (enabled.length === 0) return 'Enable at least one period.';
  if (config.periods.length > 4) return 'Adaptive Charge supports at most four periods.';
  if (config.confirmation_readings < 1 || config.confirmation_readings > 10) {
    return 'Confirmation readings must be between 1 and 10.';
  }

  const occupied = new Array<boolean>(1440).fill(false);
  for (const [index, period] of config.periods.entries()) {
    if (!period.enabled) continue;
    if (period.low_soc < 4 || period.low_soc > 99) {
      return `Period ${index + 1}: Low SOC must be between 4% and 99%.`;
    }
    if (period.recovery_soc <= period.low_soc || period.recovery_soc > 100) {
      return `Period ${index + 1}: Recovery SOC must be above Low SOC.`;
    }
    if (period.preferred_rate_percent < 0 || period.preferred_rate_percent > 100
      || period.recovery_rate_percent < 0 || period.recovery_rate_percent > 100) {
      return `Period ${index + 1}: Charge rates must be between 0% and 100%.`;
    }
    if (period.recovery_rate_percent < period.preferred_rate_percent) {
      return `Period ${index + 1}: Recovery rate must not be below Preferred rate.`;
    }
    const start = period.start_hour * 60 + period.start_minute;
    const end = period.end_hour * 60 + period.end_minute;
    if (!period.all_day && start === end) {
      return `Period ${index + 1}: Start and end must differ unless All day is selected.`;
    }
    for (let minute = 0; minute < 1440; minute += 1) {
      const covered = period.all_day || (start < end
        ? minute >= start && minute < end
        : minute >= start || minute < end);
      if (!covered) continue;
      if (occupied[minute]) return `Period ${index + 1} overlaps another period.`;
      occupied[minute] = true;
    }
  }
  return null;
}

/**
 * Short caption shown under each Adaptive Charge SOC field.
 *
 * Adaptive Charge only sets a maximum *charge* rate within its time window —
 * it never forces charging and has no effect on discharge. "Low SOC" is the
 * point where the rate switches to the recovery charge rate; it is not a
 * discharge floor. To cap discharge, use the per-slot Target SOC or the
 * global Discharge Cutoff SOC.
 */
export function adaptiveSocFieldCaption(field: 'low_soc' | 'recovery_soc'): string {
  switch (field) {
    case 'low_soc':
      return 'Below this, switch to the recovery charge rate. This is a charge-rate trigger only — it does not stop discharge. Use the slot Target SOC or Discharge Cutoff SOC to cap discharge.';
    case 'recovery_soc':
      return 'Once SOC climbs back above this, drop to the preferred charge rate.';
  }
}

export function adaptiveStateLabel(state: string | undefined): string {
  switch (state) {
    case 'outside_window': return 'Outside configured period';
    case 'preferred': return 'Preferred rate active';
    case 'recovery': return 'Low-SOC recovery active';
    case 'suspended_auto_winter': return 'Suspended by Auto Winter';
    case 'restoring': return 'Restoring manual rate';
    case 'error': return 'Error';
    default: return 'Inactive';
  }
}
