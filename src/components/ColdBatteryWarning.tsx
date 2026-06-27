import { useState, useEffect } from 'react';
import { Link } from 'react-router-dom';
import { useInverterStore } from '../store/useInverterStore';
import { apiGet } from '../lib/api';

interface AutoWinterConfig {
  enabled: boolean;
}

interface AlertsConfig {
  batt_temp_min: number;
}

export default function ColdBatteryWarning() {
  const { snapshot, developerMode } = useInverterStore();
  const [autoWinter, setAutoWinter] = useState<AutoWinterConfig | null>(null);
  const [alerts, setAlerts] = useState<AlertsConfig | null>(null);

  useEffect(() => {
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: { config: AutoWinterConfig } }>('/api/auto-winter');
        if (res.ok) setAutoWinter(res.data.config);
      } catch { /* ignore */ }
      try {
        const res = await apiGet<{ ok: boolean; data: { config: AlertsConfig } }>('/api/alerts');
        if (res.ok) setAlerts(res.data.config);
      } catch { /* ignore */ }
    })();
  }, []);

  if (!snapshot) return null;

  // Gateway devices don't expose battery temperature (the Gateway
  // aggregates SOC/power/energy but not per-pack temperature — that
  // lives on each AIO's own BMS). The backend sets this field to NaN,
  // which serde_json serializes as null. Skip the warning entirely
  // when the value isn't a finite number.
  if (!Number.isFinite(snapshot.battery_temperature)) return null;

  const threshold = alerts?.batt_temp_min ?? 0;
  const forceShow =
    developerMode && localStorage.getItem('dev_force_cold_warning') === 'true';

  if (!forceShow) {
    // Notifications "Alert if below" threshold; 0 = disabled.
    if (threshold <= 0) return null;
    // Auto Winter already warming the cells — no need to nag.
    if (autoWinter?.enabled) return null;
    // Don't show on startup before real data arrives (temp would be 0).
    if (snapshot.battery_temperature < 0.1) return null;
    // Only warn when actually below the configured alert threshold.
    if (snapshot.battery_temperature >= threshold) return null;
  }

  const tempDisplay = forceShow
    ? `${snapshot.battery_temperature.toFixed(1)}°C (dev override)`
    : `${snapshot.battery_temperature.toFixed(1)}°C`;

  return (
    <div className="bg-blue-900/30 border border-blue-700/40 rounded-xl px-4 py-3 text-sm space-y-1">
      <p className="text-text-primary font-medium">
        Cold battery ({tempDisplay})
      </p>
      <p className="text-text-secondary text-xs">
        Enable{' '}
        <Link to="/control" className="text-text-primary underline hover:text-text-primary">
          Auto Winter Mode
        </Link>{' '}
        on the Control page to automatically charge the battery when cold —
        this warms the cells and protects battery health.
      </p>
    </div>
  );
}
