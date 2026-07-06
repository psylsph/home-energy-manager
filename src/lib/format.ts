export function formatPower(watts: number): string {
  if (!Number.isFinite(watts)) {
    return '—';
  }
  const abs = Math.abs(watts);
  if (abs >= 1000) {
    return `${(watts / 1000).toFixed(1)}kW`;
  }
  return `${Math.round(watts)}W`;
}

export function formatEnergy(kwh: number): string {
  if (!Number.isFinite(kwh)) {
    return '—';
  }
  return `${kwh.toFixed(1)}kWh`;
}

/**
 * Format the EV charger's running session energy for inline display next to
 * the live power (e.g. `7.7kW(23kWh)`).
 *
 * One decimal place while the session is small enough that the fraction
 * matters (`0.1kWh`, `9.9kWh`), then plain integers once it crosses 10 kWh
 * — by that point the tenths are noise and the inline value stays compact
 * (`23kWh`, `11kWh`). NaN / null renders an em-dash like the other
 * formatters.
 *
 * @example
 *   formatSessionEnergy(0.1)   // "0.1kWh"
 *   formatSessionEnergy(9.9)   // "9.9kWh"
 *   formatSessionEnergy(10)    // "10kWh"
 *   formatSessionEnergy(23)    // "23kWh"
 *   formatSessionEnergy(NaN)   // "—"
 */
export function formatSessionEnergy(kwh: number): string {
  if (!Number.isFinite(kwh)) {
    return '—';
  }
  if (kwh >= 10) {
    return `${Math.round(kwh)}kWh`;
  }
  return `${kwh.toFixed(1)}kWh`;
}

/**
 * Absolute value that preserves non-finite / null values as NaN.
 *
 * Use this instead of `Math.abs(v)` when the result is fed into a format
 * function (formatPower, formatCurrent, …). The format helpers guard with
 * `Number.isFinite` to render '—' for NaN, but `Math.abs(null)` coerces
 * `null` to `0` *before* that guard runs — so a null field (the Gateway
 * sets battery_current / battery_voltage to f32::NAN, which serde_json
 * serialises as null) ends up rendered as '0.0A' instead of '—'.
 *
 * `finiteAbs` converts null / NaN / Infinity to NaN so the format guard
 * still fires and renders the em-dash. Real numbers get the plain absolute
 * value, matching the old `Math.abs` behaviour.
 *
 * @example
 *   finiteAbs(7.8)   // 7.8
 *   finiteAbs(-7.8)  // 7.8
 *   finiteAbs(null)  // NaN  → formatCurrent renders '—'
 *   finiteAbs(NaN)   // NaN
 */
export function finiteAbs(v: number | null | undefined): number {
  if (v == null || !Number.isFinite(v)) return NaN;
  return Math.abs(v);
}

export function formatPercent(pct: number): string {
  return `${Math.round(pct)}%`;
}

export function formatVoltage(v: number): string {
  if (!Number.isFinite(v)) {
    return '—';
  }
  return `${v.toFixed(1)}V`;
}

export function formatFrequency(f: number): string {
  if (!Number.isFinite(f)) {
    return '—';
  }
  return `${f.toFixed(2)}Hz`;
}

export function formatTemp(c: number): string {
  if (!Number.isFinite(c)) {
    return '—';
  }
  return `${c.toFixed(1)}°C`;
}

export function formatCurrent(a: number): string {
  if (!Number.isFinite(a)) {
    return '—';
  }
  return `${a.toFixed(1)}A`;
}

/**
 * Format a power value for the energy flow diagram, clamping sub-threshold
 * readings to zero so tiny flows don't produce visual noise.
 *
 * When `Math.abs(watts) < threshold`, returns `"0W"` regardless of the
 * actual value. Otherwise delegates to [`formatPower`].
 *
 * @example
 *   formatVisualPower(5, 20)   // "0W"
 *   formatVisualPower(20, 20)  // "20W"
 *   formatVisualPower(1500, 20) // "1.5kW"
 */
export function formatVisualPower(watts: number, threshold: number): string {
  if (Math.abs(watts) < threshold) return '0W';
  return formatPower(watts);
}

/**
 * Render a lifetime operating-hours figure as a human-friendly age.
 *
 * Examples:
 *   formatOperatingHours(0)     -> ''         (UI hides the row)
 *   formatOperatingHours(1)     -> '1h'
 *   formatOperatingHours(48)    -> '2d'
 *   formatOperatingHours(900)   -> '5w'
 *   formatOperatingHours(26_280) -> '3y'
 *   formatOperatingHours(29_400) -> '3y 4m'   (29 400 h ≈ 3.36 years)
 *   formatOperatingHours(80_000) -> '9y 1m'
 *
 * Unit ladder (all rounded to the nearest step):
 *   < 24 h          -> "Nh"
 *   < 7 days        -> "Nd"
 *   < 5 weeks       -> "Nw"
 *   < 12 months     -> "Ny"
 *   otherwise       -> "Ny Mm"   (months = remaining / (8760/12) / 730 h)
 *
 * Years use 365.25 days so leap years average out (8766 h/year).
 */
export function formatOperatingHours(hours: number): string {
  if (!Number.isFinite(hours) || hours <= 0) return '';
  if (hours < 24) return `${Math.round(hours)}h`;
  if (hours < 24 * 7) return `${Math.round(hours / 24)}d`;
  if (hours < 24 * 7 * 5) return `${Math.round(hours / (24 * 7))}w`;
  if (hours < 24 * 365.25) return `${Math.round(hours / (24 * 30.4375))}mo`;
  const years = Math.floor(hours / (24 * 365.25));
  const remainingAfterYears = hours - years * 24 * 365.25;
  const months = Math.round(remainingAfterYears / (24 * 30.4375));
  if (months <= 0) return `${years}y`;
  return `${years}y ${months}m`;
}

/**
 * Render a battery-mode enum value as Upper Camel Case.
 *
 * The wire format from the backend uses snake_case strings
 * (`eco`, `eco_paused`, `timed_demand`, `timed_export`, `export_paused`,
 * `unknown`) per `InverterSnapshot['battery_mode']`. The Inverter page
 * surfaces this value directly to the user, where a more polished
 * presentation reads better — `EcoPaused` rather than `eco_paused`.
 *
 * Underscores separate the words; the first letter of each word is
 * uppercased. Unknown / future values pass through as UpperCamel via
 * the same rule so the UI degrades gracefully if the backend grows new
 * modes without a frontend update.
 *
 * Examples:
 *   formatBatteryMode('eco')            -> 'Eco'
 *   formatBatteryMode('eco_paused')     -> 'EcoPaused'
 *   formatBatteryMode('timed_export')   -> 'TimedExport'
 *   formatBatteryMode('export_paused')  -> 'ExportPaused'
 *   formatBatteryMode('unknown')        -> 'Unknown'
 *   formatBatteryMode('foo_bar_baz')    -> 'FooBarBaz'   (forward-compat)
 *   formatBatteryMode(undefined)        -> '—'
 */
/**
 * Format an epoch-millis timestamp to a locale time string (HH:MM:SS).
 * Returns '—' for falsy / non-finite values.
 */
export function formatTimestamp(epochMs: number | null | undefined): string {
  if (epochMs == null || !Number.isFinite(epochMs)) return '—';
  return new Date(epochMs).toLocaleTimeString([], {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

export function formatBatteryMode(mode: string | undefined | null): string {
  if (!mode) return '—';
  const parts = mode.split('_').filter(Boolean);
  if (parts.length === 0) return '—';
  return parts
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1).toLowerCase())
    .join('');
}
