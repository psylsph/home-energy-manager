import { memo } from 'react';
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

function SummaryTilesInner({ snapshot: s }: Props) {
  const isGateway = s.device_type_code?.startsWith('70');
  const tiles: TileDef[] = [
    {
      label: 'Solar Today',
      value: formatEnergy(s.today_solar_kwh),
      accent: '#F59E0B',
      icon: '☀️',
    },
    {
      label: 'Consumption' + (isGateway ? ' (excl. EV)' : ''),
      value: formatEnergy(s.today_consumption_kwh),
      accent: '#14B8A6',
      icon: '🏠',
    },
    {
      label: 'Import',
      value: formatEnergy(s.today_import_kwh),
      accent: '#EF4444',
      icon: '⬇️',
    },
    {
      label: 'Export',
      value: formatEnergy(s.today_export_kwh),
      accent: '#22C55E',
      icon: '⬆️',
    },
  ];

  return (
    <div className="bg-bg-surface rounded-2xl p-6 h-full flex flex-col gap-4">
      <h2 className="text-text-primary text-base font-semibold tracking-wide">Today</h2>
      <div className="grid grid-cols-2 gap-3">
        {tiles.map((t) => (
          <div
            key={t.label}
            className="bg-bg-elevated rounded-xl p-4 flex flex-col gap-2 border border-transparent hover:border-white/5 transition-colors"
          >
            <div className="flex items-center gap-2">
              <span
                className="w-8 h-8 rounded-lg flex items-center justify-center text-sm"
                style={{ backgroundColor: t.accent + '20' }}
              >
                {t.icon}
              </span>
              <span className="text-text-secondary text-xs font-medium">{t.label}</span>
            </div>
            <span
              className="text-xl font-mono font-bold tracking-tight"
              style={{ color: t.accent }}
            >
              {t.value}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

const SummaryTiles = memo(SummaryTilesInner);
export default SummaryTiles;
