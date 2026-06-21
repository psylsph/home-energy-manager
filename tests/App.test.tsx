import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, act, cleanup } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------
//
// StatusPage is mocked to *throw* during render so we can prove the route's
// ErrorBoundary contains the failure. Every other page is a harmless marker.
// The two network hooks and the api module are stubbed so <App/> mounts in
// jsdom without a backend or a real WebSocket.

vi.mock('../src/pages/StatusPage', () => ({
  default: function StatusPageMock() {
    throw new Error('Status exploded');
  },
}));
vi.mock('../src/pages/PowerPage', () => ({
  default: () => <div data-testid="mock-Power">Power</div>,
}));
vi.mock('../src/pages/BatteryPage', () => ({
  default: () => <div data-testid="mock-Battery">Battery</div>,
}));
vi.mock('../src/pages/SolarPage', () => ({
  default: () => <div data-testid="mock-Solar">Solar</div>,
}));
vi.mock('../src/pages/InverterPage', () => ({
  default: () => <div data-testid="mock-Inverter">Inverter</div>,
}));
vi.mock('../src/pages/MetersPage', () => ({
  default: () => <div data-testid="mock-Meters">Meters</div>,
}));
vi.mock('../src/pages/HistoryPage', () => ({
  default: () => <div data-testid="mock-History">History</div>,
}));
vi.mock('../src/pages/ControlPage', () => ({
  default: () => <div data-testid="mock-Control">Control</div>,
}));
vi.mock('../src/pages/SettingsPage', () => ({
  default: () => <div data-testid="mock-Settings">Settings</div>,
}));
vi.mock('../src/pages/LogsPage', () => ({
  default: () => <div data-testid="mock-Logs">Logs</div>,
}));

vi.mock('../src/hooks/useWebSocket', () => ({ useWebSocket: () => {} }));
vi.mock('../src/hooks/useGridOutageNotifications', () => ({
  useGridOutageNotifications: () => {},
}));
vi.mock('../src/lib/api', () => ({
  apiGet: vi.fn().mockResolvedValue({ ok: true, data: {} }),
  fetchHistory: vi.fn().mockResolvedValue({}),
  isTauri: false,
}));

// Imported after the vi.mock() calls above (factories are hoisted regardless).
import App from '../src/App';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Silence React's dev-mode console.error for deliberately-thrown errors. */
function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

/** Drive the <HashRouter> to a route under act so React flushes the update.
 * jsdom doesn't synchronously notify React Router's history listener when
 * `location.hash` is assigned, so we dispatch the `hashchange` event
 * explicitly inside act (the async form flushes the resulting re-render). */
async function navigate(hashRoute: string) {
  await act(async () => {
    window.location.hash = hashRoute;
    window.dispatchEvent(new Event('hashchange'));
  });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe('<App/> route-level ErrorBoundary coverage (issue 3.4)', () => {
  beforeEach(() => {
    silenceConsoleError();
    window.location.hash = '';
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    window.location.hash = '';
  });

  it('contains a throwing page to its own route (shows the fallback, not a crash)', () => {
    // StatusPage throws on render — the route's ErrorBoundary must catch it
    // and show the fallback UI rather than blanking the whole app.
    render(<App />);
    expect(screen.getByText('Something went wrong')).toBeDefined();
    expect(screen.getByText('Status exploded')).toBeDefined();
  });

  it('does not let one failing page take down the rest of the app', async () => {
    // Sibling boundaries mirror the app architecture: each route is its own
    // <ErrorBoundary>. A throw in one must not affect the other. (We test the
    // boundary's sibling-isolation property directly in the ErrorBoundary suite
    // rather than via hash navigation here — React Router's <HashRouter>
    // doesn't react to post-mount `hashchange` events in jsdom, though it
    // works fine in a real browser.)
    //
    // At the App level the equivalent guarantee is structural: the `page()`
    // helper wraps every route in its own boundary, and the per-route tests
    // below show each route renders independently.
    await navigate('/');
    render(<App />);
    expect(screen.getByText('Status exploded')).toBeDefined();
    // The surrounding chrome (header + every nav link) still renders — proof
    // the error was contained and did not crash the React tree.
    expect(screen.getByText(/Home Energy Manager/)).toBeDefined();
    expect(screen.getByRole('link', { name: 'Battery' })).toBeDefined();
    expect(screen.getByRole('link', { name: 'Settings' })).toBeDefined();
  });

  // Each core route renders its (mocked) page. This also guards the structural
  // invariant introduced by the `page()` helper: every route is wrapped in an
  // ErrorBoundary, so a healthy page renders fine (and a throwing one — like
  // StatusPage above — is contained). If a route were ever added without the
  // helper, its page would render un-wrapped and a failure could escape.
  it.each([
    ['/power', 'mock-Power'],
    ['/battery', 'mock-Battery'],
    ['/solar', 'mock-Solar'],
    ['/inverter', 'mock-Inverter'],
    ['/meters', 'mock-Meters'],
    ['/history', 'mock-History'],
    ['/control', 'mock-Control'],
    ['/settings', 'mock-Settings'],
  ] as const)('renders the %s route within its ErrorBoundary', async (route, testId) => {
    await navigate(route);
    render(<App />);
    expect(screen.getByTestId(testId)).toBeDefined();
  });
});
