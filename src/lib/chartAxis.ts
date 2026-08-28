// Shared x-axis configuration for the time-series charts. Recharts
// defaults `XAxis` to a *category* axis, which renders data points as
// evenly-spaced slots regardless of their actual timestamps — the "now"
// anchor point the forecast charts prepend (e.g. 22:45) lands a full
// category slot before the first hour, and gaps (DST, a missing forecast
// hour) squeeze or stretch as if they weren't there. A numeric axis with
// the domain pinned to the data places every point at its true
// time-proportional position. Keep `tickFormatter` at the call site —
// it labels the ticks; this only scales them.
export const timeAxisProps: { type: 'number'; domain: [string, string] } = {
  type: 'number',
  domain: ['dataMin', 'dataMax'],
};
