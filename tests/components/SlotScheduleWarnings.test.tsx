import { describe, it, expect, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import SlotScheduleWarnings from '../../src/components/SlotScheduleWarnings';

afterEach(cleanup);

describe('<SlotScheduleWarnings/>', () => {
  it('explains the slot-label swap in one line, with the detail behind an ⓘ', () => {
    render(<SlotScheduleWarnings slotOrdering legacyFirmware={null} />);
    expect(screen.getByText('Slot Labels Differ from the GivEnergy Cloud')).toBeTruthy();
    expect(screen.getByText(/Our Slot 1 is the cloud.s Slot 2, and vice versa\./)).toBeTruthy();
    expect(screen.getByRole('button', { name: 'More about slot labels' })).toBeTruthy();
    expect(screen.getByText(/canonical Modbus register layout/).closest('[role="tooltip"]')).toBeTruthy();
    expect(screen.queryByText(/Older Gen3 Firmware/)).toBeNull();
  });

  it('warns about older Gen3 firmware with the version, slots and target SOCs', () => {
    render(<SlotScheduleWarnings slotOrdering={false} legacyFirmware="302" />);
    expect(screen.getByText('Older Gen3 Firmware Detected (ARM FW 302)')).toBeTruthy();
    expect(
      screen.getByText(
        'Slot 2 onwards and per-slot target SOCs may show stale or wrong values on this firmware. Firmware 303 or later fixes this.',
      ),
    ).toBeTruthy();
    expect(screen.queryByText(/Slot Labels Differ/)).toBeNull();
  });

  it('shows both together', () => {
    render(<SlotScheduleWarnings slotOrdering legacyFirmware="290" />);
    expect(screen.getByText('Slot Labels Differ from the GivEnergy Cloud')).toBeTruthy();
    expect(screen.getByText('Older Gen3 Firmware Detected (ARM FW 290)')).toBeTruthy();
  });

  it('renders nothing when neither applies', () => {
    const { container } = render(<SlotScheduleWarnings slotOrdering={false} legacyFirmware={null} />);
    expect(container.innerHTML).toBe('');
  });
});
