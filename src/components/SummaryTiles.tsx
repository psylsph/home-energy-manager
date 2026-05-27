import type { InverterSnapshot } from '../lib/types';
import { formatEnergy } from '../lib/format';

interface Props {
  snapshot: InverterSnapshot;
}

interface TileDef {
  label: string;
  value: string;
  accent: string;
  icon: string;
}

export default function SummaryTiles({ snapshot: s }: Props) {
  const tiles: TileDef[] = [
    {
      label: 'Solar Today',
      value: formatEnergy(s.today_solar_kwh),
      accent: '#F59E0B',
      icon: '☀',
    },
    {
      label: 'Home Today',
      value: formatEnergy(s.today_consumption_kwh),
      accent: '#14B8A6',
      icon: '🏠',
    },
    {
      label: 'Import Today',
      value: formatEnergy(s.today_import_kwh),
      accent: '#EF4444',
      icon: '⬇',
    },
    {
      label: 'Export Today',
      value: formatEnergy(s.today_export_kwh),
      accent: '#22C55E',
      icon: '⬆',
    },
  ];

  return (
    <div className="grid grid-cols-2 gap-3">
      {tiles.map((t) => (
        <div
          key={t.label}
          className="bg-bg-surface rounded-xl p-4 flex flex-col gap-1"
        >
          <div className="flex items-center gap-2">
            <span className="text-lg">{t.icon}</span>
            <span className="text-text-secondary text-xs font-sans">{t.label}</span>
          </div>
          <span
            className="text-xl font-mono font-bold"
            style={{ color: t.accent }}
          >
            {t.value}
          </span>
        </div>
      ))}
    </div>
  );
}
