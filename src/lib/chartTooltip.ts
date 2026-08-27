// Theme-aware tooltip styling for Recharts charts, matching the #232
// pass across the chart pages: the app's CSS variables give an elevated
// dark surface in dark mode and a light one in light mode, so the tooltip
// text stays readable in both. Without this, Recharts renders its default
// white box — light text on white in dark mode (issue #283 feedback).
export const tooltipStyleProps = {
  contentStyle: {
    backgroundColor: 'var(--app-bg-elevated)',
    borderColor: 'rgba(255, 255, 255, 0.1)',
    borderRadius: '0.5rem',
    boxShadow: '0 20px 25px -5px rgb(0 0 0 / 0.1), 0 8px 10px -6px rgb(0 0 0 / 0.1)',
  },
  labelStyle: {
    color: 'var(--app-text-primary)',
    fontWeight: 700,
  },
  itemStyle: {
    fontWeight: 600,
  },
};
