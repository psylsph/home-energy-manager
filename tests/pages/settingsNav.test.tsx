import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, within, act } from '@testing-library/react';
import SettingsNav from '../../src/components/settings/SettingsNav';
import type { SettingsNavGroup } from '../../src/lib/settingsNav';

const GROUPS: SettingsNavGroup[] = [
  { label: 'Connection', sections: [{ id: 's-inverter', label: 'Inverter' }, { id: 's-evc', label: 'EV Charger' }] },
  { label: 'Alerts', sections: [{ id: 's-notify', label: 'Notifications' }] },
  { label: 'App', sections: [{ id: 's-app', label: 'App Behaviour' }, { id: 's-about', label: 'About' }] },
];
const IDS = GROUPS.flatMap((g) => g.sections.map((s) => s.id));

/** Section viewport tops, by id; the test moves them to "scroll". */
let tops: Record<string, number>;
/** The Settings page's own top (it sits below any banners). */
let pageTop: number;
const scrollIntoView = vi.fn();

function renderPage() {
  render(
    <div data-testid="page">
      <SettingsNav groups={GROUPS} />
      {IDS.map((id) => <section key={id} id={id}>{id}</section>)}
    </div>,
  );
  for (const id of IDS) {
    const el = document.getElementById(id)!;
    el.getBoundingClientRect = () => ({ top: tops[id] ?? 0 } as DOMRect);
    el.scrollIntoView = scrollIntoView;
  }
  screen.getByTestId('page').getBoundingClientRect = () => ({ top: pageTop } as DOMRect);
}

/** Fire a window scroll and let the animation frame run. */
async function scroll(atBottom = false) {
  const root = document.scrollingElement ?? document.documentElement;
  Object.defineProperty(root, 'scrollHeight', { value: 5000, configurable: true });
  Object.defineProperty(root, 'clientHeight', { value: 800, configurable: true });
  root.scrollTop = atBottom ? 4200 : 1000;
  await act(async () => {
    fireEvent.scroll(window);
    await new Promise((r) => requestAnimationFrame(() => r(undefined)));
  });
}

const rail = () => screen.getByRole('navigation', { name: 'Settings' });
const tabs = () => screen.getByRole('navigation', { name: 'Settings sections' });
const railItem = (name: string) => within(rail()).getByRole('button', { name });
const currentLink = () => within(rail()).getAllByRole('button').find((a) => a.getAttribute('aria-current'));
const currentTab = () => within(tabs()).getAllByRole('button').find((b) => b.getAttribute('aria-current'));

beforeEach(() => {
  tops = { 's-inverter': 0, 's-evc': 600, 's-notify': 1200, 's-app': 1800, 's-about': 2400 };
  pageTop = 0;
  scrollIntoView.mockClear();
  Object.defineProperty(window, 'innerHeight', { value: 800, configurable: true });
});

afterEach(cleanup);

describe('<SettingsNav/>', () => {
  it('lists every section under its group in the rail, and the groups as tabs', () => {
    renderPage();
    expect(within(rail()).getAllByRole('button').map((a) => a.textContent)).toEqual(
      ['Inverter', 'EV Charger', 'Notifications', 'App Behaviour', 'About'],
    );
    expect(within(rail()).getByText('Alerts')).toBeDefined();
    expect(within(tabs()).getAllByRole('button').map((b) => b.textContent)).toEqual(['Connection', 'Alerts', 'App']);
    // Buttons, not `#id` links: under the hash router a link opened in a
    // new tab would name a route that doesn't exist.
    expect(rail().querySelector('a')).toBeNull();
  });

  it('starts on the first section', () => {
    renderPage();
    expect(currentLink()?.textContent).toBe('Inverter');
    expect(currentTab()?.textContent).toBe('Connection');
  });

  it('jumps to a section from the rail and marks it current', () => {
    renderPage();
    fireEvent.click(railItem('Notifications'));
    expect(scrollIntoView).toHaveBeenCalledWith({ behavior: 'smooth', block: 'start' });
    expect(scrollIntoView.mock.contexts[0]).toBe(document.getElementById('s-notify'));
    expect(currentLink()?.textContent).toBe('Notifications');
    expect(currentTab()?.textContent).toBe('Alerts');
  });

  it("jumps to a group's first section from its tab", () => {
    renderPage();
    fireEvent.click(within(tabs()).getByRole('button', { name: 'App' }));
    expect(scrollIntoView.mock.contexts[0]).toBe(document.getElementById('s-app'));
    expect(currentLink()?.textContent).toBe('App Behaviour');
  });

  it('follows the section the reader has scrolled to', async () => {
    renderPage();
    tops = { 's-inverter': -1300, 's-evc': -700, 's-notify': -100, 's-app': 500, 's-about': 1100 };
    await scroll();
    expect(currentLink()?.textContent).toBe('Notifications');
    expect(currentTab()?.textContent).toBe('Alerts');
  });

  it('marks the last section once the page is scrolled to the bottom', async () => {
    renderPage();
    tops = { 's-inverter': -3000, 's-evc': -2400, 's-notify': -1800, 's-app': 100, 's-about': 500 };
    await scroll(true);
    expect(currentLink()?.textContent).toBe('About');
  });

  it('keeps a clicked short section current while the scroll travels there and while it is on screen, then lets go', async () => {
    renderPage();
    fireEvent.click(railItem('App Behaviour'));
    // Mid smooth-scroll: the section is still below the window, and by
    // position the reader is passing EV Charger.
    tops = { 's-inverter': -700, 's-evc': -100, 's-notify': 500, 's-app': 1500, 's-about': 2100 };
    await scroll();
    expect(currentLink()?.textContent).toBe('App Behaviour');

    // The page can't scroll far enough to bring it to the top.
    tops = { 's-inverter': -3000, 's-evc': -2400, 's-notify': -1800, 's-app': 100, 's-about': 500 };
    await scroll(true);
    expect(currentLink()?.textContent).toBe('App Behaviour');

    // Once the reader scrolls it off screen, normal tracking resumes.
    tops = { 's-inverter': -500, 's-evc': 100, 's-notify': 700, 's-app': 1300, 's-about': 1900 };
    await scroll();
    expect(currentLink()?.textContent).toBe('EV Charger');
  });

  it('lets go of a jump whose section never comes on screen', async () => {
    const now = vi.spyOn(performance, 'now').mockReturnValue(1000);
    try {
      renderPage();
      fireEvent.click(railItem('About'));
      // The reader scrolled elsewhere before the jump arrived.
      now.mockReturnValue(1000 + 2500);
      tops = { 's-inverter': -700, 's-evc': -100, 's-notify': 500, 's-app': 1500, 's-about': 2100 };
      await scroll();
      expect(currentLink()?.textContent).toBe('EV Charger');
    } finally {
      now.mockRestore();
    }
  });

  it('fits every group tab in one row on narrow screens, with no sideways scrolling', () => {
    renderPage();
    expect(tabs().style.gridTemplateColumns).toBe('repeat(3, minmax(0, 1fr))');
    expect(tabs().className).not.toMatch(/overflow-x/);
  });

  it('pins both navigations just below the app header, whatever its height', () => {
    const header = document.createElement('header');
    header.getBoundingClientRect = () => ({ bottom: 73.4 } as DOMRect);
    document.body.appendChild(header);
    try {
      renderPage();
      expect(tabs().style.top).toBe('73px');
      const list = rail().firstElementChild as HTMLElement;
      expect(list.className).toMatch(/\bfixed\b/);
      expect(list.style.top).toBe('97px');
    } finally {
      header.remove();
    }
  });

  it('starts below the banners above the page, and pins under the header once they scroll away', async () => {
    const header = document.createElement('header');
    header.getBoundingClientRect = () => ({ bottom: 73 } as DOMRect);
    document.body.appendChild(header);
    try {
      renderPage();
      // An update banner pushes the page down to 200px.
      pageTop = 200;
      await scroll();
      expect(tabs().style.top).toBe('200px');
      expect((rail().firstElementChild as HTMLElement).style.top).toBe('224px');

      pageTop = -400;
      await scroll();
      expect(tabs().style.top).toBe('73px');
      expect((rail().firstElementChild as HTMLElement).style.top).toBe('97px');
    } finally {
      header.remove();
    }
  });
});
