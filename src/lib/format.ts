export function formatPower(watts: number): string {
  const abs = Math.abs(watts);
  if (abs >= 1000) {
    return `${(watts / 1000).toFixed(1)}kW`;
  }
  return `${Math.round(watts)}W`;
}

export function formatEnergy(kwh: number): string {
  return `${kwh.toFixed(1)}kWh`;
}

export function formatPercent(pct: number): string {
  return `${Math.round(pct)}%`;
}

export function formatVoltage(v: number): string {
  return `${v.toFixed(1)}V`;
}

export function formatFrequency(f: number): string {
  return `${f.toFixed(2)}Hz`;
}

export function formatTemp(c: number): string {
  if (!Number.isFinite(c)) {
    return '—';
  }
  return `${c.toFixed(1)}°C`;
}

export function formatCurrent(a: number): string {
  return `${a.toFixed(1)}A`;
}
