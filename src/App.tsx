import MetersPage from './pages/MetersPage';
import { BrowserRouter, Routes, Route, NavLink } from 'react-router-dom';
import { useWebSocket } from './hooks/useWebSocket';
import { useInverterStore } from './store/useInverterStore';
import StatusPage from './pages/StatusPage';
import BatteryPage from './pages/BatteryPage';
import ControlPage from './pages/ControlPage';
import SettingsPage from './pages/SettingsPage';
import HistoryPage from './pages/HistoryPage';
import LogsPage from './pages/LogsPage';
import SolarPage from './pages/SolarPage';
import InverterPage from './pages/InverterPage';
import ErrorBoundary from './components/ErrorBoundary';

function ConnectionIndicator() {
  const { connectionState, connectedHost } = useInverterStore();
  const colors: Record<string, string> = {
    connected: 'bg-green-500',
    reconnecting: 'bg-yellow-500 animate-pulse',
    disconnected: 'bg-gray-500',
  };
  return (
    <div className="flex items-center gap-2 text-text-secondary text-xs">
      <div className={`w-2 h-2 rounded-full ${colors[connectionState] || 'bg-gray-500'}`} />
      <span className="capitalize">
        {connectionState === 'connected' ? `Connected${connectedHost ? ` · ${connectedHost}` : ''}` : connectionState}
      </span>
    </div>
  );
}

const NAV_ITEMS = [
  { to: '/', label: 'Status', icon: StatusIcon },
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
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M3 12l2-2m0 0l7-7 7 7M5 10v10a1 1 0 001 1h3m10-11l2 2m-2-2v10a1 1 0 01-1 1h-3m-4 0a1 1 0 01-1-1v-4a1 1 0 011-1h2a1 1 0 011 1v4a1 1 0 01-1 1" />
    </svg>
  );
}

function HistoryIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M12 8v4l3 3m6-3a9 9 0 11-18 0 9 9 0 0118 0z" />
    </svg>
  );
}

function ControlIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 002.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 001.066 2.573c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 00-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 00-2.573 1.066c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 00-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 00-1.066-2.573c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 001.066-2.573c-.94-1.543.826-3.31 2.37-2.37.996.608 2.296.07 2.572-1.065z" />
      <path strokeLinecap="round" strokeLinejoin="round" d="M15 12a3 3 0 11-6 0 3 3 0 016 0z" />
    </svg>
  );
}

function BatteryIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <rect x="2" y="7" width="18" height="10" rx="2" />
      <path strokeLinecap="round" d="M22 11v2" />
      <rect x="4" y="9" width="14" height="6" rx="1" fill="currentColor" opacity="0.3" />
    </svg>
  );
}

function SettingsIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M4 6h16M4 12h16M4 18h16" />
    </svg>
  );
}

function SolarIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <circle cx="12" cy="12" r="3" />
      <path strokeLinecap="round" d="M12 2v2m0 16v2m-9-9H1m20 0h-2M4.93 4.93l1.41 1.41m11.32 11.32l1.41 1.41M4.93 19.07l1.41-1.41m11.32-11.32l1.41-1.41" />
    </svg>
  );
}

function InverterIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <rect x="4" y="4" width="16" height="16" rx="2" />
      <path strokeLinecap="round" d="M8 8h8M8 12h8M8 16h5" />
    </svg>
  );
}

function MeterIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8l-6-6z" />
      <path strokeLinecap="round" strokeLinejoin="round" d="M14 2v6h6M16 13H8M16 17H8M10 9H8" />
      <circle cx="11" cy="15" r="1" fill="currentColor" />
    </svg>
  );
}

function LogsIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M6.75 7.5l3 2.25-3 2.25m4.5 0h3m-9 8.25h13.5A2.25 2.25 0 0021 18V6a2.25 2.25 0 00-2.25-2.25H5.25A2.25 2.25 0 003 6v12a2.25 2.25 0 002.25 2.25z" />
    </svg>
  );
}

function Layout() {
  useWebSocket();
  const { developerMode } = useInverterStore();

  return (
    <div className="min-h-screen bg-bg-base flex flex-col">
      {/* Header */}
      <header className="bg-bg-surface/80 backdrop-blur-md border-b border-white/5 px-6 py-3 flex items-center justify-between sticky top-0 z-30">
        <div className="flex items-center gap-4">
          <h1 className="text-base font-bold text-text-primary tracking-tight">
            GivEnergy
          </h1>
          <ConnectionIndicator />
        </div>
      </header>

      {/* Content */}
      <main className="flex-1 overflow-auto px-4 py-6 md:px-6 md:py-8">
        <Routes>
          <Route path="/" element={<ErrorBoundary><StatusPage /></ErrorBoundary>} />
          <Route path="/battery" element={<ErrorBoundary><BatteryPage /></ErrorBoundary>} />
          <Route path="/history" element={<ErrorBoundary><HistoryPage /></ErrorBoundary>} />
          <Route path="/control" element={<ErrorBoundary><ControlPage /></ErrorBoundary>} />
          <Route path="/settings" element={<ErrorBoundary><SettingsPage /></ErrorBoundary>} />
          <Route path="/solar" element={<ErrorBoundary><SolarPage /></ErrorBoundary>} />
          <Route path="/meters" element={<ErrorBoundary><MetersPage /></ErrorBoundary>} />
          <Route path="/inverter" element={<ErrorBoundary><InverterPage /></ErrorBoundary>} />
          {developerMode && <Route path="/logs" element={<ErrorBoundary><LogsPage /></ErrorBoundary>} />}
        </Routes>
      </main>

      {/* Bottom navigation */}
      <nav className="sticky bottom-0 bg-bg-surface/90 backdrop-blur-md border-t border-white/5 px-2 py-1 flex justify-around z-30">
        {NAV_ITEMS.map(({ to, label, icon: Icon }) => (
          <NavLink
            key={to}
            to={to}
            end={to === '/'}
            className={({ isActive }) =>
              `flex flex-col items-center gap-0.5 px-3 py-2 rounded-xl text-xs font-medium transition-colors ${
                isActive
                  ? 'text-flow-active'
                  : 'text-text-secondary hover:text-text-primary'
              }`
            }
          >
            <Icon />
            <span>{label}</span>
          </NavLink>
        ))}
        {developerMode && (
          <NavLink
            to="/logs"
            className={({ isActive }) =>
              `flex flex-col items-center gap-0.5 px-3 py-2 rounded-xl text-xs font-medium transition-colors ${
                isActive
                  ? 'text-flow-active'
                  : 'text-text-secondary hover:text-text-primary'
              }`
            }
          >
            <LogsIcon />
            <span>Logs</span>
          </NavLink>
        )}
      </nav>
    </div>
  );
}

export default function App() {
  return (
    <BrowserRouter>
      <Layout />
    </BrowserRouter>
  );
}
