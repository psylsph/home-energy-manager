import { useState } from 'react';
import { apiPost } from '../../lib/api';
import { useInverterStore } from '../../store/useInverterStore';
import Toggle from '../Toggle';
import { AdvancedSettings, SettingsGroup, SettingsSection } from './SettingsSection';
import { useSettingsSave, type Flash } from './useSettingsSave';
import HelpTip from '../HelpTip';

const BASE_PANELS = [
  ['power', 'Power'],
  ['battery', 'Battery'],
  ['solar', 'Solar'],
  ['meters', 'Meters'],
  ['history', 'History'],
  ['forecast', 'Planner'],
] as const;

const TOGGLE_ROW = 'flex items-center justify-between';
const LABEL = 'text-text-primary text-sm font-sans';
const HINT = 'text-text-secondary text-xs font-sans';

/** Settings → Panel Controls: which tabs show, and how charts and the flow diagram look. */
export default function PanelControlsSection({
  id,
  flash,
  initialHiddenPanels,
  octopusConfigured,
}: {
  id?: string;
  flash: Flash;
  initialHiddenPanels: string[];
  /** The Octopus tab exists only once an account number and key are saved. */
  octopusConfigured: boolean;
}) {
  const [hiddenPanels, setHiddenPanels] = useState<string[]>(initialHiddenPanels);
  const { busy: panelSaving, run } = useSettingsSave(flash);

  const panels = [
    ...BASE_PANELS,
    ...(octopusConfigured ? [['octopus', 'Octopus'] as const] : []),
    ['control', 'Control'] as const,
  ];

  const handlePanelSave = () =>
    run(async () => {
      await apiPost('/api/settings', { hidden_panels: hiddenPanels });
      // Push to the store so App.tsx picks it up without a restart.
      useInverterStore.getState().setHiddenPanels(hiddenPanels);
      return 'Panel visibility saved';
    }, () => 'Failed to save panel visibility');

  return (
    <SettingsSection id={id} title="Panel Controls">
      <SettingsGroup title="Panel Visibility" description="Hide panels you don't use from the bottom navigation bar">
        <div className="grid grid-cols-2 sm:grid-cols-3 gap-3">
          {panels.map(([key, label]) => (
            <label key={key} className="flex items-center gap-2 cursor-pointer select-none bg-bg-elevated rounded-xl px-4 py-3 border border-white/5 hover:border-white/10 transition-colors">
              <input
                type="checkbox"
                checked={!hiddenPanels.includes(key)}
                onChange={() => {
                  setHiddenPanels((prev) =>
                    prev.includes(key) ? prev.filter((p) => p !== key) : [...prev, key],
                  );
                }}
                className="w-4 h-4 accent-accent rounded"
              />
              <span className={LABEL}>{label}</span>
            </label>
          ))}
        </div>
        <button
          onClick={() => void handlePanelSave()}
          className="self-start bg-accent text-on-accent font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 transition-opacity"
        >
          {panelSaving ? 'Saving…' : 'Save Panel Visibility'}
        </button>
      </SettingsGroup>

      <PanelGraphsGroup />
      <HistoryAveragesGroup />
      <EnergyFlowGroup />
      <AdvancedSettings>
        <GridLinesGroup />
      </AdvancedSettings>
    </SettingsSection>
  );
}

function PanelGraphsGroup() {
  const panelGraphsEnabled = useInverterStore((s) => s.panelGraphsEnabled);
  const setPanelGraphsEnabled = useInverterStore((s) => s.setPanelGraphsEnabled);
  const panelGraphsScale = useInverterStore((s) => s.panelGraphsScale);
  const setPanelGraphsScale = useInverterStore((s) => s.setPanelGraphsScale);
  const panelGraphsYLock = useInverterStore((s) => s.panelGraphsYLock);
  const setPanelGraphsYLock = useInverterStore((s) => s.setPanelGraphsYLock);
  return (
    <SettingsGroup
      title="Panel Graphs"
      description="Toggle the trend charts on the Battery and Solar tabs, and choose their time scale"
    >
      <div className={TOGGLE_ROW}>
        <span className={LABEL}>Show Graphs</span>
        <Toggle checked={panelGraphsEnabled} ariaLabel="Show Graphs" onChange={setPanelGraphsEnabled} />
      </div>

      {/* Time scale selector — disabled when graphs are off */}
      <div className={`flex flex-col gap-2 transition-opacity ${panelGraphsEnabled ? '' : 'opacity-40 pointer-events-none'}`}>
        <span className={HINT}>Time Scale</span>
        <div className="flex gap-2">
          {([
            ['today', 'Today'],
            ['24h', 'Rolling 24H'],
          ] as const).map(([key, label]) => (
            <button
              key={key}
              type="button"
              onClick={() => setPanelGraphsScale(key)}
              className={`flex-1 py-2 rounded-lg text-sm font-sans transition ${
                panelGraphsScale === key
                  ? 'bg-accent text-on-accent font-semibold'
                  : 'bg-bg-elevated text-text-primary hover:bg-bg-elevated/80'
              }`}
            >
              {label}
            </button>
          ))}
        </div>
      </div>

      <div className={TOGGLE_ROW}>
        <span className={LABEL}>Lock Y-Axis Scale</span>
        <Toggle checked={panelGraphsYLock} ariaLabel="Lock Y-Axis Scale" onChange={setPanelGraphsYLock} />
      </div>
      {panelGraphsYLock && (
        <p className={HINT}>
          Charts Y-axis locks to a clean ceiling based on the data maximum instead of auto-fitting
        </p>
      )}
    </SettingsGroup>
  );
}

/**
 * Two-weight control for the recharts `CartesianGrid` on the Power, History,
 * Battery, Solar, and Forecast charts. Issue #111: the default 2-px dashed
 * grid competes with the data series; users bothered by that can drop to a
 * hairline. Defaults to 'standard'.
 */
function GridLinesGroup() {
  const gridLineWeight = useInverterStore((s) => s.gridLineWeight);
  const setGridLineWeight = useInverterStore((s) => s.setGridLineWeight);
  return (
    <SettingsGroup
      title="Chart Grid Lines"
      description={
        <>
          Grid line weight on the Power, History, Battery, Solar, and Forecast charts.
          Standard matches the data series thickness; subtle is a hairline that sits behind it.
        </>
      }
    >
      <div className="flex gap-2">
        {([
          ['standard', 'Standard'],
          ['subtle', 'Subtle'],
        ] as const).map(([key, label]) => {
          const selected = gridLineWeight === key;
          // The swatch mirrors HISTORY_CHART_GRID_PRESETS in
          // src/lib/historyRangeConfig.ts so the user previews each weight.
          const dasharray = key === 'standard' ? '4 4' : '3 4';
          const swatchColor = key === 'standard'
            ? 'var(--color-grid-stroke)'
            : 'var(--color-grid-stroke-subtle)';
          const swatchWidth = key === 'standard' ? 2 : 1;
          return (
            <button
              key={key}
              type="button"
              onClick={() => setGridLineWeight(key)}
              aria-pressed={selected}
              className={`flex-1 py-2 rounded-lg text-sm font-sans transition flex flex-col items-center gap-2 ${
                selected
                  ? 'bg-accent text-on-accent font-semibold'
                  : 'bg-bg-elevated text-text-primary hover:bg-bg-elevated/80'
              }`}
            >
              <span>{label}</span>
              <svg width="48" height="8" viewBox="0 0 48 8" aria-hidden="true" className="opacity-80">
                <line
                  x1="0" y1="4" x2="48" y2="4"
                  stroke={swatchColor}
                  strokeWidth={swatchWidth}
                  strokeDasharray={dasharray}
                />
              </svg>
            </button>
          );
        })}
      </div>
    </SettingsGroup>
  );
}

/**
 * A fixed horizontal average line per series on the History graphs (issue
 * #345). Cumulative graphs are left out: the average of a running total
 * isn't a useful number. Off by default; persisted via the store.
 */
function HistoryAveragesGroup() {
  const showHistoryAverages = useInverterStore((s) => s.showHistoryAverages);
  const setShowHistoryAverages = useInverterStore((s) => s.setShowHistoryAverages);
  return (
    <SettingsGroup
      title="History Chart Averages"
      description="A line on each History graph at the series' average for the period."
      help="Draw a horizontal line on each History graph showing the average of that series over the selected period. Daily energy and cost graphs are left out, because the average of a running total doesn't tell you much."
    >
      <div className={TOGGLE_ROW}>
        <span className={LABEL}>Show Average Lines</span>
        <Toggle
          checked={showHistoryAverages}
          onChange={setShowHistoryAverages}
          ariaLabel="Show Average Lines on History Charts"
        />
      </div>
    </SettingsGroup>
  );
}

function EnergyFlowGroup() {
  const showFlowStatusWords = useInverterStore((s) => s.showFlowStatusWords);
  const setShowFlowStatusWords = useInverterStore((s) => s.setShowFlowStatusWords);
  const visualNoiseThreshold = useInverterStore((s) => s.visualNoiseThreshold);
  const setVisualNoiseThreshold = useInverterStore((s) => s.setVisualNoiseThreshold);
  const gridMeterAddress = useInverterStore((s) => s.gridMeterAddress);
  const setGridMeterAddress = useInverterStore((s) => s.setGridMeterAddress);
  const energyFlowDisplay = useInverterStore((s) => s.energyFlowDisplay);
  const setEnergyFlowDisplay = useInverterStore((s) => s.setEnergyFlowDisplay);
  // External CT clamps (address >= 0x01) — offered as the grid-CT source for
  // AC-coupled systems whose grid isn't the built-in 0x00 meter. The meter
  // list is replaced each snapshot, so only its addresses are taken (as a
  // stable string) to avoid re-rendering on every poll.
  const externalAddresses = useInverterStore((s) =>
    (s.snapshot?.meters ?? []).filter((m) => m.address >= 1).map((m) => m.address).join(','),
  );
  const externalMeters = externalAddresses ? externalAddresses.split(',').map((a) => ({ address: Number(a) })) : [];
  return (
    <SettingsGroup title="Energy Flow Diagram">
      {/* Issue #422: the orbit stays the default; the house view also swaps
          the bottom navigation bar for a burger menu. */}
      <div className="flex flex-col gap-2">
        <div className="flex flex-col gap-0.5">
          {/* The ⓘ sits outside the labelling span so the group's
              accessible name stays the title. */}
          <span className={LABEL}>
            <span id="energy-flow-display-label">Energy Flow Display</span>{' '}
            <HelpTip label="Energy Flow Display">
              Orbit is the radial diagram. House shows the same readings around an illustration of a house, and
              replaces the page bar along the bottom of the screen with a menu button in the top-left corner.
            </HelpTip>
          </span>
          <span className={HINT}>How the Status page draws your energy flows.</span>
        </div>
        <div className="flex gap-2" role="group" aria-labelledby="energy-flow-display-label">
          {([
            ['orbit', 'Orbit'],
            ['house', 'House'],
          ] as const).map(([key, label]) => {
            const selected = energyFlowDisplay === key;
            return (
              <button
                key={key}
                type="button"
                onClick={() => setEnergyFlowDisplay(key)}
                aria-pressed={selected}
                className={`flex-1 py-2 rounded-lg text-sm font-sans transition ${
                  selected
                    ? 'bg-accent text-on-accent font-semibold'
                    : 'bg-bg-elevated text-text-primary hover:bg-bg-elevated/80'
                }`}
              >
                {label}
              </button>
            );
          })}
        </div>
      </div>

      {/* The house view always shows its status pills, so the toggle only
          applies to the orbit. */}
      {energyFlowDisplay === 'orbit' && (
        <div className="flex items-center justify-between gap-3">
          <div className="flex flex-col gap-0.5">
            <span className={LABEL}>Show Node Status Words</span>
            <span className={HINT}>
              Show short words like Generating, Importing, and Charging under the orbit nodes.
            </span>
          </div>
          <Toggle checked={showFlowStatusWords} ariaLabel="Show Node Status Words" onChange={setShowFlowStatusWords} />
        </div>
      )}

      {/* Grid CT meter source (issue #192): which CT clamp measures the grid
          point, so the wheel can show grid amps instead of frequency. "Auto"
          picks the built-in 0x00 grid CT or, without one, the lowest external
          clamp. Shown only when external CTs exist. */}
      {externalMeters.length > 0 && (
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between sm:gap-3">
          <div className="flex flex-col gap-0.5 sm:max-w-[60%]">
            <span className={LABEL}>
              Grid CT Meter{' '}
              <HelpTip label="Grid CT Meter">
                Which CT clamp measures your grid point. “Auto” uses the built-in grid CT when present, otherwise meter
                0x01. Used to show grid amps instead of frequency on the energy wheel.
              </HelpTip>
            </span>
            <span className={HINT}>The CT clamp at your grid point, for grid amps on the wheel.</span>
          </div>
          <select
            value={gridMeterAddress}
            onChange={(e) => setGridMeterAddress(Number(e.target.value))}
            aria-label="Grid CT Meter"
            data-testid="grid-ct-meter-select"
            className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-accent outline-none transition-colors w-full sm:w-auto"
          >
            <option value={0}>Auto (Recommended)</option>
            {externalMeters.map((m) => (
              <option key={m.address} value={m.address}>
                Meter 0x{m.address.toString(16).padStart(2, '0')}
              </option>
            ))}
            {/* An offline configured meter keeps its option so the select
                stays controlled instead of snapping to the default. */}
            {gridMeterAddress !== 0 &&
              !externalMeters.some((m) => m.address === gridMeterAddress) && (
              <option value={gridMeterAddress}>
                Meter 0x{gridMeterAddress.toString(16).padStart(2, '0')} (offline)
              </option>
            )}
          </select>
        </div>
      )}

      <p className={HINT}>
        Flows below this wattage show as zero.{' '}
        <HelpTip label="the flow threshold">
          Flows below this wattage are treated as zero — no animated line, no arrow, displayed value rounds to 0W.
          Prevents tiny readings from cluttering the diagram.
        </HelpTip>
      </p>
      <div className="flex items-center gap-3">
        <input
          type="number"
          min={0}
          max={100}
          step={5}
          value={visualNoiseThreshold}
          onChange={(e) => {
            const v = parseInt(e.target.value, 10);
            if (!Number.isFinite(v)) return;
            setVisualNoiseThreshold(Math.max(0, Math.min(100, v)));
          }}
          className="w-20 bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-accent outline-none transition-colors text-center"
        />
        <span className="text-text-secondary text-sm font-sans">Watts</span>
      </div>
      <div className="flex items-center gap-2">
        <input
          type="range"
          min={0}
          max={100}
          step={5}
          value={visualNoiseThreshold}
          onChange={(e) => setVisualNoiseThreshold(Number(e.target.value))}
          className="w-full"
        />
        <span className="text-text-secondary text-xs font-sans w-8 text-right tabular-nums">{visualNoiseThreshold}W</span>
      </div>
    </SettingsGroup>
  );
}
