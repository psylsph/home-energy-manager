import { describe, it, expect, afterEach, vi } from 'vitest';
import { act, render, screen, cleanup, fireEvent } from '@testing-library/react';
import HelpTip from '../../src/components/HelpTip';
import { SettingsGroup, SettingsRow, SettingsSection } from '../../src/components/settings/SettingsSection';

afterEach(cleanup);

function renderTip() {
  render(
    <div>
      <HelpTip label="Join Sessions for Me">HEM joins each session for you.</HelpTip>
      <button type="button">Elsewhere</button>
    </div>,
  );
  return screen.getByRole('button', { name: 'More about Join Sessions for Me' });
}

const tip = () => screen.getByText('HEM joins each session for you.');

describe('<HelpTip/>', () => {
  it('has an accessible name and describes itself with the help text', () => {
    const button = renderTip();
    expect(button.getAttribute('aria-expanded')).toBe('false');
    expect(tip().getAttribute('role')).toBe('tooltip');
    expect(button.getAttribute('aria-describedby')).toBe(tip().id);
  });

  it('keeps the text in the page while closed, hidden', () => {
    renderTip();
    expect(tip().hidden).toBe(true);
    expect(screen.queryByRole('tooltip')).toBeNull();
  });

  it('opens on mouse hover and closes shortly after the mouse leaves', () => {
    vi.useFakeTimers();
    try {
      const button = renderTip();
      fireEvent.pointerEnter(button, { pointerType: 'mouse' });
      expect(screen.getByRole('tooltip').textContent).toBe('HEM joins each session for you.');
      expect(button.getAttribute('aria-expanded')).toBe('true');
      fireEvent.pointerLeave(button, { pointerType: 'mouse' });
      act(() => vi.advanceTimersByTime(250));
      expect(tip().hidden).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it('does not open on a touch pointer entering, only on the tap itself', () => {
    const button = renderTip();
    fireEvent.pointerEnter(button, { pointerType: 'touch' });
    expect(tip().hidden).toBe(true);
    fireEvent.click(button);
    expect(tip().hidden).toBe(false);
  });

  it('opens on keyboard focus and closes when focus moves on', () => {
    const button = renderTip();
    act(() => button.focus());
    expect(tip().hidden).toBe(false);
    act(() => screen.getByRole('button', { name: 'Elsewhere' }).focus());
    expect(tip().hidden).toBe(true);
  });

  it('closes on Escape', () => {
    const button = renderTip();
    act(() => button.focus());
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(tip().hidden).toBe(true);
  });

  it('toggles on tap, and a tap elsewhere closes it', () => {
    const button = renderTip();
    fireEvent.click(button);
    expect(tip().hidden).toBe(false);
    fireEvent.click(button);
    expect(tip().hidden).toBe(true);
    fireEvent.click(button);
    fireEvent.pointerDown(document.body);
    expect(tip().hidden).toBe(true);
  });

  it('stays open after a click even when the mouse leaves', () => {
    const button = renderTip();
    fireEvent.pointerEnter(button, { pointerType: 'mouse' });
    fireEvent.click(button);
    fireEvent.pointerLeave(button, { pointerType: 'mouse' });
    expect(tip().hidden).toBe(false);
  });

  it('stays open when focus moves to a control inside the tip', () => {
    render(
      <HelpTip label="Setup">
        See <a href="#guide">the guide</a>.
      </HelpTip>,
    );
    const button = screen.getByRole('button', { name: 'More about Setup' });
    act(() => button.focus());
    act(() => (screen.getByText('the guide') as HTMLElement).focus());
    expect(screen.getByRole('tooltip')).toBeTruthy();
  });

  it('stays open when a click on its text takes focus off the button', () => {
    // A click on plain text moves focus to the body: relatedTarget is null.
    const button = renderTip();
    act(() => button.focus());
    fireEvent.click(button);
    fireEvent.pointerDown(tip());
    act(() => button.blur());
    expect(tip().hidden).toBe(false);
  });

  it('stays open while the mouse crosses from the icon onto the tip', () => {
    vi.useFakeTimers();
    try {
      const button = renderTip();
      fireEvent.pointerEnter(button, { pointerType: 'mouse' });
      fireEvent.pointerLeave(button, { pointerType: 'mouse' });
      act(() => vi.advanceTimersByTime(50));
      fireEvent.pointerEnter(tip(), { pointerType: 'mouse' });
      act(() => vi.advanceTimersByTime(1000));
      expect(tip().hidden).toBe(false);
      fireEvent.pointerLeave(tip(), { pointerType: 'mouse' });
      act(() => vi.advanceTimersByTime(1000));
      expect(tip().hidden).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it('a mouse passing over a keyboard-focused tip does not close it', () => {
    vi.useFakeTimers();
    try {
      const button = renderTip();
      act(() => button.focus());
      fireEvent.pointerEnter(button, { pointerType: 'mouse' });
      fireEvent.pointerLeave(button, { pointerType: 'mouse' });
      act(() => vi.advanceTimersByTime(1000));
      expect(tip().hidden).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it('follows the icon when the page scrolls or resizes instead of closing', () => {
    const button = renderTip();
    let top = 100;
    button.getBoundingClientRect = () =>
      ({ top, bottom: top + 16, left: 50, right: 66, width: 16, height: 16, x: 50, y: top, toJSON: () => ({}) }) as DOMRect;
    fireEvent.click(button);
    expect(tip().style.top).toBe('122px');
    top = 40;
    fireEvent.scroll(window);
    expect(tip().hidden).toBe(false);
    expect(tip().style.top).toBe('62px');
    top = 10;
    fireEvent(window, new Event('resize'));
    expect(tip().style.top).toBe('32px');
  });

  it('a tap on the tip itself does not close it', () => {
    const button = renderTip();
    fireEvent.click(button);
    fireEvent.pointerDown(tip());
    expect(tip().hidden).toBe(false);
  });

  it('does not toggle a surrounding label or row', () => {
    let clicks = 0;
    render(
      <label onClick={() => clicks++}>
        Setting <HelpTip label="Setting">Detail</HelpTip>
      </label>,
    );
    fireEvent.click(screen.getByRole('button', { name: 'More about Setting' }));
    expect(clicks).toBe(0);
  });
});

describe('settings help', () => {
  it('puts an ⓘ after section, group and row titles when given help', () => {
    render(
      <SettingsSection title="Weather" description="Outdoor temperature." help="Weather detail.">
        <SettingsGroup title="Backups" description="Copies of history." help="Group detail." />
        <SettingsRow label="Start on Login" hint="Opens with your desktop." help="Row detail.">
          <span />
        </SettingsRow>
      </SettingsSection>,
    );
    expect(screen.getByRole('heading', { name: /Weather/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'More about Weather' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'More about Backups' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'More about Start on Login' })).toBeTruthy();
    expect(screen.getByText('Outdoor temperature.')).toBeTruthy();
    expect(screen.getByText('Opens with your desktop.')).toBeTruthy();
    expect(screen.getByText('Row detail.')).toBeTruthy();
  });

  it('names the ⓘ from helpLabel when the title is not plain text', () => {
    render(
      <SettingsGroup title={<span>Telegram</span>} helpLabel="Telegram" help="Detail." />,
    );
    expect(screen.getByRole('button', { name: 'More about Telegram' })).toBeTruthy();
  });

  it('adds no button without help', () => {
    render(<SettingsGroup title="Plain" description="Short." />);
    expect(screen.queryByRole('button')).toBeNull();
  });
});
