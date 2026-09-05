import type { ScheduleSlot } from './types';

/**
 * Keep the slots returned by an older/partial backend and fill only the
 * positions it did not provide up to the device capability.
 */
export function fillScheduleSlots(
  slots: ScheduleSlot[] | undefined,
  capability: number,
  defaultSlot: ScheduleSlot,
): ScheduleSlot[] {
  const returned = (slots ?? []).slice(0, capability);
  return returned.concat(
    Array.from(
      { length: Math.max(0, capability - returned.length) },
      () => ({ ...defaultSlot }),
    ),
  );
}
