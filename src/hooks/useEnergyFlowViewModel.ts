/**
 * Builds the energy-flow view model for the Status page diagrams.
 *
 * Both renderers — the orbit and the house view (issue #422) — call this hook,
 * so the store-driven options (noise threshold, grid CT meter) and the EV
 * charger label logic are applied in exactly one place and the two views can
 * never disagree on the numbers.
 */
import { useInverterStore } from '../store/useInverterStore';
import type { InverterSnapshot } from '../lib/types';
import type { EnergyFlowViewModel } from '../types/energyFlow';
import { buildEnergyFlows } from '../lib/energyFlow';
import { evcNodeLabel } from '../lib/evcLabel';

/** Live inputs every energy-flow renderer takes from the Status page. */
export interface EnergyFlowSourceProps {
  snapshot: InverterSnapshot;
  /** EV Charger active power in watts. 0 = not charging or no data. */
  evcPower?: number;
  /** Raw EV Charger charging-state string from HR 0 (issue #139). */
  evcChargingState?: string;
  /** Whether the EV Charger is actively charging. */
  evcCharging?: boolean;
  /** Whether the EV Charger is connected/responding. */
  evcConnected?: boolean;
  /** EV Charger physical cable plugged in (HR 2 `connection_status`). */
  evcCableConnected?: boolean;
  /**
   * EV Charger session energy (kWh) for the current/most-recent charge
   * session (issue #189). Counts up live while charging, then latches at
   * the final total after the session ends.
   */
  evcSessionEnergyKwh?: number;
  /** Latch: true once a valid EVC snapshot has ever arrived (issue #138). */
  evcEverConnected?: boolean;
  /** Whether EV Charger is configured (non-empty host). When falsy, EV node is hidden. */
  showEvc?: boolean;
}

export function useEnergyFlowViewModel({
  snapshot,
  evcPower = 0,
  evcChargingState = '',
  evcCharging = false,
  evcConnected = false,
  evcCableConnected = false,
  evcSessionEnergyKwh = 0,
  evcEverConnected,
  showEvc = false,
}: EnergyFlowSourceProps): EnergyFlowViewModel {
  const noise = useInverterStore((st) => st.visualNoiseThreshold);
  const gridMeterAddress = useInverterStore((st) => st.gridMeterAddress);

  const evcLabel = showEvc
    ? evcNodeLabel(evcCharging, evcConnected, !!evcEverConnected, evcChargingState)
    : undefined;
  // Cable indicator under the kW value (HR 2 `connection_status`). It's
  // only meaningful while we have a *fresh* frame — when the charger is
  // offline or never reached we genuinely can't see the cable, so we omit
  // the sub-label rather than assert a state. Gating on `evcConnected`
  // (network) keeps the last good cable value from lingering after a drop.
  const evcCableLabel =
    showEvc && evcConnected ? (evcCableConnected ? 'Cable In' : 'No Cable') : undefined;

  return buildEnergyFlows(snapshot, {
    noiseThresholdW: noise,
    evcPowerW: evcPower,
    showEvc,
    evcLabel,
    evcCableLabel,
    evcSessionEnergyKwh,
    gridMeterAddress,
  });
}
