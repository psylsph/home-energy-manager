import { BrowserRouter, Routes, Route, NavLink } from 'react-router-dom';
import { useWebSocket } from './hooks/useWebSocket';
import { useInverterStore } from './store/useInverterStore';
import StatusPage from './pages/StatusPage';
import ControlPage from './pages/ControlPage';
import SettingsPage from './pages/SettingsPage';
import HistoryPage from './pages/HistoryPage';

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

function SettingsIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M4 6h16M4 12h16M4 18h16" />
    </svg>
  );
}

function Layout() {
  useWebSocket();

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
          <Route path="/" element={<StatusPage />} />
          <Route path="/history" element={<HistoryPage />} />
          <Route path="/control" element={<ControlPage />} />
          <Route path="/settings" element={<SettingsPage />} />
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
              `flex flex-col items-center gap-0.5 px-4 py-2 rounded-xl text-xs font-medium transition-colors ${
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
