import { describe, expect, it } from 'vitest';
import { fillScheduleSlots } from '../../src/lib/scheduleSlots';
import type { ScheduleSlot } from '../../src/lib/types';

const disabled: ScheduleSlot = {
  enabled: false,
  start_hour: 16,
  start_minute: 0,
  end_hour: 19,
  end_minute: 0,
  target_soc: 4,
};

describe('fillScheduleSlots', () => {
  it('preserves returned slots and pads only the missing capability slots', () => {
    const configured: ScheduleSlot = { ...disabled, enabled: true, target_soc: 80 };

    expect(fillScheduleSlots([configured], 3, disabled)).toEqual([
      configured,
      disabled,
      disabled,
    ]);
  });

  it('truncates data that exceeds the capability count', () => {
    const slots = [disabled, disabled, disabled];

    expect(fillScheduleSlots(slots, 2, disabled)).toEqual([disabled, disabled]);
  });

  it('creates a fresh default slot for every missing position', () => {
    const result = fillScheduleSlots(undefined, 2, disabled);

    expect(result).toHaveLength(2);
    expect(result[0]).not.toBe(result[1]);
    result[0]!.enabled = true;
    expect(result[1]!.enabled).toBe(false);
  });
});
