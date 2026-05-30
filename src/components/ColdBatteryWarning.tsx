import { useState, useEffect } from 'react';
import { Link } from 'react-router-dom';
import { useInverterStore } from '../store/useInverterStore';
import { apiGet } from '../lib/api';

interface AutoWinterConfig {
  enabled: boolean;
  cold_threshold: number;
}

export default function ColdBatteryWarning() {
  const { snapshot, developerMode } = useInverterStore();
  const [config, setConfig] = useState<AutoWinterConfig | null>(null);

  useEffect(() => {
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: { config: AutoWinterConfig } }>('/api/auto-winter');
        if (res.ok) setConfig(res.data.config);
      } catch { /* ignore */ }
    })();
  }, []);

  if (!snapshot || !config) return null;

  const forceShow =
    developerMode && localStorage.getItem('dev_force_cold_warning') === 'true';

  if (!forceShow) {
    if (config.enabled) return null;
    if (snapshot.battery_temperature >= config.cold_threshold) return null;
  }

  const tempDisplay = forceShow
    ? `${snapshot.battery_temperature.toFixed(1)}°C (dev override)`
    : `${snapshot.battery_temperature.toFixed(1)}°C`;

  return (
    <div className="bg-blue-900/30 border border-blue-700/40 rounded-xl px-4 py-3 text-sm space-y-1">
      <p className="text-blue-200 font-medium">
        Cold battery ({tempDisplay})
      </p>
      <p className="text-blue-300/80 text-xs">
        Enable{' '}
        <Link to="/control" className="text-blue-200 underline hover:text-blue-100">
          Auto Winter Mode
        </Link>{' '}
        on the Control page to automatically charge the battery when cold —
        this warms the cells and protects battery health.
      </p>
    </div>
  );
}
