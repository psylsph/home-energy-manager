import { describe, it, expect, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, act } from '@testing-library/react';
import { MemoryRouter, Routes, Route, useNavigate, useLocation } from 'react-router';
import NavDrawer, { type NavDrawerItem } from '../../src/components/NavDrawer';

/**
 * Burger-menu drawer for the house view (issue #422): same pages as the
 * bottom bar, closes on selection / Escape / outside tap, traps focus, and
 * hands focus back to the button.
 */

const Icon = () => <svg data-testid="icon" />;
const ITEMS: NavDrawerItem[] = [
  { to: '/', label: 'Status', icon: Icon, accent: '#4F7DFF' },
  { to: '/battery', label: 'Battery', icon: Icon, accent: '#22C55E' },
  { to: '/settings', label: 'Settings', icon: Icon },
];

/** Stand-ins for a link elsewhere on the page and the browser's Back/Forward. */
function Navigator() {
  const navigate = useNavigate();
  return (
    <>
      <button type="button" onClick={() => void navigate('/battery')}>go-battery</button>
      <button type="button" onClick={() => void navigate(-1)}>go-back</button>
      <button type="button" onClick={() => void navigate(1)}>go-forward</button>
    </>
  );
}
const go = (name: string) => act(async () => { fireEvent.click(screen.getByRole('button', { name })); });
function Where() {
  return <div data-testid="where">{useLocation().pathname}</div>;
}

function renderDrawer(initial = '/') {
  return render(
    <MemoryRouter initialEntries={[initial]}>
      <NavDrawer items={ITEMS} />
      <Navigator />
      <Routes>
        <Route path="*" element={<Where />} />
      </Routes>
    </MemoryRouter>,
  );
}

const menuButton = () => screen.getByRole('button', { name: 'Menu' });
const dialog = () => screen.queryByRole('dialog', { name: 'Pages' });

describe('<NavDrawer/>', () => {
  afterEach(() => {
    cleanup();
    document.body.style.overflow = '';
  });

  it('starts closed, with the button collapsed', () => {
    renderDrawer();
    expect(dialog()).toBeNull();
    expect(menuButton().getAttribute('aria-expanded')).toBe('false');
  });

  it('opens with every page and highlights the current one', () => {
    renderDrawer('/battery');
    fireEvent.click(menuButton());
    expect(dialog()).not.toBeNull();
    expect(menuButton().getAttribute('aria-expanded')).toBe('true');
    expect(menuButton().getAttribute('aria-controls')).toBe(dialog()!.id);
    const links = screen.getAllByRole('link');
    expect(links.map((l) => l.textContent)).toEqual(['Status', 'Battery', 'Settings']);
    expect(screen.getByRole('link', { name: 'Battery' }).getAttribute('aria-current')).toBe('page');
    expect(screen.getByRole('link', { name: 'Status' }).getAttribute('aria-current')).toBeNull();
  });

  it('moves focus to the current page on open', () => {
    renderDrawer('/battery');
    fireEvent.click(menuButton());
    expect(document.activeElement).toBe(screen.getByRole('link', { name: 'Battery' }));
  });

  it('falls back to the first control when no page matches', () => {
    renderDrawer('/unknown');
    fireEvent.click(menuButton());
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close Menu' }));
  });

  it('navigates and closes on choosing a page, returning focus to the button', () => {
    renderDrawer('/');
    fireEvent.click(menuButton());
    fireEvent.click(screen.getByRole('link', { name: 'Settings' }));
    expect(screen.getByTestId('where').textContent).toBe('/settings');
    expect(dialog()).toBeNull();
    expect(document.activeElement).toBe(menuButton());
  });

  it('closes on Escape', () => {
    renderDrawer();
    fireEvent.click(menuButton());
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(dialog()).toBeNull();
    expect(document.activeElement).toBe(menuButton());
  });

  it('ignores other keys', () => {
    renderDrawer();
    fireEvent.click(menuButton());
    fireEvent.keyDown(document, { key: 'a' });
    expect(dialog()).not.toBeNull();
  });

  it('closes on a tap outside', () => {
    renderDrawer();
    fireEvent.click(menuButton());
    fireEvent.click(screen.getByTestId('nav-drawer-backdrop'));
    expect(dialog()).toBeNull();
  });

  it('closes from its close button and from the menu button', () => {
    renderDrawer();
    fireEvent.click(menuButton());
    fireEvent.click(screen.getByRole('button', { name: 'Close Menu' }));
    expect(dialog()).toBeNull();
    fireEvent.click(menuButton());
    fireEvent.click(menuButton());
    expect(dialog()).toBeNull();
  });

  it('keeps Tab and Shift+Tab inside the drawer', () => {
    renderDrawer('/');
    fireEvent.click(menuButton());
    const close = screen.getByRole('button', { name: 'Close Menu' });
    const last = screen.getByRole('link', { name: 'Settings' });

    last.focus();
    fireEvent.keyDown(dialog()!, { key: 'Tab' });
    expect(document.activeElement).toBe(close);

    close.focus();
    fireEvent.keyDown(dialog()!, { key: 'Tab', shiftKey: true });
    expect(document.activeElement).toBe(last);

    // Focus escaped somehow (e.g. a click on the page behind): Tab brings it back.
    menuButton().focus();
    fireEvent.keyDown(dialog()!, { key: 'Tab' });
    expect(document.activeElement).toBe(close);
    menuButton().focus();
    fireEvent.keyDown(dialog()!, { key: 'Tab', shiftKey: true });
    expect(document.activeElement).toBe(last);
  });

  it('leaves Tab alone between the first and last controls', () => {
    renderDrawer('/');
    fireEvent.click(menuButton());
    const middle = screen.getByRole('link', { name: 'Battery' });
    middle.focus();
    const ev = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
    dialog()!.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(false);
    fireEvent.keyDown(dialog()!, { key: 'Enter' });
    expect(dialog()).not.toBeNull();
  });

  it('locks page scroll while open and restores it on close', () => {
    document.body.style.overflow = 'scroll';
    renderDrawer();
    fireEvent.click(menuButton());
    expect(document.body.style.overflow).toBe('hidden');
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(document.body.style.overflow).toBe('scroll');
  });

  it('closes on Back and stays closed on returning to that page', async () => {
    renderDrawer('/');
    await go('go-battery');
    fireEvent.click(menuButton());
    expect(dialog()).not.toBeNull();

    await go('go-back');
    expect(screen.getByTestId('where').textContent).toBe('/');
    expect(dialog()).toBeNull();

    await go('go-forward');
    expect(screen.getByTestId('where').textContent).toBe('/battery');
    expect(dialog()).toBeNull();
  });

  it('applies each page accent to its active link', () => {
    renderDrawer('/battery');
    fireEvent.click(menuButton());
    const battery = screen.getByRole('link', { name: 'Battery' });
    expect(battery.style.getPropertyValue('--nav-accent')).toBe('#22C55E');
    expect(battery.className).toContain('text-[var(--nav-accent)]');
    fireEvent.click(menuButton());
    cleanup();
    renderDrawer('/settings');
    fireEvent.click(menuButton());
    expect(screen.getByRole('link', { name: 'Settings' }).className).toContain('text-accent');
  });
});
