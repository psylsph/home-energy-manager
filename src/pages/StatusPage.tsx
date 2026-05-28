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
    <div className="flex flex-col gap-6 max-w-4xl mx-auto">
      {/* Energy flow diagram — full width card */}
      <section className="bg-bg-surface rounded-2xl p-6">
        <EnergyFlowDiagram snapshot={snapshot} />
      </section>

      {/* Battery + Summary side by side on md+ */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
        <BatteryPanel snapshot={snapshot} />
        <SummaryTiles snapshot={snapshot} />
      </div>
    </div>
  );
}
