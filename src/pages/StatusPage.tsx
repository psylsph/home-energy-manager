import { useInverterStore } from '../store/useInverterStore';
import EnergyFlowDiagram from '../components/EnergyFlowDiagram';
import BatteryPanel from '../components/BatteryPanel';
import SummaryTiles from '../components/SummaryTiles';

export default function StatusPage() {
  const { snapshot, connectionState } = useInverterStore();

  if (!snapshot) {
    return (
      <div className="flex flex-col items-center justify-center min-h-[60vh] gap-4">
        <div className="w-10 h-10 border-4 border-flow-active border-t-transparent rounded-full animate-spin" />
        <p className="text-text-secondary text-sm font-sans">
          Waiting for data{connectionState === 'reconnecting' ? ' — reconnecting…' : ''}
        </p>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-6 max-w-5xl mx-auto px-4 py-6">
      {/* Top: two-column on desktop, stacked on mobile */}
      <div className="grid grid-cols-1 md:grid-cols-[1fr_320px] gap-6">
        {/* Left: Energy flow diagram */}
        <div className="bg-bg-surface rounded-xl p-4">
          <EnergyFlowDiagram snapshot={snapshot} />
        </div>

        {/* Right: Battery panel */}
        <BatteryPanel snapshot={snapshot} />
      </div>

      {/* Bottom: Summary tiles */}
      <SummaryTiles snapshot={snapshot} />
    </div>
  );
}
