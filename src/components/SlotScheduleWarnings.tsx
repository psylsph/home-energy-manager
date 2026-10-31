import HelpTip from './HelpTip';

const BANNER = 'rounded-xl border border-yellow-500/30 bg-yellow-500/10 p-3 text-xs text-text-primary';

/**
 * Warnings shown above slot 2 of the Charge Schedule and Timed Export
 * sections (issue #41), so both schedules say the same thing.
 *
 * - `slotOrdering`: this app labels slots as the givenergy-modbus reference
 *   library does, the reverse of the GivEnergy cloud UI.
 * - `legacyFirmware`: the ARM firmware version when it is 302 or older, where
 *   the extended schedule registers (slots 2+ and per-slot target SOCs) may
 *   read back stale; null otherwise.
 */
export default function SlotScheduleWarnings({
  slotOrdering,
  legacyFirmware,
}: {
  slotOrdering: boolean;
  legacyFirmware: string | null;
}) {
  if (!slotOrdering && legacyFirmware === null) return null;
  return (
    <div className="space-y-2">
      {slotOrdering && (
        <div className={BANNER}>
          <div className="font-semibold mb-1 flex items-center gap-1.5">
            <span>Slot Labels Differ from the GivEnergy Cloud</span>
            <HelpTip label="slot labels">
              This app uses the canonical Modbus register layout from the{' '}
              <code>givenergy-modbus</code> reference library, which labels discharge slots in the
              opposite order to the GivEnergy cloud UI: our <strong>Slot 1</strong> is the
              cloud&apos;s <strong>Slot 2</strong> and vice versa. The underlying schedule data is
              identical — only the labels differ.
            </HelpTip>
          </div>
          Our Slot 1 is the cloud&apos;s Slot 2, and vice versa.
        </div>
      )}
      {legacyFirmware !== null && (
        <div className={BANNER}>
          <div className="font-semibold mb-1">Older Gen3 Firmware Detected (ARM FW {legacyFirmware})</div>
          Slot 2 onwards and per-slot target SOCs may show stale or wrong values on this firmware.
          Firmware 303 or later fixes this.
        </div>
      )}
    </div>
  );
}
