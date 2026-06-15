import { useEffect } from 'react';
import MetersPage from './pages/MetersPage';
import { HashRouter, Routes, Route, NavLink, Navigate } from 'react-router-dom';
import { useWebSocket } from './hooks/useWebSocket';
import { useGridOutageNotifications } from './hooks/useGridOutageNotifications';
import type { PollSettings } from './lib/types';
import { apiGet } from './lib/api';
import { formatPercent } from './lib/format';
import { gridFaultReason, gridFaultTitle, hasGridFault } from './lib/gridFault';
import { useInverterStore } from './store/useInverterStore';
import StatusPage from './pages/StatusPage';
import BatteryPage from './pages/BatteryPage';
import ControlPage from './pages/ControlPage';
import SettingsPage from './pages/SettingsPage';
import HistoryPage from './pages/HistoryPage';
import LogsPage from './pages/LogsPage';
import SolarPage from './pages/SolarPage';
import InverterPage from './pages/InverterPage';
import PowerPage from './pages/PowerPage';
import ErrorBoundary from './components/ErrorBoundary';

function ThemeToggle() {
  const { themeMode, setThemeMode } = useInverterStore();
  const isLight = themeMode === 'light';

  return (
    <button
      type="button"
      onClick={() => setThemeMode(isLight ? 'dark' : 'light')}
      className="flex items-center gap-2 rounded-full bg-bg-elevated px-2 py-1 text-xs text-text-secondary transition hover:text-text-primary focus:outline-none focus:ring-2 focus:ring-flow-active/60"
      aria-label={`Switch to ${isLight ? 'dark' : 'light'} mode`}
      title={`Switch to ${isLight ? 'dark' : 'light'} mode`}
    >
      <span aria-hidden="true">{isLight ? '☀️' : '🌙'}</span>
      <span className="hidden sm:inline">{isLight ? 'Light' : 'Dark'}</span>
    </button>
  );
}

function GridFaultBanner() {
  const snapshot = useInverterStore((state) => state.snapshot);
  if (!snapshot || !hasGridFault(snapshot)) return null;

  return (
    <div className="bg-red-950/90 border-b border-red-500/40 px-4 py-2 text-red-100 text-sm">
      <div className="max-w-4xl mx-auto flex items-center gap-2">
        <span aria-hidden="true">⚠️</span>
        <strong>{gridFaultTitle(snapshot)}</strong>
        <span className="text-red-100/85">
          {gridFaultReason(snapshot)} · Battery {formatPercent(snapshot.soc)}
        </span>
      </div>
    </div>
  );
}

function ConnectionIndicator() {
  const { connectionState, connectedHost, snapshot } = useInverterStore();
  const colors: Record<string, string> = {
    connected: 'bg-green-500',
    reconnecting: 'bg-yellow-500 animate-pulse',
    disconnected: 'bg-gray-500',
  };
  const inverterTime = snapshot?.inverter_time;
  return (
    <div className="flex items-start gap-2 text-text-secondary text-xs">
      <div className={`mt-1.5 w-2 h-2 rounded-full ${colors[connectionState] || 'bg-gray-500'}`} />
      <div className="flex flex-col items-start leading-tight">
        <span className="capitalize">
          {connectionState === 'connected' ? `Connected${connectedHost ? ` · ${connectedHost}` : ''}` : connectionState}
        </span>
        {connectionState === 'connected' && inverterTime ? (
          <span className="font-mono normal-case text-[10px] text-text-secondary/80">
            Inverter Time: {inverterTime}
          </span>
        ) : null}
      </div>
    </div>
  );
}

const NAV_ITEMS = [
  { to: '/', label: 'Status', icon: StatusIcon },
  { to: '/power', label: 'Power', icon: PowerIcon },
  { to: '/battery', label: 'Battery', icon: BatteryIcon },
  { to: '/inverter', label: 'Inverter', icon: InverterIcon },
  { to: '/solar', label: 'Solar', icon: SolarIcon },
  { to: '/meters', label: 'Meters', icon: MeterIcon },
  { to: '/history', label: 'History', icon: HistoryIcon },
  { to: '/control', label: 'Control', icon: ControlIcon },
  { to: '/settings', label: 'Settings', icon: SettingsIcon },
] as const;

function StatusIcon() {
  return (
    <svg className="w-7 h-7 sm:w-5 sm:h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M3 12l2-2m0 0l7-7 7 7M5 10v10a1 1 0 001 1h3m10-11l2 2m-2-2v10a1 1 0 01-1 1h-3m-4 0a1 1 0 01-1-1v-4a1 1 0 011-1h2a1 1 0 011 1v4a1 1 0 01-1 1" />
    </svg>
  );
}

function HistoryIcon() {
  return (
    <svg className="w-7 h-7 sm:w-5 sm:h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M12 8v4l3 3m6-3a9 9 0 11-18 0 9 9 0 0118 0z" />
    </svg>
  );
}

function PowerIcon() {
  return (
    <svg className="w-4 h-4 sm:w-5 sm:h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M13 2L4 14h7l-1 8 10-13h-7V2z" />
    </svg>
  );
}

function ControlIcon() {
  return (
    <svg className="w-7 h-7 sm:w-5 sm:h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 002.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 001.066 2.573c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 00-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 00-2.573 1.066c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 00-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 00-1.066-2.573c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 001.066-2.573c-.94-1.543.826-3.31 2.37-2.37.996.608 2.296.07 2.572-1.065z" />
      <path strokeLinecap="round" strokeLinejoin="round" d="M15 12a3 3 0 11-6 0 3 3 0 016 0z" />
    </svg>
  );
}

function BatteryIcon() {
  return (
    <svg className="w-7 h-7 sm:w-5 sm:h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <rect x="2" y="7" width="18" height="10" rx="2" />
      <path strokeLinecap="round" d="M22 11v2" />
      <rect x="4" y="9" width="14" height="6" rx="1" fill="currentColor" opacity="0.3" />
    </svg>
  );
}

function SettingsIcon() {
  return (
    <svg className="w-7 h-7 sm:w-5 sm:h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M4 6h16M4 12h16M4 18h16" />
    </svg>
  );
}

function SolarIcon() {
  return (
    <svg className="w-7 h-7 sm:w-5 sm:h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <circle cx="12" cy="12" r="3" />
      <path strokeLinecap="round" d="M12 2v2m0 16v2m-9-9H1m20 0h-2M4.93 4.93l1.41 1.41m11.32 11.32l1.41 1.41M4.93 19.07l1.41-1.41m11.32-11.32l1.41-1.41" />
    </svg>
  );
}

function InverterIcon() {
  return (
    <svg className="w-7 h-7 sm:w-5 sm:h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <rect x="4" y="4" width="16" height="16" rx="2" />
      <path strokeLinecap="round" d="M8 8h8M8 12h8M8 16h5" />
    </svg>
  );
}

function MeterIcon() {
  return (
    <svg className="w-7 h-7 sm:w-5 sm:h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8l-6-6z" />
      <path strokeLinecap="round" strokeLinejoin="round" d="M14 2v6h6M16 13H8M16 17H8M10 9H8" />
      <circle cx="11" cy="15" r="1" fill="currentColor" />
    </svg>
  );
}

function LogsIcon() {
  return (
    <svg className="w-7 h-7 sm:w-5 sm:h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M6.75 7.5l3 2.25-3 2.25m4.5 0h3m-9 8.25h13.5A2.25 2.25 0 0021 18V6a2.25 2.25 0 00-2.25-2.25H5.25A2.25 2.25 0 003 6v12a2.25 2.25 0 002.25 2.25z" />
    </svg>
  );
}

function Layout() {
  useWebSocket();
  useGridOutageNotifications();
  const { developerMode, themeMode, hiddenPanels, setHiddenPanels, setEvcHost } = useInverterStore();

  // Load hidden panels from settings on mount
  useEffect(() => {
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: PollSettings }>('/api/settings');
        if (res.ok && res.data.hidden_panels) {
          setHiddenPanels(res.data.hidden_panels);
        }
        if (res.ok && res.data.evc_host) {
          setEvcHost(res.data.evc_host);
        }
      } catch { /* use defaults */ }
    })();
  }, [setHiddenPanels, setEvcHost]);

  useEffect(() => {
    document.documentElement.dataset.theme = themeMode;
  }, [themeMode]);

  const visibleItems = NAV_ITEMS.filter(item => {
    const key = item.to.replace(/^\//, '');
    return !key || !hiddenPanels.includes(key);
  });

  // Helper: return the panel element unless it's hidden, then redirect to /
  function panelRoute(path: string, element: React.ReactNode) {
    const key = path.replace(/^\//, '');
    if (hiddenPanels.includes(key)) {
      return <Route path={path} element={<Navigate to="/" replace />} />;
    }
    return <Route path={path} element={element} />;
  }

  return (
    <div className="min-h-screen bg-bg-base flex flex-col">
      {/* Header */}
      <header className="bg-bg-surface/80 backdrop-blur-md border-b border-white/5 px-6 pt-safe py-3 flex items-center justify-between sticky top-0 z-30">
        <div>
          <h1 className="text-base font-bold text-text-primary tracking-tight">
            Home Energy Manager  <span className="text-text-secondary font-mono text-xs font-normal">v{__APP_VERSION__}</span>
          </h1>
          <p className="hidden sm:block text-xs text-text-secondary">
            For GivEnergy Solar and Battery Systems
          </p>
        </div>
        <div className="flex items-center gap-3">
          <ThemeToggle />
          <ConnectionIndicator />
        </div>
      </header>

      <GridFaultBanner />

      {/* Content */}
      <main className="flex-1 overflow-auto px-4 py-6 md:px-6 md:py-8 pb-safe">
        <Routes>
          <Route path="/" element={<ErrorBoundary><StatusPage /></ErrorBoundary>} />
          {panelRoute('/power', <ErrorBoundary><PowerPage /></ErrorBoundary>)}
          {panelRoute('/battery', <ErrorBoundary><BatteryPage /></ErrorBoundary>)}
          {panelRoute('/history', <ErrorBoundary><HistoryPage /></ErrorBoundary>)}
          <Route path="/control" element={<ErrorBoundary><ControlPage /></ErrorBoundary>} />
          <Route path="/settings" element={<ErrorBoundary><SettingsPage /></ErrorBoundary>} />
          {panelRoute('/solar', <ErrorBoundary><SolarPage /></ErrorBoundary>)}
          {panelRoute('/meters', <ErrorBoundary><MetersPage /></ErrorBoundary>)}
          {panelRoute('/inverter', <ErrorBoundary><InverterPage /></ErrorBoundary>)}
          {developerMode && <Route path="/logs" element={<ErrorBoundary><LogsPage /></ErrorBoundary>} />}
        </Routes>
      </main>

      {/* Bottom navigation
          Each link uses flex-1 + min-w-0 so the row shares the full width
          equally and never overflows, no matter how narrow the viewport is.
          - <sm:  icon-only, 16px icon, tight vertical padding
          - sm+:  icon-only, 20px icon, slightly larger padding
          - md+:  icon + text label
          A title/aria-label keeps icon-only modes discoverable. */}
      <nav className="sticky bottom-0 bg-bg-surface/90 backdrop-blur-md border-t border-white/5 px-0 pt-1 pb-safe flex items-stretch z-30">
        {visibleItems.map(({ to, label, icon: Icon }) => (
          <NavLink
            key={to}
            to={to}
            end={to === '/'}
            title={label}
            aria-label={label}
            className={({ isActive }) =>
              `min-w-0 flex-1 flex flex-col items-center justify-center
               gap-0 py-1.5 sm:py-2
               rounded-none
               text-[10px] sm:text-xs font-medium transition-colors
               ${
                 isActive
                  ? 'text-flow-active'
                  : 'text-text-secondary hover:text-text-primary'
              }`
            }
          >
            <Icon />
            <span className="hidden md:inline">{label}</span>
          </NavLink>
        ))}
        {developerMode && (
          <NavLink
            to="/logs"
            title="Logs"
            aria-label="Logs"
            className={({ isActive }) =>
              `min-w-0 flex-1 flex flex-col items-center justify-center
               gap-0 py-1.5 sm:py-2
               rounded-none
               text-[10px] sm:text-xs font-medium transition-colors
               ${
                isActive
                  ? 'text-flow-active'
                  : 'text-text-secondary hover:text-text-primary'
              }`
            }
          >
            <LogsIcon />
            <span className="hidden md:inline">Logs</span>
          </NavLink>
        )}
      </nav>
    </div>
  );
}

export default function App() {
  return (
    <HashRouter>
      <Layout />
    </HashRouter>
  );
}
