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
    <div className="flex items-center gap-2 text-text-secondary text-sm">
      <div className={`w-2 h-2 rounded-full ${colors[connectionState] || 'bg-gray-500'}`} />
      <span className="capitalize">
        {connectionState === 'connected' ? `Connected ${connectedHost || ''}` : connectionState}
      </span>
    </div>
  );
}

function Layout() {
  useWebSocket();

  const navLinkClass = ({ isActive }: { isActive: boolean }) =>
    `px-4 py-2 text-sm font-medium rounded-lg transition-colors ${
      isActive
        ? 'bg-bg-elevated text-text-primary'
        : 'text-text-secondary hover:text-text-primary'
    }`;

  return (
    <div className="min-h-screen bg-bg-base flex flex-col">
      {/* Header */}
      <header className="bg-bg-surface border-b border-bg-elevated px-4 py-3 flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h1 className="text-lg font-semibold text-text-primary font-sans">
            GivEnergy Local
          </h1>
          <ConnectionIndicator />
        </div>
      </header>

      {/* Content */}
      <main className="flex-1 overflow-auto p-4 md:p-6">
        <Routes>
          <Route path="/" element={<StatusPage />} />
          <Route path="/history" element={<HistoryPage />} />
          <Route path="/control" element={<ControlPage />} />
          <Route path="/settings" element={<SettingsPage />} />
        </Routes>
      </main>

      {/* Bottom navigation */}
      <nav className="bg-bg-surface border-t border-bg-elevated px-4 py-2 flex justify-center gap-2">
        <NavLink to="/" className={navLinkClass} end>
          Status
        </NavLink>
        <NavLink to="/history" className={navLinkClass}>
          History
        </NavLink>
        <NavLink to="/control" className={navLinkClass}>
          Control
        </NavLink>
        <NavLink to="/settings" className={navLinkClass}>
          Settings
        </NavLink>
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
