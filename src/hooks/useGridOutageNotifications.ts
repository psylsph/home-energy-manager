import { useEffect, useRef } from 'react';
import { formatPercent, formatPower } from '../lib/format';
import { gridFaultReason, gridFaultTitle } from '../lib/gridFault';
import { useInverterStore } from '../store/useInverterStore';

function canNotify(): boolean {
  return typeof window !== 'undefined' && 'Notification' in window && Notification.permission === 'granted';
}

function sendNotification(title: string, body: string) {
  if (!canNotify()) return;

  try {
    const notification = new Notification(title, {
      body,
      tag: 'home-energy-manager-grid-outage',
    });
    window.setTimeout(() => notification.close(), 15_000);
  } catch (error) {
    console.warn('Failed to show grid notification:', error);
  }
}

type FaultKind = 'none' | 'grid' | 'inverter' | 'battery_temp';

function classifyFault(snapshot: ReturnType<typeof useInverterStore.getState>['snapshot']): FaultKind {
  if (!snapshot) return 'none';
  if (snapshot.grid_loss || !snapshot.grid_online) return 'grid';
  if (snapshot.battery_over_temp) return 'battery_temp';
  if (snapshot.inverter_trip) return 'inverter';
  return 'none';
}

export function useGridOutageNotifications() {
  const snapshot = useInverterStore((state) => state.snapshot);
  // Grid outage notifications are always enabled.
  const enabled = true;
  const previousKind = useRef<FaultKind>('none');
  const notifiedForCurrentOutage = useRef(false);

  useEffect(() => {
    if (!enabled) {
      previousKind.current = 'none';
      notifiedForCurrentOutage.current = false;
      return;
    }

    if (!snapshot) return;

    const kind = classifyFault(snapshot);
    const hadFault = previousKind.current !== 'none';

    if (kind !== 'none' && !hadFault && !notifiedForCurrentOutage.current) {
      const discharging = snapshot.battery_power < 0
        ? ` Battery is discharging at ${formatPower(Math.abs(snapshot.battery_power))}.`
        : '';
      sendNotification(
        gridFaultTitle(snapshot),
        `The inverter reports ${gridFaultReason(snapshot)}. Battery SOC is ${formatPercent(snapshot.soc)}.${discharging}`,
      );
      notifiedForCurrentOutage.current = true;
    }

    if (kind === 'none' && hadFault) {
      // Determine what to say based on what we were recovering from
      const prev = previousKind.current;
      if (prev === 'grid' || prev === 'inverter') {
        sendNotification(
          'Grid power restored',
          `Grid voltage is back at ${snapshot.grid_voltage.toFixed(1)}V. Battery SOC is ${formatPercent(snapshot.soc)}.`,
        );
      } else if (prev === 'battery_temp') {
        sendNotification(
          'Battery temperature normal',
          `The battery has cooled down. Battery SOC is ${formatPercent(snapshot.soc)}.`,
        );
      }
      notifiedForCurrentOutage.current = false;
    }

    if (kind === 'none') {
      notifiedForCurrentOutage.current = false;
    }

    previousKind.current = kind;
  }, [enabled, snapshot]);
}
